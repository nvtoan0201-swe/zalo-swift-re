//! Native SQLite behind the official client's data access layer (DAL). Port of `SQLiteHost.swift`.
//!
//! Zalo PC 26.10 keeps every database (messages, conversations, contacts, search, …) in SQLite, served by a
//! `utility-process-sqlite` Electron utility process that the renderer reaches through a MessagePort channel. Here
//! `sqlite-host.js` plays that process inside each page: it answers the official channel protocol and executes every
//! statement through the `$znode.sqliteHost.*` bridge members (`install`). This reproduces what the official connection
//! class does with node-sqlite3 (`main-dist/utility-process-sqlite.js`):
//!
//! - one file per `fullname` under `<userData>/sqlite/_production/<fullname>.db`, WAL, `synchronous=NORMAL`,
//!   `busy_timeout=5000`;
//! - `run` resolves `{lastID, changes}` and treats a failed UNIQUE constraint as success; `runs` binds every parameter
//!   row of one prepared statement inside a transaction; `serialize` runs a list of `run` / `runs` in a transaction;
//!   `all` / `get` / `optimized-all` return rows as objects;
//! - errors carry node-sqlite3's shape (`errno`, `code`), sent as `"<code>:<errno>:<detail>"` for the script to parse;
//! - the schema-version handshake (`PRAGMA user_version` vs the version the renderer asks for) is coordinated here so
//!   two pages opening the same database cannot both run the renderer's migration.
//!
//! The official binding is SQLCipher, but a database is only encrypted once the official encryption job ran on it,
//! which never starts here (`DB_ENCRYPTION/CHECKSTT` → not started): files stay plain SQLite. Each database has its own
//! thread, so its statements run one after the other in arrival order (the Swift serial queue).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::channel::oneshot;
use olaz_core::value::{Args, BV};
use olaz_host::bridge::ZaloBridge;
use rusqlite::types::ValueRef;
use rusqlite::{Connection as Db, ffi};

/// A SQLite failure in node-sqlite3's terms.
#[derive(Debug, Clone, PartialEq)]
pub struct SqlError {
    pub errno: i32,
    pub code: String,
    pub detail: String,
}

impl SqlError {
    fn new(errno: i32, detail: impl Into<String>) -> Self {
        Self { errno, code: code_name(errno).to_string(), detail: detail.into() }
    }
    /// Wire form for the bridge (`sqlite-host.js` parses it back into `{errno, code, message}`).
    pub fn wire(&self) -> String {
        format!("{}:{}:{}", self.code, self.errno, self.detail)
    }
    fn is_unique_constraint(&self) -> bool {
        self.errno == ffi::SQLITE_CONSTRAINT && self.detail.contains("UNIQUE constraint failed")
    }
}

impl From<rusqlite::Error> for SqlError {
    fn from(e: rusqlite::Error) -> Self {
        match e {
            rusqlite::Error::SqliteFailure(f, message) => {
                let errno = f.extended_code & 0xff;
                let detail = message.unwrap_or_else(|| f.to_string());
                SqlError::new(if errno == 0 { ffi::SQLITE_ERROR } else { errno }, detail)
            }
            other => SqlError::new(ffi::SQLITE_ERROR, other.to_string()),
        }
    }
}

/// node-sqlite3's `code` strings for the primary result codes.
pub fn code_name(errno: i32) -> &'static str {
    match errno {
        1 => "SQLITE_ERROR",
        2 => "SQLITE_INTERNAL",
        3 => "SQLITE_PERM",
        4 => "SQLITE_ABORT",
        5 => "SQLITE_BUSY",
        6 => "SQLITE_LOCKED",
        7 => "SQLITE_NOMEM",
        8 => "SQLITE_READONLY",
        9 => "SQLITE_INTERRUPT",
        10 => "SQLITE_IOERR",
        11 => "SQLITE_CORRUPT",
        12 => "SQLITE_NOTFOUND",
        13 => "SQLITE_FULL",
        14 => "SQLITE_CANTOPEN",
        15 => "SQLITE_PROTOCOL",
        16 => "SQLITE_EMPTY",
        17 => "SQLITE_SCHEMA",
        18 => "SQLITE_TOOBIG",
        19 => "SQLITE_CONSTRAINT",
        20 => "SQLITE_MISMATCH",
        21 => "SQLITE_MISUSE",
        22 => "SQLITE_NOLFS",
        23 => "SQLITE_AUTH",
        24 => "SQLITE_FORMAT",
        25 => "SQLITE_RANGE",
        26 => "SQLITE_NOTADB",
        27 => "SQLITE_NOTICE",
        28 => "SQLITE_WARNING",
        100 => "SQLITE_ROW",
        101 => "SQLITE_DONE",
        _ => "SQLITE_ERROR",
    }
}

/// One statement: SQL text and positional parameters.
#[derive(Debug, Clone, Default)]
pub struct Statement {
    pub query: String,
    pub params: Vec<BV>,
}

#[derive(Debug, Clone)]
pub enum SerializedItem {
    Run(Statement),
    Runs { query: String, rows: Vec<Vec<BV>> },
}

type Job = Box<dyn FnOnce(&mut Db) + Send>;

/// An open database: its thread runs the jobs in order.
struct Connection {
    jobs: std::sync::mpsc::Sender<Job>,
    sessional: AtomicBool,
}

impl Connection {
    fn open(path: &Path, name: &str, sessional: bool) -> Result<Arc<Self>, SqlError> {
        let db = Db::open(path)?;
        // The official connection: WAL, journal_size_limit, busy_timeout, synchronous NORMAL…
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000; PRAGMA journal_size_limit=67108864; PRAGMA temp_store=MEMORY;")?;
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name(format!("olaz-sqlite {}", name.chars().take(20).collect::<String>()))
            .spawn(move || {
                let mut db = db;
                // Ends (closing the database) when the connection is dropped and its queued jobs ran.
                while let Ok(job) = rx.recv() {
                    job(&mut db);
                }
            })
            .map_err(|e| SqlError::new(ffi::SQLITE_ERROR, e.to_string()))?;
        Ok(Arc::new(Self { jobs: tx, sessional: AtomicBool::new(sessional) }))
    }

    /// Runs `body` on the database's thread.
    async fn run<T: Send + 'static>(&self, body: impl FnOnce(&mut Db) -> Result<T, SqlError> + Send + 'static) -> Result<T, SqlError> {
        let (tx, rx) = oneshot::channel();
        self.jobs
            .send(Box::new(move |db| {
                let _ = tx.send(body(db));
            }))
            .map_err(|_| SqlError::new(ffi::SQLITE_MISUSE, "database is closed"))?;
        rx.await.unwrap_or_else(|_| Err(SqlError::new(ffi::SQLITE_MISUSE, "database is closed")))
    }
}

#[derive(Default)]
struct Inner {
    connections: HashMap<String, Arc<Connection>>,
    /// Schema-version handshakes in flight: later openers wait for them.
    migrations: HashMap<String, Vec<oneshot::Sender<()>>>,
}

pub struct SqliteHost {
    pub root: PathBuf,
    inner: Mutex<Inner>,
}

fn sanitized(fullname: &str) -> String {
    fullname.split('/').filter(|p| !p.is_empty() && *p != "." && *p != "..").collect::<Vec<_>>().join("/")
}

impl SqliteHost {
    /// `root`: the directory that holds the database files (`<userData>/sqlite/_production`).
    pub fn new(root: PathBuf) -> Arc<Self> {
        Arc::new(Self { root, inner: Mutex::new(Inner::default()) })
    }

    /// `<root>/<fullname>.db` (a fullname may hold path components: partitioned databases live in sub-directories).
    pub fn file_path(&self, fullname: &str) -> PathBuf {
        let mut path = self.root.clone();
        for part in fullname.split('/').filter(|p| !p.is_empty() && *p != "." && *p != "..") {
            path.push(part);
        }
        let mut s = path.into_os_string();
        s.push(".db");
        PathBuf::from(s)
    }

    fn connect(&self, name: &str, sessional: bool) -> Result<Arc<Connection>, SqlError> {
        if let Some(existing) = self.inner.lock().unwrap().connections.get(name) {
            if sessional {
                existing.sessional.store(true, Ordering::SeqCst);
            }
            return Ok(existing.clone());
        }
        let path = self.file_path(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SqlError::new(ffi::SQLITE_CANTOPEN, e.to_string()))?;
        }
        let connection = Connection::open(&path, name, sessional)?;
        let mut inner = self.inner.lock().unwrap();
        // Another opener raced us: keep theirs (ours closes when dropped).
        Ok(inner.connections.entry(name.to_string()).or_insert(connection).clone())
    }

    fn connection(&self, fullname: &str) -> Result<Arc<Connection>, SqlError> {
        let name = sanitized(fullname);
        if let Some(c) = self.inner.lock().unwrap().connections.get(&name) {
            return Ok(c.clone());
        }
        self.connect(&name, false)
    }

    /// Opens (creating) the database: `(user_version, migrate)`. `requested` None = whatever the file has. `migrate`:
    /// the file is at another version and this caller must run the renderer's migration (`NEED-SCHEMA-MIGRATION`),
    /// then call `migrated`. Other openers wait for that and get `migrate: false`.
    pub async fn open(&self, fullname: &str, requested: Option<i64>, sessional: bool) -> Result<(i64, bool), SqlError> {
        let name = sanitized(fullname);
        let connection = self.connect(&name, sessional)?;
        loop {
            let wait = {
                let mut inner = self.inner.lock().unwrap();
                match inner.migrations.get_mut(&name) {
                    Some(waiters) => {
                        let (tx, rx) = oneshot::channel();
                        waiters.push(tx);
                        Some(rx)
                    }
                    None => None,
                }
            };
            match wait {
                Some(rx) => {
                    let _ = rx.await;
                }
                None => break,
            }
        }
        let current = connection.run(|db| Ok(db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?)).await?;
        let wanted = requested.unwrap_or(current).max(1);
        if current == wanted {
            return Ok((current, false));
        }
        self.inner.lock().unwrap().migrations.insert(name, Vec::new());
        Ok((current, true))
    }

    /// The renderer finished (or failed) the schema migration started by `open`.
    pub fn migrated(&self, fullname: &str) {
        let waiters = self.inner.lock().unwrap().migrations.remove(&sanitized(fullname)).unwrap_or_default();
        for w in waiters {
            let _ = w.send(());
        }
    }

    /// Closes the database once its queued statements ran.
    pub async fn close(&self, fullname: &str) {
        let removed = self.inner.lock().unwrap().connections.remove(&sanitized(fullname));
        if let Some(c) = removed {
            // A last job marks the point after which the thread only drops the database.
            let _ = c.run(|_| Ok(())).await;
        }
    }

    pub async fn close_all(&self) {
        let names: Vec<String> = self.inner.lock().unwrap().connections.keys().cloned().collect();
        for n in names {
            self.close(&n).await;
        }
    }

    pub async fn close_sessional(&self) {
        let names: Vec<String> =
            self.inner.lock().unwrap().connections.iter().filter(|(_, c)| c.sessional.load(Ordering::SeqCst)).map(|(n, _)| n.clone()).collect();
        for n in names {
            self.close(&n).await;
        }
    }

    /// Closes and removes the database with its WAL / shm files.
    pub async fn delete(&self, fullname: &str) {
        self.close(fullname).await;
        let path = self.file_path(fullname);
        for suffix in ["", "-shm", "-wal"] {
            let mut p = path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(p));
        }
    }

    /// Official `DAL_UTILITY/GET_PARTITION_KEYS`: `[""]` when `<fullname>.db` exists, else the names of the `.db` files
    /// in every sub-directory of `<root>/<fullname>/`.
    pub fn partition_keys(&self, fullname: &str) -> Vec<String> {
        let file = self.file_path(fullname);
        if file.is_file() {
            return vec![String::new()];
        }
        let base = file.with_extension("");
        let mut keys = Vec::new();
        let Ok(groups) = std::fs::read_dir(&base) else { return keys };
        for group in groups.flatten().filter(|g| g.path().is_dir()) {
            let Ok(files) = std::fs::read_dir(group.path()) else { continue };
            for f in files.flatten() {
                let name = f.file_name().to_string_lossy().into_owned();
                if let Some(stem) = name.strip_suffix(".db") {
                    keys.push(stem.to_string());
                }
            }
        }
        keys
    }

    // MARK: Statements (the official `libMethod`s)

    /// `run`: `{lastID, changes}`; a failed UNIQUE constraint still resolves (official behaviour).
    pub async fn run(&self, fullname: &str, statement: Statement) -> Result<(i64, i64), SqlError> {
        self.connection(fullname)?
            .run(move |db| {
                match execute(db, &statement) {
                    Err(e) if e.is_unique_constraint() => {}
                    other => other?,
                }
                Ok((db.last_insert_rowid(), db.changes() as i64))
            })
            .await
    }

    /// `runs`: one prepared statement bound with every row, in a transaction.
    pub async fn runs(&self, fullname: &str, query: String, rows: Vec<Vec<BV>>) -> Result<(), SqlError> {
        self.connection(fullname)?.run(move |db| transaction(db, |db| run_rows(db, &query, &rows))).await
    }

    /// `serialize`: a list of `run` / `runs` in one transaction.
    pub async fn serialize(&self, fullname: &str, items: Vec<SerializedItem>) -> Result<(), SqlError> {
        self.connection(fullname)?
            .run(move |db| {
                transaction(db, |db| {
                    for item in &items {
                        match item {
                            SerializedItem::Run(s) => execute(db, s)?,
                            SerializedItem::Runs { query, rows } => run_rows(db, query, rows)?,
                        }
                    }
                    Ok(())
                })
            })
            .await
    }

    pub async fn all(&self, fullname: &str, statement: Statement) -> Result<Vec<BV>, SqlError> {
        self.connection(fullname)?.run(move |db| select(db, &statement.query, &statement.params, None)).await
    }

    pub async fn get(&self, fullname: &str, statement: Statement) -> Result<Option<BV>, SqlError> {
        self.connection(fullname)?.run(move |db| Ok(select(db, &statement.query, &statement.params, Some(1))?.into_iter().next())).await
    }

    /// `optimized-all`: every query bound with each of its parameter rows; all rows concatenated.
    pub async fn optimized_all(&self, fullname: &str, queries: Vec<String>, params: Vec<Vec<Vec<BV>>>) -> Result<Vec<BV>, SqlError> {
        self.connection(fullname)?
            .run(move |db| {
                let mut out = Vec::new();
                for (i, query) in queries.iter().enumerate() {
                    for row in params.get(i).map(Vec::as_slice).unwrap_or(&[]) {
                        out.extend(select(db, query, row, None)?);
                    }
                }
                Ok(out)
            })
            .await
    }
}

// MARK: SQLite plumbing (always on the database's thread)

fn transaction(db: &mut Db, body: impl FnOnce(&mut Db) -> Result<(), SqlError>) -> Result<(), SqlError> {
    db.execute_batch("BEGIN TRANSACTION")?;
    match body(db) {
        Ok(()) => Ok(db.execute_batch("COMMIT")?),
        Err(e) => {
            let _ = db.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Binds JS values the way node-sqlite3 does: numbers as INTEGER / REAL, booleans as 0 / 1, strings as TEXT, bytes as
/// BLOB, null / undefined / anything else as NULL. Missing parameters bind NULL.
fn bind(stmt: &mut rusqlite::Statement<'_>, params: &[BV]) -> Result<(), SqlError> {
    use rusqlite::types::Value;
    for index in 0..stmt.parameter_count() {
        let value = match params.get(index) {
            Some(BV::Bool(b)) => Value::Integer(*b as i64),
            Some(BV::Number(n)) => match (n.as_i64(), n.as_f64()) {
                (Some(i), _) => Value::Integer(i),
                (None, Some(f)) if f.round() == f && f.abs() < 9.0e15 => Value::Integer(f as i64),
                (None, Some(f)) => Value::Real(f),
                _ => Value::Null,
            },
            Some(BV::String(s)) => Value::Text(s.clone()),
            Some(BV::Bytes(b)) | Some(BV::ArrayBuffer(b)) => Value::Blob(b.clone()),
            _ => Value::Null,
        };
        stmt.raw_bind_parameter(index + 1, value)?;
    }
    Ok(())
}

/// Steps a statement to its end (rows, if any, are discarded).
fn step_to_end(stmt: &mut rusqlite::Statement<'_>) -> Result<(), SqlError> {
    let mut rows = stmt.raw_query();
    while rows.next()?.is_some() {}
    Ok(())
}

/// With parameters a single statement; without, every statement of the text (pragmas, DDL).
fn execute(db: &mut Db, statement: &Statement) -> Result<(), SqlError> {
    if statement.params.is_empty() {
        let mut batch = rusqlite::Batch::new(db, &statement.query);
        while let Some(mut stmt) = batch.next()? {
            step_to_end(&mut stmt)?;
        }
        return Ok(());
    }
    let mut stmt = db.prepare(&statement.query)?;
    bind(&mut stmt, &statement.params)?;
    step_to_end(&mut stmt)
}

fn run_rows(db: &mut Db, query: &str, rows: &[Vec<BV>]) -> Result<(), SqlError> {
    let mut stmt = db.prepare(query)?;
    for row in rows {
        stmt.clear_bindings();
        bind(&mut stmt, row)?;
        step_to_end(&mut stmt)?;
    }
    Ok(())
}

fn select(db: &mut Db, query: &str, params: &[BV], limit: Option<usize>) -> Result<Vec<BV>, SqlError> {
    let mut stmt = db.prepare(query)?;
    bind(&mut stmt, params)?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.raw_query();
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut object = BTreeMap::new();
        for (i, name) in names.iter().enumerate() {
            let value = match row.get_ref(i)? {
                ValueRef::Null => BV::Null,
                ValueRef::Integer(n) => BV::int(n),
                ValueRef::Real(f) => BV::float(f),
                ValueRef::Text(t) => BV::String(String::from_utf8_lossy(t).into_owned()),
                ValueRef::Blob(b) => BV::Bytes(b.to_vec()),
            };
            object.insert(name.clone(), value);
        }
        out.push(BV::Object(object));
        if limit.is_some_and(|l| out.len() >= l) {
            break;
        }
    }
    Ok(out)
}

// MARK: Bridge members (`$znode.sqliteHost.*`, called by sqlite-host.js)

/// Official statement objects: `{query: {query, params: <count>}, params: [...]}`; `query` may also be the SQL text.
fn query_of(v: Option<&BV>) -> String {
    match v {
        Some(BV::String(s)) => s.clone(),
        Some(o) => o.get("query").and_then(BV::as_str).unwrap_or("").to_string(),
        None => String::new(),
    }
}

fn list(v: Option<&BV>) -> Vec<BV> {
    v.and_then(BV::as_array).cloned().unwrap_or_default()
}

fn rows_of(v: Option<&BV>) -> Vec<Vec<BV>> {
    list(v).into_iter().map(|r| r.as_array().cloned().unwrap_or_default()).collect()
}

fn statement_of(o: &BV) -> Statement {
    Statement { query: query_of(o.get("query")), params: list(o.get("params")) }
}

impl SqliteHost {
    /// Registers the executor on a page's bridge.
    pub fn install(self: &Arc<Self>, bridge: &Rc<ZaloBridge>) {
        let me = self.clone();
        bridge.r#async("$znode.sqliteHost.open", move |a| {
            let me = me.clone();
            Box::pin(async move {
                let name = a.string(0).ok_or("open: missing fullname")?;
                let version = a.arg(1).and_then(BV::as_f64).map(|v| v as i64);
                let sessional = a.arg(2).and_then(BV::as_bool).unwrap_or(false);
                let (user_version, migrate) = me.open(&name, version, sessional).await.map_err(|e| e.wire())?;
                Ok(BV::obj([("userVersion", BV::int(user_version)), ("migrate", BV::Bool(migrate))]))
            })
        });
        let me = self.clone();
        bridge.async_now("$znode.sqliteHost.migrated", move |a| {
            if let Some(name) = a.string(0) {
                me.migrated(&name);
            }
            Ok(BV::Undefined)
        });
        let me = self.clone();
        bridge.r#async("$znode.sqliteHost.exec", move |a| {
            let me = me.clone();
            Box::pin(async move {
                let (Some(name), Some(method)) = (a.string(0), a.string(1)) else { return Err("exec: bad arguments".to_string()) };
                let st = a.arg(2).cloned().unwrap_or_else(|| BV::obj::<[(&str, BV); 0], &str>([]));
                let result = match method.as_str() {
                    "run" => me.run(&name, statement_of(&st)).await.map(|(id, changes)| BV::obj([("lastID", BV::int(id)), ("changes", BV::int(changes))])),
                    "runs" => me.runs(&name, query_of(st.get("query")), rows_of(st.get("params"))).await.map(|_| BV::Undefined),
                    "all" => me.all(&name, statement_of(&st)).await.map(BV::Array),
                    "get" => me.get(&name, statement_of(&st)).await.map(|r| r.unwrap_or(BV::Undefined)),
                    "optimized-all" => {
                        let queries = list(st.get("queries")).iter().map(|q| query_of(Some(q))).collect();
                        let params = list(st.get("params")).iter().map(|p| rows_of(Some(p))).collect();
                        me.optimized_all(&name, queries, params).await.map(BV::Array)
                    }
                    "serialize" => {
                        let items = list(st.get("data"))
                            .iter()
                            .map(|item| {
                                if item.get("method").and_then(BV::as_str) == Some("runs") {
                                    SerializedItem::Runs { query: query_of(item.get("query")), rows: rows_of(item.get("params")) }
                                } else {
                                    SerializedItem::Run(statement_of(item))
                                }
                            })
                            .collect();
                        me.serialize(&name, items).await.map(|_| BV::Undefined)
                    }
                    other => return Err(format!("exec: unknown method {other}")),
                };
                result.map_err(|e| e.wire())
            })
        });
        let me = self.clone();
        bridge.r#async("$znode.sqliteHost.close", move |a| {
            let me = me.clone();
            Box::pin(async move {
                if let Some(name) = a.string(0) {
                    me.close(&name).await;
                }
                Ok(BV::Undefined)
            })
        });
        let me = self.clone();
        bridge.r#async("$znode.sqliteHost.delete", move |a| {
            let me = me.clone();
            Box::pin(async move {
                if let Some(name) = a.string(0) {
                    me.delete(&name).await;
                }
                Ok(BV::Undefined)
            })
        });
        let me = self.clone();
        bridge.async_now("$znode.sqliteHost.partitionKeys", move |a| {
            Ok(BV::Array(a.string(0).map(|n| me.partition_keys(&n)).unwrap_or_default().into_iter().map(BV::String).collect()))
        });
        let me = self.clone();
        bridge.r#async("$znode.sqliteHost.closeSessional", move |_| {
            let me = me.clone();
            Box::pin(async move {
                me.close_sessional().await;
                Ok(BV::Undefined)
            })
        });
        let me = self.clone();
        bridge.r#async("$znode.sqliteHost.closeAll", move |_| {
            let me = me.clone();
            Box::pin(async move {
                me.close_all().await;
                Ok(BV::Undefined)
            })
        });
        bridge.value("$znode.sqliteHostEnabled", BV::Bool(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use serde_json::json;

    fn st(query: &str, params: serde_json::Value) -> Statement {
        Statement { query: query.into(), params: params.as_array().unwrap().iter().cloned().map(BV::from_js).collect() }
    }

    #[test]
    fn node_sqlite3_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let host = SqliteHost::new(dir.path().join("sqlite/_production"));
        block_on(async {
            let (v, migrate) = host.open("u1/msg", Some(3), false).await.unwrap();
            assert_eq!((v, migrate), (0, true));
            // Migration statements run while the handshake is open.
            host.run("u1/msg", st("CREATE TABLE m (id TEXT PRIMARY KEY, n INTEGER, f REAL, b BLOB, t TEXT); PRAGMA user_version=3;", json!([])))
                .await
                .unwrap();
            host.migrated("u1/msg");
            assert_eq!(host.open("u1/msg", Some(3), false).await.unwrap(), (3, false));
            assert_eq!(host.open("u1/msg", None, false).await.unwrap(), (3, false));

            let (id, changes) = host.run("u1/msg", st("INSERT INTO m VALUES (?, ?, ?, ?, ?)", json!(["a", 1, 1.5, {"__bytes": "AAE="}, null]))).await.unwrap();
            assert_eq!((id, changes), (1, 1));
            // A failed UNIQUE constraint resolves.
            assert!(host.run("u1/msg", st("INSERT INTO m (id) VALUES (?)", json!(["a"]))).await.is_ok());
            // Other errors carry node-sqlite3's code.
            let e = host.run("u1/msg", st("INSERT INTO nope VALUES (?)", json!([1]))).await.unwrap_err();
            assert_eq!(e.code, "SQLITE_ERROR");
            assert!(e.wire().starts_with("SQLITE_ERROR:1:no such table"));

            host.runs("u1/msg", "INSERT INTO m (id, n) VALUES (?, ?)".into(), vec![vec![BV::str("b"), BV::int(2)], vec![BV::str("c"), BV::Bool(true)]]).await.unwrap();
            // A failing row rolls the whole batch back.
            assert!(host.runs("u1/msg", "INSERT INTO m (id) VALUES (?)".into(), vec![vec![BV::str("d")], vec![BV::str("d")]]).await.is_err());
            let rows = host.all("u1/msg", st("SELECT id, n, f, b, t FROM m ORDER BY id", json!([]))).await.unwrap();
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].get("b"), Some(&BV::Bytes(vec![0, 1])));
            assert_eq!(rows[0].get("f"), Some(&BV::float(1.5)));
            assert_eq!(rows[0].get("t"), Some(&BV::Null));
            assert_eq!(rows[2].get("n"), Some(&BV::int(1)));
            assert!(host.get("u1/msg", st("SELECT * FROM m WHERE id = ?", json!(["zz"]))).await.unwrap().is_none());

            host.serialize(
                "u1/msg",
                vec![
                    SerializedItem::Run(st("DELETE FROM m WHERE id = ?", json!(["a"]))),
                    SerializedItem::Runs { query: "UPDATE m SET n = ? WHERE id = ?".into(), rows: vec![vec![BV::int(9), BV::str("b")]] },
                ],
            )
            .await
            .unwrap();
            let rows = host
                .optimized_all("u1/msg", vec!["SELECT id FROM m WHERE id = ?".into()], vec![vec![vec![BV::str("b")], vec![BV::str("c")]]])
                .await
                .unwrap();
            assert_eq!(rows.len(), 2);

            assert!(host.file_path("u1/msg").is_file());
            assert_eq!(host.partition_keys("u1/msg"), vec![String::new()]);
            host.open("part/g1/k1", None, false).await.unwrap();
            assert_eq!(host.partition_keys("part"), vec!["k1".to_string()]);
            host.delete("u1/msg").await;
            assert!(!host.file_path("u1/msg").exists());
            // Path components cannot escape the root.
            assert_eq!(host.file_path("../../x"), dir.path().join("sqlite/_production/x.db"));
        });
    }
}

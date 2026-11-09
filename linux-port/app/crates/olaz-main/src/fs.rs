//! `$znode.fs*` and `$zFileManager`. Port of `FileSystemBridge.swift` + `FileManagerBridge`.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use olaz_core::value::{Args, BV};
use olaz_host::bridge::{BridgeResult, ZaloBridge};

use crate::platform::ZaloPlatform;

/// `"<CODE>: <path>"` → JS `err.code`.
pub fn fs_error(code: &str, path: &str) -> String {
    format!("{code}: {path}")
}

fn io_err(e: std::io::Error) -> String {
    e.to_string()
}

const DISTANT_PAST_MS: f64 = -62_135_769_600_000.0;

pub fn stat(path: &str) -> BridgeResult {
    let meta = std::fs::metadata(path).map_err(|_| fs_error("ENOENT", path))?;
    let link = std::fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false);
    use std::os::unix::fs::PermissionsExt;
    let ms = |t: std::io::Result<std::time::SystemTime>| {
        t.ok()
            .and_then(|t| match t.duration_since(std::time::UNIX_EPOCH) {
                Ok(d) => Some(d.as_secs_f64() * 1000.0),
                Err(e) => Some(-e.duration().as_secs_f64() * 1000.0),
            })
            .unwrap_or(DISTANT_PAST_MS)
    };
    let mtime = ms(meta.modified());
    let birth = ms(meta.created());
    Ok(BV::obj([
        ("__stats", BV::Bool(true)),
        ("size", BV::int(meta.len() as i64)),
        ("mode", BV::int((meta.permissions().mode() & 0o7777) as i64)),
        ("mtimeMs", BV::float(mtime)),
        ("ctimeMs", BV::float(mtime)),
        ("atimeMs", BV::float(mtime)),
        ("birthtimeMs", BV::float(birth)),
        // `attributesOfItem` follows nothing: a symlink path reports the link itself as a symlink.
        ("isFile", BV::Bool(!link && meta.is_file())),
        ("isDirectory", BV::Bool(!link && meta.is_dir())),
        ("isSymbolicLink", BV::Bool(link)),
    ]))
}

/// Hard links of `file` under `scope` (same device and inode), bounded walk.
pub fn hardlinks(file: &str, scope: &str, limit: usize) -> Vec<String> {
    use std::os::unix::fs::MetadataExt;
    let Ok(target) = std::fs::symlink_metadata(file) else { return vec![] };
    if file.is_empty() || target.nlink() <= 1 || scope.is_empty() {
        return vec![];
    }
    let root = crate::platform::canonical(Path::new(scope));
    let root = if Path::new(scope).is_absolute() { standardize(Path::new(scope)) } else { root };
    let mut links = Vec::new();
    for (seen, entry) in walkdir::WalkDir::new(&root).min_depth(1).into_iter().filter_map(Result::ok).enumerate() {
        if seen >= limit {
            break;
        }
        let Ok(info) = std::fs::symlink_metadata(entry.path()) else { continue };
        if info.file_type().is_file() && info.ino() == target.ino() && info.dev() == target.dev() {
            links.push(entry.path().to_string_lossy().into_owned());
        }
    }
    links
}

fn standardize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn disk_usage(path: &str) -> BridgeResult {
    let c = std::ffi::CString::new(path).map_err(|e| e.to_string())?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let total = (s.f_blocks as i64) * (s.f_frsize as i64);
    let available = (s.f_bavail as i64) * (s.f_frsize as i64);
    Ok(BV::obj([
        ("total", BV::int(total)),
        ("free", BV::int(available)),
        ("available", BV::int(available)),
        ("used", BV::int((total - available).max(0))),
    ]))
}

pub fn install_file_system(bridge: &Rc<ZaloBridge>, platform: &Rc<ZaloPlatform>) {
    let p = platform.clone();
    let writable = move |path: &str| -> Result<String, String> {
        if p.is_writable(path) { Ok(path.to_string()) } else { Err(fs_error("EACCES", path)) }
    };
    let writable = Rc::new(writable);
    let exists = |path: &str| Path::new(path).exists() || std::fs::symlink_metadata(path).is_ok();
    let require = move |path: &str| -> Result<String, String> {
        if exists(path) { Ok(path.to_string()) } else { Err(fs_error("ENOENT", path)) }
    };

    let w = writable.clone();
    let mkdir = Rc::new(move |path: &str, recursive: bool| -> Result<(), String> {
        if recursive && Path::new(path).is_dir() {
            return Ok(());
        }
        let path = w(path)?;
        if recursive { std::fs::create_dir_all(&path) } else { std::fs::create_dir(&path) }.map_err(io_err)
    });
    let w = writable.clone();
    let remove = Rc::new(move |path: &str| -> Result<(), String> {
        if exists(path) {
            let path = w(path)?;
            let meta = std::fs::symlink_metadata(&path).map_err(io_err)?;
            if meta.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) }.map_err(io_err)?;
        }
        Ok(())
    });
    let w = writable.clone();
    let rm = remove.clone();
    let move_item = Rc::new(move |from: &str, to: &str| -> Result<(), String> {
        w(to)?;
        if exists(to) {
            rm(to)?;
        }
        std::fs::rename(require(from)?, to).map_err(io_err)
    });
    let w = writable.clone();
    let rm = remove.clone();
    let copy = Rc::new(move |from: &str, to: &str| -> Result<(), String> {
        w(to)?;
        if exists(to) {
            rm(to)?;
        }
        if let Some(parent) = Path::new(to).parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        copy_recursive(Path::new(&require(from)?), Path::new(to)).map_err(io_err)
    });

    // Each operation as async (callback/promise) and, where Node has one, *Sync.
    let both = |names: &[&str], sync: &[&str], body: Rc<dyn Fn(Vec<BV>) -> BridgeResult>| {
        for n in names {
            let b = body.clone();
            bridge.async_now(n, move |a| b(a));
        }
        for n in sync {
            let b = body.clone();
            bridge.sync(n, move |a| b(a));
        }
    };
    let arg0 = |a: &Vec<BV>| a.string(0).unwrap_or_default();
    let recursive = |a: &Vec<BV>| a.get(1).and_then(|o| o.get("recursive")).and_then(BV::as_bool).unwrap_or(false);

    both(&["$znode.fs.access", "$znode.fsPromise.access"], &["$znode.fs.accessSync"], Rc::new(move |a| {
        require(&arg0(&a))?;
        Ok(BV::Undefined)
    }));
    bridge.sync("$znode.fs.existsSync", move |a| Ok(BV::Bool(exists(&arg0(&a)))));
    bridge.sync("$znode.fsExtra.existsSync", move |a| Ok(BV::Bool(exists(&arg0(&a)))));
    bridge.async_now("$znode.fsExtra.pathExists", move |a| Ok(BV::Bool(exists(&arg0(&a)))));
    both(
        &["$znode.fs.stat", "$znode.fs.lstat", "$znode.fsPromise.stat", "$znode.fsPromise.lstat"],
        &["$znode.fs.statSync", "$znode.fsExtra.statSync"],
        Rc::new(move |a| stat(&arg0(&a))),
    );
    let m = mkdir.clone();
    both(&["$znode.fs.mkdir", "$znode.fsPromise.mkdir"], &["$znode.fs.mkdirSync"], Rc::new(move |a| {
        m(&arg0(&a), recursive(&a))?;
        Ok(BV::Undefined)
    }));
    let m = mkdir.clone();
    both(&["$znode.fsExtra.ensureDir"], &["$znode.fsExtra.ensureDirSync"], Rc::new(move |a| {
        m(&arg0(&a), true)?;
        Ok(BV::Undefined)
    }));
    both(&["$znode.fs.readdir", "$znode.fsPromise.readdir"], &["$znode.fs.readdirSync"], Rc::new(move |a| {
        let path = require(&arg0(&a))?;
        let mut names: Vec<String> =
            std::fs::read_dir(&path).map_err(io_err)?.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned())).collect();
        names.sort();
        Ok(BV::Array(names.into_iter().map(BV::String).collect()))
    }));
    let r = remove.clone();
    both(
        &["$znode.fs.unlink", "$znode.fsPromise.unlink", "$znode.fs.rmdir", "$znode.fsPromise.rmdir", "$znode.fsPromise.rm", "$znode.fsExtra.remove"],
        &["$znode.fs.unlinkSync", "$znode.fsExtra.removeSync"],
        Rc::new(move |a| {
            r(&arg0(&a))?;
            Ok(BV::Undefined)
        }),
    );
    let mv = move_item.clone();
    both(&["$znode.fsExtra.move", "$znode.fsExtra.rename"], &["$znode.fsExtra.renameSync"], Rc::new(move |a| {
        mv(&arg0(&a), &a.string(1).unwrap_or_default())?;
        Ok(BV::Undefined)
    }));
    let cp = copy.clone();
    both(&["$znode.fsExtra.copy"], &["$znode.fsExtra.copySync"], Rc::new(move |a| {
        cp(&arg0(&a), &a.string(1).unwrap_or_default())?;
        Ok(BV::Undefined)
    }));
    let w = writable.clone();
    both(&["$znode.fs.link", "$znode.fsPromise.link"], &[], Rc::new(move |a| {
        let from = require(&arg0(&a))?;
        let to = w(&a.string(1).unwrap_or_default())?;
        std::fs::hard_link(from, to).map_err(io_err)?;
        Ok(BV::Undefined)
    }));
    bridge.async_now("$znode.fs.exists", move |a| Ok(BV::Bool(exists(&arg0(&a)))));
    let r = remove.clone();
    bridge.async_now("$znode.rimraf", move |a| {
        r(&arg0(&a))?;
        Ok(BV::Undefined)
    });
    bridge.r#async("$znode.nativelibs.fileUtilitiesAddon.detectHardlinksAsync", |a| {
        let (file, scope) = (a.string(0).unwrap_or_default(), a.string(1).unwrap_or_default());
        Box::pin(async move {
            let found = blocking(move || hardlinks(&file, &scope, 200_000)).await;
            Ok(BV::Array(found.into_iter().map(BV::String).collect()))
        })
    });
    bridge.sync("$znode.nativelibs.fileUtilsAddon.getDiskUsage", |a| disk_usage(&a.string(0).unwrap_or_else(|| "/".into())));

    bridge.async_now("$znode.fs.readFile", move |a| {
        let path = require(&arg0(&a))?;
        let data = std::fs::read(&path).unwrap_or_default();
        let encoding = a.string(1).or_else(|| a.get(1).and_then(|o| o.get("encoding")).and_then(BV::as_str).map(str::to_string));
        Ok(if encoding.is_none() { BV::Bytes(data) } else { BV::String(String::from_utf8_lossy(&data).into_owned()) })
    });
    let w = writable.clone();
    bridge.async_now("$znode.fs.writeFile", move |a| {
        let path = w(&arg0(&a))?;
        olaz_core::paths::write_atomic(Path::new(&path), &a.data(1).unwrap_or_default()).map_err(io_err)?;
        Ok(BV::Undefined)
    });
}

fn copy_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// Runs blocking work on a background thread and awaits it.
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.expect("blocking task")
}

/// `$zFileManager` — preload-local file helpers.
pub fn install_file_manager(bridge: &Rc<ZaloBridge>, platform: &Rc<ZaloPlatform>) {
    let user_data = platform.user_data.clone();
    let ud = user_data.clone();
    let path_in = move |dir: Option<String>, name: &str| -> PathBuf { dir.map(PathBuf::from).unwrap_or_else(|| ud.clone()).join(name) };

    bridge.sync("$zFileManager.getSentryConfig", |_| Ok(BV::Undefined));
    bridge.async_now("$zFileManager.saveSentryConfig", |_| Ok(BV::Undefined));
    bridge.async_now("$zFileManager.getViewerkey", move |a| {
        let file = path_in(a.string(0), "z_u.txt");
        match std::fs::read(&file) {
            Ok(d) => Ok(BV::String(String::from_utf8_lossy(&d).into_owned())),
            Err(_) => Err("z_u.txt not found".into()),
        }
    });
    let ud = user_data.clone();
    bridge.async_now("$zFileManager.writeRecentId", move |a| {
        olaz_core::paths::write_atomic(&ud.join("z_u.txt"), a.string(0).unwrap_or_default().as_bytes()).map_err(io_err)?;
        Ok(BV::Undefined)
    });

    // Client-side log files (zlog / QoS / action log). Kept local; never uploaded.
    let log_dir = user_data.join("cal");
    let log_path = {
        let log_dir = log_dir.clone();
        move |name: &str| -> PathBuf { log_dir.join(Path::new(name).file_name().map(|n| n.to_os_string()).unwrap_or_default()) }
    };
    let (ld, lp) = (log_dir.clone(), log_path.clone());
    bridge.async_now("$zFileManager.writeZLog", move |a| {
        std::fs::create_dir_all(&ld).map_err(io_err)?;
        let url = lp(&a.string(0).unwrap_or_else(|| "z.log".into()));
        let data = a.data(1).unwrap_or_default();
        let overwrite = a.dict(2).and_then(|d| d.get("overwrite")).and_then(BV::as_bool).unwrap_or(false);
        if overwrite || !url.exists() {
            olaz_core::paths::write_atomic(&url, &data).map_err(io_err)?;
        } else {
            append(&url, &data)?;
        }
        Ok(BV::Undefined)
    });
    let (ld, lp) = (log_dir.clone(), log_path.clone());
    bridge.async_now("$zFileManager.writeZLogBetween", move |a| {
        std::fs::create_dir_all(&ld).map_err(io_err)?;
        append(&lp(&a.string(0).unwrap_or_else(|| "z.log".into())), &a.data(1).unwrap_or_default())?;
        Ok(BV::Undefined)
    });
    let lp = log_path.clone();
    bridge.async_now("$zFileManager.readZLog", move |a| {
        Ok(std::fs::read(lp(&a.string(0).unwrap_or_default())).map(|d| BV::String(String::from_utf8_lossy(&d).into_owned())).unwrap_or(BV::Undefined))
    });
    let ld = log_dir.clone();
    bridge.async_now("$zFileManager.scandirLog", move |_| {
        let names: Vec<BV> = std::fs::read_dir(&ld)
            .map(|r| r.filter_map(|e| e.ok().map(|e| BV::String(e.file_name().to_string_lossy().into_owned()))).collect())
            .unwrap_or_default();
        Ok(BV::Array(names))
    });
    bridge.async_now("$zFileManager.getExtraLogFiles", |_| Ok(BV::Array(vec![])));
    for name in ["getQosLog", "getActionLog", "readUptimeLog"] {
        bridge.async_now(&format!("$zFileManager.{name}"), |_| Ok(BV::str("")));
    }
    for name in [
        "removeQosLog",
        "removeActionLog",
        "writeActionLog",
        "writeUptimeLog",
        "truncateFileUpto",
        "cleanupLogFileDescriptors",
        "initBinaryLogFile",
        "sealCalfLog",
        "mergeCalfLogGroup",
        "writeLogRange",
        "trustBlob",
    ] {
        bridge.async_now(&format!("$zFileManager.{name}"), |_| Ok(BV::Undefined));
    }
    bridge.async_now("$zFileManager.getInstallSource", |_| Err("ENOENT: install source".into()));

    // File access for attachments the user picked (absolute paths from open panels).
    bridge.async_now("$zFileManager.getFileArrayBuffer", |a| {
        let path = a.string(0).unwrap_or_default();
        std::fs::read(&path).map(BV::ArrayBuffer).map_err(|_| fs_error("ENOENT", &path))
    });
    bridge.async_now("$zFileManager.readFileAsync", |a| {
        let path = a.string(0).unwrap_or_default();
        let data = std::fs::read(&path).map_err(|_| fs_error("ENOENT", &path))?;
        Ok(if a.string(1).is_none() { BV::Bytes(data) } else { BV::String(String::from_utf8_lossy(&data).into_owned()) })
    });
    bridge.async_now("$zFileManager.svcStatAsync", |a| {
        let meta = std::fs::metadata(a.string(0).unwrap_or_default()).map_err(io_err)?;
        Ok(BV::obj([("size", BV::int(meta.len() as i64))]))
    });
    let p = platform.clone();
    bridge.async_now("$zFileManager.writeBufferToPath", move |a| {
        let target = a.string(0).unwrap_or_default();
        if !p.is_writable(&target) {
            return Err(fs_error("EACCES", &target));
        }
        olaz_core::paths::write_atomic(Path::new(&target), &a.data(1).unwrap_or_default()).map_err(io_err)?;
        Ok(BV::Bool(true))
    });
}

fn append(path: &Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(io_err)?;
    f.write_all(data).map_err(io_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use olaz_core::exec::TestExecutor;
    use olaz_core::paths::XdgPaths;
    use serde_json::{Value, json};

    fn setup() -> (tempfile::TempDir, Rc<TestExecutor>, Rc<ZaloBridge>, Rc<ZaloPlatform>) {
        let dir = tempfile::tempdir().unwrap();
        let t = TestExecutor::new();
        let b = ZaloBridge::new(t.exec());
        let p = Rc::new(ZaloPlatform::new("26.10.20", XdgPaths::rooted(dir.path())));
        install_file_system(&b, &p);
        install_file_manager(&b, &p);
        (dir, t, b, p)
    }

    fn sync(b: &ZaloBridge, path: &str, args: Value) -> Value {
        serde_json::from_slice(&b.handle_sync_request(serde_json::to_vec(&json!({"path": path, "args": args})).unwrap().as_slice())).unwrap()
    }

    fn call(t: &TestExecutor, b: &ZaloBridge, path: &str, args: Value) -> (Option<Value>, Option<String>) {
        let slot = Rc::new(std::cell::RefCell::new(None));
        let s = slot.clone();
        b.handle_message(json!({"kind": "call", "path": path, "args": args}), Box::new(move |v, e| *s.borrow_mut() = Some((v, e))));
        t.run_until_stalled();
        slot.borrow_mut().take().unwrap()
    }

    #[test]
    fn writes_are_confined() {
        let (dir, t, b, p) = setup();
        let ud = p.user_data.clone();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, ud.join("x")).unwrap();
        let (_, e) = call(&t, &b, "$znode.fs.writeFile", json!([ud.join("x/passwd").to_string_lossy(), "p"]));
        assert!(e.unwrap().starts_with("EACCES: "));
        let (_, e) = call(&t, &b, "$znode.fs.writeFile", json!([ud.join("ok/a.txt").to_string_lossy(), "hi"]));
        assert_eq!(e, None);
        assert_eq!(std::fs::read_to_string(ud.join("ok/a.txt")).unwrap(), "hi");
        let out = sync(&b, "$znode.fs.mkdirSync", json!(["/etc/olaz-test", {"recursive": true}]));
        assert_eq!(out["error"], "EACCES: /etc/olaz-test");
    }

    #[test]
    fn read_missing_is_enoent_and_stats_shape() {
        let (_d, t, b, p) = setup();
        let (_, e) = call(&t, &b, "$znode.fs.readFile", json!([p.user_data.join("missing").to_string_lossy()]));
        assert!(e.unwrap().starts_with("ENOENT: "));
        std::fs::write(p.user_data.join("f.txt"), b"abc").unwrap();
        let out = sync(&b, "$znode.fs.statSync", json!([p.user_data.join("f.txt").to_string_lossy()]));
        let v = &out["value"];
        assert_eq!(v["__stats"], true);
        assert_eq!(v["size"], 3);
        assert_eq!(v["isFile"], true);
        assert_eq!(v["isDirectory"], false);
        assert_eq!(v["ctimeMs"], v["mtimeMs"]);
        assert_eq!(v["atimeMs"], v["mtimeMs"]);
        let (v, _) = call(&t, &b, "$znode.fs.readFile", json!([p.user_data.join("f.txt").to_string_lossy(), "utf8"]));
        assert_eq!(v, Some(json!("abc")));
        let (v, _) = call(&t, &b, "$znode.fs.readFile", json!([p.user_data.join("f.txt").to_string_lossy(), {"encoding": "utf8"}]));
        assert_eq!(v, Some(json!("abc")));
        let (v, _) = call(&t, &b, "$znode.fs.readFile", json!([p.user_data.join("f.txt").to_string_lossy()]));
        assert_eq!(v, Some(json!({"__bytes": "YWJj"})));
        let out = sync(&b, "$znode.fs.readdirSync", json!([p.user_data.to_string_lossy()]));
        assert_eq!(out["value"], json!(["f.txt"]));
        assert_eq!(sync(&b, "$znode.fs.existsSync", json!([p.user_data.join("nope").to_string_lossy()]))["value"], false);
    }

    #[test]
    fn move_copy_remove() {
        let (_d, t, b, p) = setup();
        let ud = &p.user_data;
        std::fs::create_dir_all(ud.join("d/sub")).unwrap();
        std::fs::write(ud.join("d/sub/x"), b"1").unwrap();
        assert_eq!(call(&t, &b, "$znode.fsExtra.copy", json!([ud.join("d").to_string_lossy(), ud.join("e").to_string_lossy()])).1, None);
        assert!(ud.join("e/sub/x").is_file());
        assert_eq!(call(&t, &b, "$znode.fsExtra.move", json!([ud.join("e").to_string_lossy(), ud.join("d").to_string_lossy()])).1, None);
        assert!(!ud.join("e").exists() && ud.join("d/sub/x").is_file());
        assert_eq!(call(&t, &b, "$znode.rimraf", json!([ud.join("d").to_string_lossy()])).1, None);
        assert!(!ud.join("d").exists());
        assert_eq!(call(&t, &b, "$znode.fsExtra.remove", json!([ud.join("never").to_string_lossy()])).1, None);
    }

    #[test]
    fn hardlink_detection() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::write(&a, b"x").unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::hard_link(&a, dir.path().join("sub/b")).unwrap();
        std::fs::write(dir.path().join("single"), b"y").unwrap();
        let mut found = hardlinks(&a.to_string_lossy(), &dir.path().to_string_lossy(), 1000);
        found.sort();
        assert_eq!(found.len(), 2);
        assert!(hardlinks(&dir.path().join("single").to_string_lossy(), &dir.path().to_string_lossy(), 1000).is_empty());
        assert!(hardlinks("", "/", 10).is_empty());
    }

    #[test]
    fn file_manager_members() {
        let (_d, t, b, p) = setup();
        let (_, e) = call(&t, &b, "$zFileManager.getViewerkey", json!([]));
        assert_eq!(e.as_deref(), Some("z_u.txt not found"));
        call(&t, &b, "$zFileManager.writeRecentId", json!(["vk"]));
        assert_eq!(call(&t, &b, "$zFileManager.getViewerkey", json!([])).0, Some(json!("vk")));
        call(&t, &b, "$zFileManager.writeZLog", json!(["../evil/z.log", "a"]));
        call(&t, &b, "$zFileManager.writeZLog", json!(["z.log", "b"]));
        assert_eq!(std::fs::read_to_string(p.user_data.join("cal/z.log")).unwrap(), "ab");
        call(&t, &b, "$zFileManager.writeZLog", json!(["z.log", "c", {"overwrite": true}]));
        assert_eq!(call(&t, &b, "$zFileManager.readZLog", json!(["z.log"])).0, Some(json!("c")));
        assert_eq!(call(&t, &b, "$zFileManager.scandirLog", json!([])).0, Some(json!(["z.log"])));
        assert_eq!(call(&t, &b, "$zFileManager.getInstallSource", json!([])).1.as_deref(), Some("ENOENT: install source"));
        assert_eq!(call(&t, &b, "$zFileManager.writeBufferToPath", json!(["/etc/x", "a"])).1.as_deref(), Some("EACCES: /etc/x"));
        let target = p.user_data.join("buf/x.bin");
        assert_eq!(call(&t, &b, "$zFileManager.writeBufferToPath", json!([target.to_string_lossy(), {"__bytes": "AAE="}])).0, Some(json!(true)));
        assert_eq!(
            call(&t, &b, "$zFileManager.getFileArrayBuffer", json!([target.to_string_lossy()])).0,
            Some(json!({"__bytes": "AAE=", "__arraybuffer": true}))
        );
        assert_eq!(call(&t, &b, "$zFileManager.svcStatAsync", json!([target.to_string_lossy()])).0, Some(json!({"size": 2})));
        assert_eq!(sync(&b, "$zFileManager.getSentryConfig", json!([]))["value"], json!({"__undefined": true}));
        let du = sync(&b, "$znode.nativelibs.fileUtilsAddon.getDiskUsage", json!(["/"]));
        assert!(du["value"]["total"].as_i64().unwrap() > 0);
    }
}

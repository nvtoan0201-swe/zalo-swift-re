//! EngineLog: append-only diagnostics file, shapes never secrets.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::value::BV;

pub struct EngineLog {
    path: PathBuf,
    file: Mutex<Option<File>>,
    echo: bool,
}

static SHARED: Mutex<Option<&'static EngineLog>> = Mutex::new(None);

impl EngineLog {
    /// Opens `<dir>/engine.log`, moving a non-empty previous log to `engine.previous.log`.
    pub fn open(dir: &Path) -> EngineLog {
        let _ = std::fs::create_dir_all(dir);
        let path = dir.join("engine.log");
        if std::fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false) {
            let previous = dir.join("engine.previous.log");
            let _ = std::fs::remove_file(&previous);
            let _ = std::fs::rename(&path, previous);
        }
        let file = OpenOptions::new().create(true).write(true).truncate(true).open(&path).ok();
        EngineLog { path, file: Mutex::new(file), echo: std::env::var_os("OLAZ_LOG_STDERR").is_some() }
    }

    /// Installs the process-wide log (leaked: lives for the whole process).
    pub fn install(log: EngineLog) -> &'static EngineLog {
        let leaked: &'static EngineLog = Box::leak(Box::new(log));
        *SHARED.lock().unwrap() = Some(leaked);
        leaked
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&self, line: &str) {
        let now = chrono::Local::now().format("%H:%M:%S%.3f");
        let truncated: String = line.chars().take(1500).collect();
        let text = format!("{now} {truncated}\n");
        if self.echo {
            eprint!("{text}");
        }
        if let Some(f) = self.file.lock().unwrap().as_mut() {
            let _ = f.write_all(text.as_bytes());
        }
    }
}

/// Writes to the installed log (tests: `<tmp>/OlazTests-logs/engine.log` on first use).
pub fn write(line: impl AsRef<str>) {
    let shared = *SHARED.lock().unwrap();
    let log = match shared {
        Some(l) => l,
        None => {
            let dir = std::env::temp_dir().join("OlazTests-logs");
            EngineLog::install(EngineLog::open(&dir))
        }
    };
    log.write(line.as_ref());
}

/// Argument summary without values: `(string, object{3}, bytes[512])`.
pub fn shape(args: &[BV]) -> String {
    let parts: Vec<String> = args
        .iter()
        .map(|a| match a {
            BV::String(_) => "string".into(),
            BV::Bool(_) => "bool".into(),
            BV::Number(_) => "number".into(),
            BV::Bytes(d) | BV::ArrayBuffer(d) => format!("bytes[{}]", d.len()),
            BV::Array(a) => format!("array[{}]", a.len()),
            BV::Object(o) => format!("object{{{}}}", o.keys().take(6).cloned().collect::<Vec<_>>().join(",")),
            BV::Null => "null".into(),
            BV::Undefined => "Optional<Any>".into(),
        })
        .collect();
    format!("({})", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes() {
        let args = vec![
            BV::str("secret"),
            BV::int(1),
            BV::Bool(true),
            BV::Bytes(vec![0; 512]),
            BV::Array(vec![BV::Null]),
            BV::obj([("z", BV::Null), ("a", BV::Null)]),
            BV::Null,
        ];
        assert_eq!(shape(&args), "(string, number, bool, bytes[512], array[1], object{a,z}, null)");
    }

    #[test]
    fn rotates_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let log = EngineLog::open(dir.path());
        log.write("first");
        drop(log);
        let log = EngineLog::open(dir.path());
        log.write(&"x".repeat(2000));
        let prev = std::fs::read_to_string(dir.path().join("engine.previous.log")).unwrap();
        assert!(prev.ends_with(" first\n"));
        let cur = std::fs::read_to_string(dir.path().join("engine.log")).unwrap();
        assert_eq!(cur.trim_end().split(' ').nth(1).unwrap().len(), 1500);
    }
}

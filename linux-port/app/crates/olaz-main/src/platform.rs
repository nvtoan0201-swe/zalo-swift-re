//! `ZaloPlatform` : data directories, device identity, electron-store files, `isWritable`.

use std::path::{Component, Path, PathBuf};

use olaz_core::paths::{XdgPaths, write_atomic};
use serde_json::{Map, Value};

pub struct ZaloPlatform {
    pub app_version: String,
    pub client_type: String,
    pub client_version: String,
    pub paths: XdgPaths,
    pub user_data: PathBuf,
}

impl ZaloPlatform {
    pub fn new(app_version: &str, paths: XdgPaths) -> Self {
        let user_data = paths.user_data();
        let _ = std::fs::create_dir_all(&user_data);
        Self {
            app_version: app_version.to_string(),
            client_type: olaz_core::CLIENT_TYPE.into(),
            client_version: olaz_core::CLIENT_VERSION.into(),
            paths,
            user_data,
        }
    }

    pub fn user_agent(&self) -> String {
        olaz_core::user_agent(&self.client_type, &self.client_version)
    }

    /// Stable per-install device id (`imei`): stored once in settings.json `olaz.imei`, never regenerated.
    pub fn imei(&self) -> String {
        let mut settings = Settings::load(&self.paths);
        if let Some(v) = settings.get_str("olaz.imei") {
            return v;
        }
        let fresh = uuid::Uuid::new_v4().to_string().to_lowercase();
        settings.set("olaz.imei", Value::String(fresh.clone()));
        settings.save();
        fresh
    }

    /// Electron `app.getPath(name)`.
    pub fn path(&self, name: &str) -> String {
        let p: PathBuf = match name {
            "userData" | "appData" => self.user_data.clone(),
            "temp" => self.paths.temp(),
            "home" => self.paths.home.clone(),
            "downloads" => self.paths.user_dir("DOWNLOAD"),
            "documents" => self.paths.user_dir("DOCUMENTS"),
            "desktop" => self.paths.user_dir("DESKTOP"),
            "pictures" => self.paths.user_dir("PICTURES"),
            "logs" => self.user_data.join("logs"),
            other => {
                olaz_core::log::write(format!("platform: getPath({other}) → userData"));
                self.user_data.clone()
            }
        };
        p.to_string_lossy().into_owned()
    }

    // MARK: electron-store files (<userData>/<name>.json)

    pub fn store_path(&self, name: &str) -> PathBuf {
        let safe = name.replace('/', "_").replace("..", "_");
        self.user_data.join(format!("{safe}.json"))
    }

    pub fn read_store(&self, name: &str) -> Map<String, Value> {
        std::fs::read(self.store_path(name))
            .ok()
            .and_then(|d| serde_json::from_slice::<Value>(&d).ok())
            .and_then(|v| match v {
                Value::Object(m) => Some(m),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Pretty-printed, **sorted keys**, atomic.
    pub fn write_store(&self, name: &str, object: &Map<String, Value>) -> Result<(), String> {
        // Keys serialize sorted, recursively (gpui turns on serde_json's `preserve_order`, so sort explicitly).
        let data = serde_json::to_vec_pretty(&sorted(&Value::Object(object.clone()))).map_err(|e| e.to_string())?;
        write_atomic(&self.store_path(name), &data).map_err(|e| e.to_string())
    }

    /// Writes only inside userData, temp, downloads — symlinks resolved on both sides.
    pub fn is_writable(&self, path: &str) -> bool {
        let resolved = canonical(Path::new(path));
        [self.user_data.clone(), PathBuf::from(self.path("temp")), PathBuf::from(self.path("downloads"))]
            .iter()
            .map(|r| canonical(r))
            .any(|root| resolved.starts_with(&root))
    }
}

/// Standardize, then resolve symlinks on the longest existing ancestor and re-append the rest.
pub fn canonical(path: &Path) -> PathBuf {
    let mut std_path = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                std_path.pop();
            }
            Component::CurDir => {}
            other => std_path.push(other),
        }
    }
    let mut existing = std_path.clone();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    while existing.as_os_str() != "/" && !existing.as_os_str().is_empty() && std::fs::symlink_metadata(&existing).is_err() {
        if let Some(name) = existing.file_name() {
            rest.insert(0, name.to_os_string());
        }
        existing.pop();
    }
    let mut resolved = std::fs::canonicalize(&existing).unwrap_or(existing);
    for part in rest {
        resolved.push(part);
    }
    resolved
}

/// `$XDG_CONFIG_HOME/olaz/settings.json` — the UserDefaults equivalent (`olaz.*` keys).
pub struct Settings {
    path: PathBuf,
    values: Map<String, Value>,
}

impl Settings {
    pub fn load(paths: &XdgPaths) -> Self {
        let path = paths.settings_file();
        let values = std::fs::read(&path)
            .ok()
            .and_then(|d| serde_json::from_slice::<Value>(&d).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        Self { path, values }
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }
    pub fn get_str(&self, key: &str) -> Option<String> {
        self.values.get(key).and_then(Value::as_str).map(str::to_string)
    }
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.values.get(key).and_then(Value::as_bool)
    }
    pub fn set(&mut self, key: &str, value: Value) {
        self.values.insert(key.to_string(), value);
    }
    pub fn remove(&mut self, key: &str) {
        self.values.remove(key);
    }
    pub fn save(&self) {
        if let Ok(data) = serde_json::to_vec_pretty(&Value::Object(self.values.clone())) {
            let _ = write_atomic(&self.path, &data);
        }
    }
}

/// A copy of `v` whose objects list their keys in sorted order.
fn sorted(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort();
            Value::Object(keys.into_iter().map(|k| (k.clone(), sorted(&o[k]))).collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform() -> (tempfile::TempDir, ZaloPlatform) {
        let dir = tempfile::tempdir().unwrap();
        let p = ZaloPlatform::new("26.10.20", XdgPaths::rooted(dir.path()));
        (dir, p)
    }

    #[test]
    fn paths() {
        let (dir, p) = platform();
        assert_eq!(p.path("userData"), dir.path().join("data/olaz/ZaloData").to_string_lossy());
        assert_eq!(p.path("appData"), p.path("userData"));
        assert_eq!(p.path("logs"), dir.path().join("data/olaz/ZaloData/logs").to_string_lossy());
        assert_eq!(p.path("whatever"), p.path("userData"));
        assert_eq!(p.path("temp"), dir.path().join("run/olaz").to_string_lossy());
    }

    #[test]
    fn stores_sorted_and_safe_names() {
        let (_d, p) = platform();
        assert!(p.store_path("a/../b").ends_with("a__.._b.json") || p.store_path("a/../b").ends_with("a___b.json"));
        assert!(p.read_store("config").is_empty());
        let mut m = Map::new();
        m.insert("z".into(), Value::from(1));
        m.insert("a".into(), Value::from(2));
        p.write_store("config", &m).unwrap();
        let text = std::fs::read_to_string(p.store_path("config")).unwrap();
        assert!(text.find("\"a\"").unwrap() < text.find("\"z\"").unwrap());
        assert_eq!(p.read_store("config"), m);
        std::fs::write(p.store_path("bad"), b"[1,2]").unwrap();
        assert!(p.read_store("bad").is_empty());
    }

    #[test]
    fn imei_is_stable() {
        let (_d, p) = platform();
        let a = p.imei();
        assert_eq!(a, p.imei());
        assert_eq!(a.len(), 36);
        assert_eq!(a, a.to_lowercase());
    }

    #[test]
    fn writable_resolves_symlinks() {
        let (dir, p) = platform();
        let ud = &p.user_data;
        assert!(p.is_writable(&ud.join("x/y.json").to_string_lossy()));
        assert!(p.is_writable(&ud.to_string_lossy()));
        assert!(!p.is_writable("/etc/passwd"));
        assert!(!p.is_writable(&format!("{}/../../escape", ud.display())));
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, ud.join("link")).unwrap();
        assert!(!p.is_writable(&ud.join("link/passwd").to_string_lossy()), "symlink must not lead out");
        assert!(p.is_writable(&p.path("downloads")));
        // prefix trick: userData2 is not inside userData
        assert!(!p.is_writable(&format!("{}2/x", ud.display())));
    }
}

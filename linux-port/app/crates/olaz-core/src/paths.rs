//! XDG data locations (data, config, cache, state) and the folders the official client expects in them.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct XdgPaths {
    pub home: PathBuf,
    pub data_home: PathBuf,
    pub config_home: PathBuf,
    pub cache_home: PathBuf,
    pub state_home: PathBuf,
    pub runtime_dir: Option<PathBuf>,
}

fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).map(PathBuf::from).filter(|p| p.is_absolute())
}

impl XdgPaths {
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
        Self {
            data_home: env_dir("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share")),
            config_home: env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")),
            cache_home: env_dir("XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache")),
            state_home: env_dir("XDG_STATE_HOME").unwrap_or_else(|| home.join(".local/state")),
            runtime_dir: env_dir("XDG_RUNTIME_DIR"),
            home,
        }
    }

    /// All roots under one directory (tests, `OLAZ_HOME`).
    pub fn rooted(root: &Path) -> Self {
        Self {
            home: root.join("home"),
            data_home: root.join("data"),
            config_home: root.join("config"),
            cache_home: root.join("cache"),
            state_home: root.join("state"),
            runtime_dir: Some(root.join("run")),
        }
    }

    pub fn app_data(&self) -> PathBuf {
        self.data_home.join("olaz")
    }
    /// Zalo user data (stores, z_u.txt, logs `cal/`).
    pub fn user_data(&self) -> PathBuf {
        self.app_data().join("ZaloData")
    }
    pub fn runtime_root(&self) -> PathBuf {
        self.app_data().join("ZaloRuntime")
    }
    pub fn webkit_data(&self) -> PathBuf {
        self.app_data().join("webkit")
    }
    pub fn cookies_file(&self) -> PathBuf {
        self.app_data().join("cookies.json")
    }
    pub fn settings_file(&self) -> PathBuf {
        self.config_home.join("olaz/settings.json")
    }
    pub fn cache(&self) -> PathBuf {
        self.cache_home.join("olaz")
    }
    pub fn logs(&self) -> PathBuf {
        self.state_home.join("olaz/logs")
    }
    /// Electron `temp`: `$XDG_RUNTIME_DIR/olaz`, else `/tmp/olaz-$UID`.
    pub fn temp(&self) -> PathBuf {
        match &self.runtime_dir {
            Some(dir) => dir.join("olaz"),
            None => PathBuf::from(format!("/tmp/olaz-{}", unsafe_uid())),
        }
    }

    /// `xdg-user-dir NAME` (reads `user-dirs.dirs`), falling back to `~/<Default>`.
    pub fn user_dir(&self, key: &str) -> PathBuf {
        let default = match key {
            "DOWNLOAD" => "Downloads",
            "DOCUMENTS" => "Documents",
            "DESKTOP" => "Desktop",
            "PICTURES" => "Pictures",
            _ => "",
        };
        let file = self.config_home.join("user-dirs.dirs");
        if let Ok(text) = std::fs::read_to_string(file) {
            let var = format!("XDG_{key}_DIR=");
            for line in text.lines() {
                if let Some(rest) = line.trim().strip_prefix(&var) {
                    let value = rest.trim().trim_matches('"').replace("$HOME", &self.home.to_string_lossy());
                    if !value.is_empty() {
                        return PathBuf::from(value);
                    }
                }
            }
        }
        self.home.join(default)
    }
}

fn unsafe_uid() -> u32 {
    std::fs::metadata("/proc/self").map(|m| std::os::unix::fs::MetadataExt::uid(&m)).unwrap_or(0)
}

/// Writes `data` to `path` atomically (temp file + rename), creating parent directories.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let p = XdgPaths::rooted(Path::new("/r"));
        assert_eq!(p.user_data(), Path::new("/r/data/olaz/ZaloData"));
        assert_eq!(p.runtime_root(), Path::new("/r/data/olaz/ZaloRuntime"));
        assert_eq!(p.cookies_file(), Path::new("/r/data/olaz/cookies.json"));
        assert_eq!(p.settings_file(), Path::new("/r/config/olaz/settings.json"));
        assert_eq!(p.temp(), Path::new("/r/run/olaz"));
    }

    #[test]
    fn user_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let p = XdgPaths::rooted(dir.path());
        assert_eq!(p.user_dir("DOWNLOAD"), dir.path().join("home/Downloads"));
        std::fs::create_dir_all(&p.config_home).unwrap();
        std::fs::write(p.config_home.join("user-dirs.dirs"), "XDG_DOWNLOAD_DIR=\"$HOME/Tải về\"\n").unwrap();
        assert_eq!(p.user_dir("DOWNLOAD"), dir.path().join("home/Tải về"));
    }

    #[test]
    fn atomic_write() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a/b.json");
        write_atomic(&f, b"1").unwrap();
        write_atomic(&f, b"2").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"2");
        assert_eq!(std::fs::read_dir(dir.path().join("a")).unwrap().count(), 1);
    }
}

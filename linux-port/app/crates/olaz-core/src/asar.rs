//! Read-only access to an Electron `app.asar` archive. Port of `AsarArchive.swift`.
//!
//! Format: `[u32 4][u32 headerPickleSize]` then a pickle `[u32 payload][u32 jsonLen][json]`;
//! file offsets in the JSON header are relative to `8 + headerPickleSize`.
//! Entries flagged `unpacked` live beside the archive in `app.asar.unpacked/`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use memmap2::Mmap;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum AsarError {
    #[error("Không đọc được {}", .0.display())]
    Unreadable(PathBuf),
    #[error("app.asar không hợp lệ")]
    BadHeader,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    offset: usize,
    size: usize,
    unpacked: bool,
}

pub struct AsarArchive {
    path: PathBuf,
    map: Mmap,
    base: usize,
    entries: HashMap<String, Entry>,
}

impl AsarArchive {
    pub fn open(path: &Path) -> Result<Self, AsarError> {
        let file = std::fs::File::open(path).map_err(|_| AsarError::Unreadable(path.to_path_buf()))?;
        // SAFETY: the runtime folder is owned by Olaz and never modified while in use.
        let map = unsafe { Mmap::map(&file) }.map_err(|_| AsarError::Unreadable(path.to_path_buf()))?;
        if map.len() <= 16 {
            return Err(AsarError::BadHeader);
        }
        let header_pickle_size = u32_at(&map, 4) as usize;
        let json_length = u32_at(&map, 12) as usize;
        if 16 + json_length > map.len() {
            return Err(AsarError::BadHeader);
        }
        let json: Value = serde_json::from_slice(&map[16..16 + json_length]).map_err(|_| AsarError::BadHeader)?;
        if !json.is_object() {
            return Err(AsarError::BadHeader);
        }
        let mut entries = HashMap::new();
        index(&json, "", &mut entries);
        Ok(Self { path: path.to_path_buf(), map, base: 8 + header_pickle_size, entries })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn contains(&self, path: &str) -> bool {
        self.entries.contains_key(&normalize(path))
    }

    /// Bytes of a file inside the archive, e.g. `pc-dist/login.html`.
    pub fn read(&self, path: &str) -> Option<Cow<'_, [u8]>> {
        let key = normalize(path);
        let entry = *self.entries.get(&key)?;
        if entry.unpacked {
            let unpacked = self.path.parent().unwrap_or(Path::new(".")).join("app.asar.unpacked").join(&key);
            return std::fs::read(unpacked).ok().map(Cow::Owned);
        }
        let start = self.base.checked_add(entry.offset)?;
        let end = start.checked_add(entry.size)?;
        if end > self.map.len() {
            return None;
        }
        Some(Cow::Borrowed(&self.map[start..end]))
    }

    pub fn paths_with_prefix(&self, prefix: &str) -> Vec<String> {
        let mut out: Vec<String> = self.entries.keys().filter(|k| k.starts_with(prefix)).cloned().collect();
        out.sort();
        out
    }
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn index(node: &Value, prefix: &str, entries: &mut HashMap<String, Entry>) {
    let Some(files) = node.get("files").and_then(Value::as_object) else { return };
    for (name, child) in files {
        if !child.is_object() {
            continue;
        }
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        if child.get("files").is_some() {
            index(child, &path, entries);
        } else if let Some(size) = child.get("size").and_then(Value::as_f64) {
            let offset = child.get("offset").and_then(Value::as_str).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
            let unpacked = child.get("unpacked").and_then(Value::as_bool).unwrap_or(false);
            entries.insert(path, Entry { offset, size: size as usize, unpacked });
        }
    }
}

/// Percent-decode, then strip every leading `/`.
fn normalize(path: &str) -> String {
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| path.to_string());
    decoded.trim_start_matches('/').to_string()
}

/// `OfficialZaloInstall`: the runtime Olaz installed, laid out like Zalo.app's `Contents/`.
pub struct InstallInfo {
    pub app_dir: PathBuf,
    pub version: String,
    pub asar: AsarArchive,
}

pub fn locate(app_dir: &Path) -> Result<InstallInfo, AsarError> {
    let asar = AsarArchive::open(&app_dir.join("Contents/Resources/app.asar"))?;
    Ok(InstallInfo { app_dir: app_dir.to_path_buf(), version: plist_version(app_dir).unwrap_or_else(|| "?".into()), asar })
}

/// `CFBundleShortVersionString` of `<app>/Contents/Info.plist`.
pub fn plist_version(app_dir: &Path) -> Option<String> {
    let value = plist::Value::from_file(app_dir.join("Contents/Info.plist")).ok()?;
    value.as_dictionary()?.get("CFBundleShortVersionString")?.as_string().map(str::to_string)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a minimal asar: header pickle + files (port of `AsarReading.makeArchive`).
    pub fn make_archive(dir: &Path, files: &[(&str, &[u8])]) -> PathBuf {
        let mut tree = serde_json::json!({});
        let mut payload = Vec::new();
        let mut offset = 0usize;
        fn insert(node: &mut serde_json::Value, parts: &[&str], leaf: serde_json::Value) {
            let files = node.as_object_mut().unwrap().entry("files").or_insert_with(|| serde_json::json!({}));
            if parts.len() == 1 {
                files[parts[0]] = leaf;
            } else {
                if files.get(parts[0]).is_none() {
                    files[parts[0]] = serde_json::json!({});
                }
                insert(files.get_mut(parts[0]).unwrap(), &parts[1..], leaf);
            }
        }
        for (path, data) in files {
            let parts: Vec<&str> = path.split('/').collect();
            insert(&mut tree, &parts, serde_json::json!({"size": data.len(), "offset": offset.to_string()}));
            payload.extend_from_slice(data);
            offset += data.len();
        }
        let json = serde_json::to_vec(&tree).unwrap();
        let padded = (json.len() + 3) & !3;
        let mut header = Vec::new();
        header.extend_from_slice(&((padded + 4) as u32).to_le_bytes());
        header.extend_from_slice(&(json.len() as u32).to_le_bytes());
        header.extend_from_slice(&json);
        header.resize(8 + padded, 0);
        let mut archive = Vec::new();
        archive.extend_from_slice(&4u32.to_le_bytes());
        archive.extend_from_slice(&(header.len() as u32).to_le_bytes());
        archive.extend_from_slice(&header);
        archive.extend_from_slice(&payload);
        let path = dir.join("app.asar");
        std::fs::write(&path, archive).unwrap();
        path
    }

    #[test]
    fn reads_files_from_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_archive(dir.path(), &[("pc-dist/login.html", b"<html>hi</html>"), ("pc-dist/lazy/a.js", b"1+1")]);
        let asar = AsarArchive::open(&path).unwrap();
        assert_eq!(asar.read("pc-dist/login.html").as_deref(), Some(&b"<html>hi</html>"[..]));
        assert_eq!(asar.read("/pc-dist/lazy/a.js").as_deref(), Some(&b"1+1"[..]));
        assert_eq!(asar.read("//pc-dist/lazy/%61.js").as_deref(), Some(&b"1+1"[..]));
        assert!(asar.read("pc-dist/missing.js").is_none());
        assert_eq!(asar.paths_with_prefix("pc-dist/lazy").len(), 1);
        assert!(asar.contains("pc-dist/login.html"));
    }

    #[test]
    fn rejects_bad_archives() {
        let dir = tempfile::tempdir().unwrap();
        let short = dir.path().join("short.asar");
        std::fs::write(&short, b"0123456789abcdef").unwrap();
        assert!(matches!(AsarArchive::open(&short), Err(AsarError::BadHeader)));
        let garbage = dir.path().join("garbage.asar");
        let mut bytes = vec![4, 0, 0, 0, 20, 0, 0, 0, 16, 0, 0, 0, 5, 0, 0, 0];
        bytes.extend_from_slice(b"nojsn and more bytes");
        std::fs::write(&garbage, bytes).unwrap();
        assert!(matches!(AsarArchive::open(&garbage), Err(AsarError::BadHeader)));
        assert!(matches!(AsarArchive::open(&dir.path().join("missing.asar")), Err(AsarError::Unreadable(_))));
        assert_eq!(AsarError::BadHeader.to_string(), "app.asar không hợp lệ");
    }

    #[test]
    fn out_of_range_entry_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_archive(dir.path(), &[("a.txt", b"abc")]);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 1);
        std::fs::write(&path, bytes).unwrap();
        let asar = AsarArchive::open(&path).unwrap();
        assert!(asar.read("a.txt").is_none());
    }

    #[test]
    fn unpacked_entries_are_read_beside_the_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.asar");
        let unpacked_dir = dir.path().join("app.asar.unpacked/native");
        std::fs::create_dir_all(&unpacked_dir).unwrap();
        std::fs::write(unpacked_dir.join("x.node"), b"binary").unwrap();
        let tree = serde_json::json!({"files": {"native": {"files": {"x.node": {"size": 6, "unpacked": true}}}}});
        let json = serde_json::to_vec(&tree).unwrap();
        let padded = (json.len() + 3) & !3;
        let mut archive = Vec::new();
        archive.extend_from_slice(&4u32.to_le_bytes());
        archive.extend_from_slice(&((8 + padded) as u32).to_le_bytes());
        archive.extend_from_slice(&((padded + 4) as u32).to_le_bytes());
        archive.extend_from_slice(&(json.len() as u32).to_le_bytes());
        archive.extend_from_slice(&json);
        archive.resize(16 + padded, 0);
        std::fs::write(&path, archive).unwrap();
        let asar = AsarArchive::open(&path).unwrap();
        assert_eq!(asar.read("native/x.node").as_deref(), Some(&b"binary"[..]));
    }

    #[test]
    fn plist_version_and_locate() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path();
        std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
        make_archive(&app.join("Contents/Resources"), &[("pc-dist/index.html", b"x")]);
        assert_eq!(locate(app).unwrap().version, "?");
        let mut dict = plist::Dictionary::new();
        dict.insert("CFBundleShortVersionString".into(), "26.10.20".into());
        plist::Value::Dictionary(dict).to_file_xml(app.join("Contents/Info.plist")).unwrap();
        let info = locate(app).unwrap();
        assert_eq!(info.version, "26.10.20");
        assert!(info.asar.contains("pc-dist/index.html"));
    }
}

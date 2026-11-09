//! The Zalo runtime Olaz runs: fetched from VNG on first launch, verified, kept in
//! `$XDG_DATA_HOME/olaz/ZaloRuntime/{current,pending,work}`. Port of `ZaloRuntimeInstaller.swift`.
//!
//! Linux keeps only `Contents/Info.plist` + `Contents/Resources/app.asar` (the helpers are macOS binaries).
//! Trust anchor: the SHA-256 of the pinned DMG. An unpinned release (zalo.me "latest") is refused unless its
//! Apple code signature can be verified — not implemented on Linux, so it is always refused.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub url: String,
    /// SHA-256 (hex) of the DMG when the release is pinned.
    pub sha256: Option<String>,
}

pub fn pinned() -> Release {
    Release {
        version: "26.10.20".into(),
        url: "https://res-download-pc.zadn.vn/mac/ZaloSetup-universal-26.10.20.dmg".into(),
        sha256: Some("a45d30b7f7b060f76b87722ef419b8f41d23a2913cdef084dcb0955dde8d85a0".into()),
    }
}

pub const LATEST_LINK: &str = "https://zalo.me/download/zalo-pc";
pub const BROWSER_UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
/// What is copied out of Zalo.app on Linux.
pub const PARTS: [&str; 2] = ["Contents/Info.plist", "Contents/Resources/app.asar"];

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Looking,
    Downloading { received: u64, total: Option<u64> },
    Verifying,
    Installing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Failure(pub String);

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Failure {}

fn fail<T>(msg: impl Into<String>) -> Result<T, Failure> {
    Err(Failure(msg.into()))
}

pub struct Layout {
    pub root: PathBuf,
}

impl Layout {
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf() }
    }
    pub fn current(&self) -> PathBuf {
        self.root.join("current")
    }
    pub fn pending(&self) -> PathBuf {
        self.root.join("pending")
    }
    pub fn work(&self) -> PathBuf {
        self.root.join("work")
    }
}

/// Linux: Info.plist + app.asar present, and app.asar opens as a valid asar.
pub fn is_complete(runtime: &Path) -> bool {
    runtime.join("Contents/Info.plist").is_file()
        && olaz_core::asar::AsarArchive::open(&runtime.join("Contents/Resources/app.asar")).is_ok()
}

pub fn version_of(bundle: &Path) -> Option<String> {
    olaz_core::asar::plist_version(bundle)
}

/// At launch, before anything reads the runtime: drop leftovers and swap in a prepared upgrade.
pub fn prepare_for_launch(layout: &Layout) {
    let (current, pending, work) = (layout.current(), layout.pending(), layout.work());
    let _ = std::fs::remove_dir_all(&work);
    if !pending.exists() {
        return;
    }
    if !is_complete(&pending) {
        let _ = std::fs::remove_dir_all(&pending);
        return;
    }
    let retired = work.join(format!("retired-{}", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&work)?;
        if current.exists() {
            std::fs::rename(&current, &retired)?;
        }
        std::fs::rename(&pending, &current)
    })();
    match result {
        Ok(()) => olaz_core::log::write(format!("runtime: switched to Zalo {}", version_of(&current).unwrap_or_else(|| "?".into()))),
        Err(e) => {
            if !current.exists() && retired.exists() {
                let _ = std::fs::rename(&retired, &current);
            }
            olaz_core::log::write(format!("runtime: upgrade not applied: {e}"));
        }
    }
    let _ = std::fs::remove_dir_all(&retired);
}

/// `26.10.20` is newer than `26.9.10`: component-wise numbers (non-numbers → 0).
pub fn is_newer(version: &str, other: &str) -> bool {
    let parse = |s: &str| s.split('.').map(|p| p.parse::<i64>().unwrap_or(0)).collect::<Vec<_>>();
    let (a, b) = (parse(version), parse(other));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// `ZaloSetup-universal-26.10.20.dmg` → `26.10.20`.
pub fn version_from_installer_name(name: &str) -> Option<String> {
    let rest = name.strip_prefix("ZaloSetup-")?;
    let (arch, tail) = rest.split_once('-')?;
    if arch.is_empty() || !arch.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let version = tail.strip_suffix(".dmg")?;
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() < 2 || parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    Some(version.to_string())
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 4 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(olaz_core::crypto::hex(&hasher.finalize()))
}

/// 7-Zip binary: `$OLAZ_7Z`, bundled `7zz`, or the distro's `7z`.
pub fn seven_zip() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OLAZ_7Z") {
        return Some(PathBuf::from(p));
    }
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    let mut candidates: Vec<PathBuf> = exe_dir.into_iter().map(|d| d.join("7zz")).collect();
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join("7zz"));
            candidates.push(dir.join("7z"));
        }
    }
    candidates.into_iter().find(|c| c.is_file())
}

fn run_tool(tool: &Path, args: &[&std::ffi::OsStr]) -> Result<(), Failure> {
    let name = tool.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let out = Command::new(tool).args(args).stdin(std::process::Stdio::null()).output().map_err(|e| Failure(format!("{name} lỗi -1: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        fail(format!("{name} lỗi {}: {msg}", out.status.code().unwrap_or(-1)))
    }
}

pub type Progress = std::sync::Arc<dyn Fn(Step) + Send + Sync>;

pub struct Installer {
    pub release: Release,
    pub allows_latest: bool,
    pub work: PathBuf,
    pub progress: Progress,
    /// A DMG already on disk to use instead of downloading (`OLAZ_ZALO_DMG`, tests).
    pub local_dmg: Option<PathBuf>,
}

impl Installer {
    pub fn new(work: PathBuf, progress: Progress) -> Self {
        let local_dmg = std::env::var_os("OLAZ_ZALO_DMG").map(PathBuf::from).filter(|p| p.is_file());
        Self { release: pinned(), allows_latest: true, work, progress, local_dmg }
    }

    /// Installs into `destination`, replacing what is there; returns the Zalo version installed.
    pub async fn install(&self, destination: &Path) -> Result<String, Failure> {
        (self.progress)(Step::Looking);
        std::fs::create_dir_all(&self.work).map_err(|e| Failure(e.to_string()))?;
        let dmg = self.work.join(format!("ZaloSetup-{}.dmg", uuid::Uuid::new_v4()));
        let result = self.install_via(&dmg, destination).await;
        let _ = std::fs::remove_file(&dmg);
        result
    }

    async fn install_via(&self, dmg: &Path, destination: &Path) -> Result<String, Failure> {
        let source = match &self.local_dmg {
            Some(local) => {
                (self.progress)(Step::Verifying);
                self.verify(&self.release, local)?;
                local.clone()
            }
            None => {
                match self.fetch(&self.release, dmg).await {
                    Ok(()) => {}
                    Err(e) if self.allows_latest => {
                        olaz_core::log::write(format!("runtime: Zalo {} unavailable: {e}", self.release.version));
                        let latest = latest_release().await?;
                        self.fetch(&latest, dmg).await?;
                    }
                    Err(e) => return Err(e),
                }
                dmg.to_path_buf()
            }
        };
        self.install_from_disk_image(&source, destination)
    }

    fn verify(&self, release: &Release, file: &Path) -> Result<(), Failure> {
        match &release.sha256 {
            Some(expected) => {
                let actual = sha256_file(file).map_err(|e| Failure(e.to_string()))?;
                if &actual != expected {
                    return fail(format!("Bộ cài Zalo {} tải về không khớp mã kiểm tra.", release.version));
                }
                Ok(())
            }
            // Never install unverified code.
            None => fail("Bộ cài Zalo không có chữ ký hợp lệ của VNG nên Olaz không dùng nó"),
        }
    }

    async fn fetch(&self, release: &Release, file: &Path) -> Result<(), Failure> {
        let progress = self.progress.clone();
        let reporter = move |received: u64, total: Option<u64>| progress(Step::Downloading { received, total });
        download(&release.url, file, reporter).await?;
        (self.progress)(Step::Verifying);
        self.verify(release, file)
    }

    /// 7-Zip extraction (no mount), Zalo.app found by its Info.plist, parts copied into a staging folder,
    /// completeness checked, then `destination` replaced.
    pub fn install_from_disk_image(&self, dmg: &Path, destination: &Path) -> Result<String, Failure> {
        let Some(tool) = seven_zip() else { return fail("Không tìm thấy 7-Zip (7zz) để giải nén bộ cài Zalo.") };
        let extract = self.work.join(format!("x-{}", uuid::Uuid::new_v4()));
        let staging = self.work.join(format!("staging-{}", uuid::Uuid::new_v4()));
        let result = (|| -> Result<String, Failure> {
            std::fs::create_dir_all(&extract).map_err(|e| Failure(e.to_string()))?;
            let out = format!("-o{}", extract.display());
            let args: Vec<&std::ffi::OsStr> = vec![
                "x".as_ref(),
                "-y".as_ref(),
                out.as_ref(),
                dmg.as_os_str(),
                "*/Zalo.app/Contents/Info.plist".as_ref(),
                "*/Zalo.app/Contents/Resources/app.asar".as_ref(),
                "-xr!*:com.apple.*".as_ref(),
            ];
            run_tool(&tool, &args)?;
            let app = find_zalo_app(&extract)?;
            (self.progress)(Step::Installing);
            let version = version_of(&app).ok_or_else(|| Failure("Không đọc được phiên bản Zalo.".into()))?;
            for part in PARTS {
                let target = staging.join(part);
                std::fs::create_dir_all(target.parent().unwrap()).map_err(|e| Failure(e.to_string()))?;
                let from = app.join(part);
                if std::fs::rename(&from, &target).is_err() {
                    std::fs::copy(&from, &target).map_err(|e| Failure(e.to_string()))?;
                }
            }
            if !is_complete(&staging) {
                return fail("Bản sao Zalo không đầy đủ.");
            }
            let _ = std::fs::remove_dir_all(destination);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|e| Failure(e.to_string()))?;
            }
            std::fs::rename(&staging, destination).map_err(|e| Failure(e.to_string()))?;
            olaz_core::log::write(format!("runtime: Zalo {version} installed"));
            Ok(version)
        })();
        let _ = std::fs::remove_dir_all(&extract);
        let _ = std::fs::remove_dir_all(&staging);
        result
    }
}

/// The `.app` whose Info.plist has `CFBundleIdentifier == com.vng.zalo` (searched two levels deep: `<volume>/<Zalo.app>`).
fn find_zalo_app(extract: &Path) -> Result<PathBuf, Failure> {
    let is_zalo = |app: &Path| {
        plist::Value::from_file(app.join("Contents/Info.plist"))
            .ok()
            .and_then(|v| v.as_dictionary()?.get("CFBundleIdentifier")?.as_string().map(|s| s == "com.vng.zalo"))
            .unwrap_or(false)
    };
    for entry in walk_apps(extract, 2) {
        if is_zalo(&entry) {
            return Ok(entry);
        }
    }
    fail("Bộ cài không chứa Zalo.app.")
}

fn walk_apps(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "app") {
            out.push(p);
        } else if depth > 0 && p.is_dir() {
            out.extend(walk_apps(&p, depth - 1));
        }
    }
    out
}

fn client(redirects: bool) -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(BROWSER_UA)
        .redirect(if redirects { reqwest::redirect::Policy::limited(5) } else { reqwest::redirect::Policy::none() })
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .build()
        .expect("http client")
}

/// Download with progress at most once per MiB (plus 0 and completion). Runs on the I/O runtime.
pub async fn download(url: &str, file: &Path, progress: impl Fn(u64, Option<u64>) + Send + 'static) -> Result<(), Failure> {
    let url = url.to_string();
    let file = file.to_path_buf();
    let task = olaz_host::net::runtime().spawn(async move {
        use futures::StreamExt;
        use std::io::Write;
        let response = client(true).get(&url).send().await.map_err(|e| Failure(describe(&e)))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return fail(format!("Máy chủ tải Zalo trả về lỗi HTTP {status}."));
        }
        let total = response.content_length();
        let mut out = std::fs::File::create(&file).map_err(|e| Failure(e.to_string()))?;
        let mut received = 0u64;
        let mut last: Option<u64> = None;
        progress(0, total);
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| Failure(describe(&e)))?;
            out.write_all(&chunk).map_err(|e| Failure(e.to_string()))?;
            received += chunk.len() as u64;
            let due = Some(received) == total || last.is_none_or(|l| received - l >= 1 << 20);
            if due {
                last = Some(received);
                progress(received, total);
            }
        }
        out.sync_all().map_err(|e| Failure(e.to_string()))?;
        Ok(())
    });
    task.await.map_err(|e| Failure(e.to_string()))?
}

fn describe(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(inner) = src {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        src = inner.source();
    }
    s
}

/// The release zalo.me offers now (HEAD, redirects not followed, ephemeral session).
pub async fn latest_release() -> Result<Release, Failure> {
    let task = olaz_host::net::runtime().spawn(async {
        let response = client(false).head(LATEST_LINK).send().await.map_err(|e| Failure(describe(&e)))?;
        let location = response.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
        Ok::<_, Failure>(location)
    });
    let location = task.await.map_err(|e| Failure(e.to_string()))??;
    let not_found = || Failure("Không tìm thấy bản Zalo PC cho Mac trên zalo.me.".into());
    let base = url::Url::parse(LATEST_LINK).unwrap();
    let url = location.and_then(|l| base.join(&l).ok()).filter(|u| u.scheme() == "https").ok_or_else(not_found)?;
    let name = url.path_segments().and_then(|mut s| s.next_back().map(str::to_string)).unwrap_or_default();
    let version = version_from_installer_name(&name).ok_or_else(not_found)?;
    Ok(Release { version, url: url.to_string(), sha256: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_runtime(dir: &Path, version: &str) {
        std::fs::create_dir_all(dir.join("Contents/Resources")).unwrap();
        let mut d = plist::Dictionary::new();
        d.insert("CFBundleShortVersionString".into(), version.into());
        plist::Value::Dictionary(d).to_file_xml(dir.join("Contents/Info.plist")).unwrap();
        // minimal valid asar with one file
        let json = br#"{"files":{"a":{"size":1,"offset":"0"}}}"#;
        let padded = (json.len() + 3) & !3;
        let mut out = Vec::new();
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(&((8 + padded) as u32).to_le_bytes());
        out.extend_from_slice(&((padded + 4) as u32).to_le_bytes());
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(json);
        out.resize(16 + padded, 0);
        out.push(b'x');
        std::fs::write(dir.join("Contents/Resources/app.asar"), out).unwrap();
    }

    #[test]
    fn reads_version_from_installer_name() {
        assert_eq!(version_from_installer_name("ZaloSetup-universal-26.10.20.dmg").as_deref(), Some("26.10.20"));
        assert_eq!(version_from_installer_name("ZaloSetup-universal-26.10.20-RC4.zip"), None);
        assert_eq!(version_from_installer_name("Zalo-3.2.64.dmg"), None);
        assert_eq!(version_from_installer_name("ZaloSetup-arm64-26.dmg"), None);
        let name = pinned().url.rsplit('/').next().unwrap().to_string();
        assert_eq!(version_from_installer_name(&name), Some(pinned().version));
    }

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("26.10.20", "26.9.10"));
        assert!(is_newer("27.1.5", "26.12.31"));
        assert!(!is_newer("26.10.20", "26.10.20"));
        assert!(!is_newer("26.10", "26.10.0"));
        assert!(!is_newer("26.9.10", "26.10.20"));
        assert!(is_newer("26.10.x1", "26.9"));
    }

    #[test]
    fn launch_swaps_in_prepared_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let l = Layout::new(dir.path());
        make_runtime(&l.current(), "26.9.10");
        make_runtime(&l.pending(), "26.10.20");
        std::fs::create_dir_all(l.work().join("staging-x")).unwrap();
        prepare_for_launch(&l);
        assert_eq!(version_of(&l.current()).as_deref(), Some("26.10.20"));
        assert!(is_complete(&l.current()));
        assert!(!l.pending().exists());
        assert_eq!(std::fs::read_dir(l.work()).map(|r| r.count()).unwrap_or(0), 0);
    }

    #[test]
    fn launch_drops_incomplete_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let l = Layout::new(dir.path());
        make_runtime(&l.current(), "26.9.10");
        std::fs::create_dir_all(l.pending().join("Contents")).unwrap();
        prepare_for_launch(&l);
        assert_eq!(version_of(&l.current()).as_deref(), Some("26.9.10"));
        assert!(!l.pending().exists());
    }

    #[test]
    fn interrupted_install_leaves_current_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let l = Layout::new(dir.path());
        make_runtime(&l.current(), "26.10.20");
        std::fs::create_dir_all(l.work().join("x-123/partial")).unwrap();
        std::fs::write(l.work().join("ZaloSetup-1.dmg"), b"half").unwrap();
        prepare_for_launch(&l);
        assert!(!l.work().exists());
        assert!(is_complete(&l.current()));
    }

    #[test]
    fn invalid_asar_is_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        make_runtime(dir.path(), "1.0");
        std::fs::write(dir.path().join("Contents/Resources/app.asar"), b"asar").unwrap();
        assert!(!is_complete(dir.path()));
    }

    #[test]
    fn tampered_dmg_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let dmg = dir.path().join("x.dmg");
        std::fs::write(&dmg, b"not the real installer").unwrap();
        let mut inst = Installer::new(dir.path().join("work"), std::sync::Arc::new(|_| {}));
        inst.local_dmg = Some(dmg);
        let dest = dir.path().join("current");
        let r = futures::executor::block_on(inst.install(&dest));
        assert_eq!(r, Err(Failure("Bộ cài Zalo 26.10.20 tải về không khớp mã kiểm tra.".into())));
        assert!(!dest.exists());
        let unpinned = Release { sha256: None, ..pinned() };
        assert!(inst.verify(&unpinned, &dir.path().join("x.dmg")).is_err(), "never install unverified code");
    }

    /// `OLAZ_ZALO_DMG=<pinned dmg>`: the whole path (checksum, 7-Zip, staging) against the official DMG.
    #[test]
    fn installs_from_official_disk_image() {
        let Some(dmg) = std::env::var_os("OLAZ_ZALO_DMG").map(PathBuf::from).filter(|p| p.is_file()) else { return };
        let dir = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
        let steps = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let s = steps.clone();
        let mut inst = Installer::new(dir.path().join("work"), std::sync::Arc::new(move |step| s.lock().unwrap().push(step)));
        inst.local_dmg = Some(dmg);
        let dest = dir.path().join("current");
        let version = futures::executor::block_on(inst.install(&dest)).unwrap();
        assert_eq!(version, pinned().version);
        assert!(is_complete(&dest));
        let info = olaz_core::asar::locate(&dest).unwrap();
        assert!(info.asar.contains("pc-dist/index.html") && info.asar.contains("pc-dist/login.html"));
        let steps = steps.lock().unwrap();
        assert!(steps.contains(&Step::Verifying) && steps.contains(&Step::Installing));
        assert_eq!(std::fs::read_dir(dir.path().join("work")).unwrap().count(), 0, "work emptied");
    }

    /// `OLAZ_NETWORK_TESTS=1`: zalo.me still redirects a Mac to a DMG whose name carries the version.
    #[test]
    fn zalo_me_offers_mac_installer() {
        if std::env::var_os("OLAZ_NETWORK_TESTS").is_none() {
            return;
        }
        let latest = futures::executor::block_on(latest_release()).unwrap();
        assert!(latest.url.ends_with(".dmg"));
        assert!(!is_newer(&pinned().version, &latest.version));
    }
}

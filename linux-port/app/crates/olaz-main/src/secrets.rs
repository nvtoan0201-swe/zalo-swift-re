//! zsafe-storage and the keychain cipher. Port of `SafeStorage` / `KeychainCipher` / `Keychain`.

use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use olaz_core::crypto::{aes_cbc, random_bytes};
use olaz_core::value::{Args, BV};
use serde_json::{Map, Value};
use sha2::Sha256;

use crate::platform::ZaloPlatform;

// MARK: zsafe-storage

const SAFE_KEY: &[u8; 32] = b"d9c07b9de9915af401607d3f360211ac";
const SAFE_IV: [u8; 16] = [155, 72, 37, 195, 9, 217, 80, 42, 145, 102, 171, 75, 214, 176, 134, 18];

pub fn safe_encrypt(plain: &str) -> Result<String, String> {
    Ok(STANDARD.encode(aes_cbc(plain.as_bytes(), SAFE_KEY, &SAFE_IV, true)?))
}

pub fn safe_decrypt(sealed: &str) -> Result<String, String> {
    let data = STANDARD.decode(sealed).unwrap_or_default();
    let plain = aes_cbc(&data, SAFE_KEY, &SAFE_IV, false)?;
    Ok(String::from_utf8_lossy(&plain).into_owned())
}

/// `zsafestorage-execute(method, ...args)`.
pub fn safe_storage_execute(platform: &ZaloPlatform, method: &str, args: &[BV]) -> Result<BV, String> {
    match method {
        "encryptString" => safe_encrypt(&args.string(0).unwrap_or_default()).map(BV::String),
        "decryptString" => safe_decrypt(&args.string(0).unwrap_or_default()).map(BV::String),
        "get" => {
            let Some(key) = args.string(0).filter(|k| !k.contains('.')) else { return Ok(BV::Undefined) };
            let store = platform.read_store("zsafe-storage");
            let sealed = store.get("zsskd").and_then(Value::as_object).and_then(|m| m.get(&key)).and_then(Value::as_str);
            Ok(sealed.and_then(|s| safe_decrypt(s).ok()).map(BV::String).unwrap_or(BV::Undefined))
        }
        "set" => {
            let Some(key) = args.string(0).filter(|k| !k.contains('.')) else { return Ok(BV::Undefined) };
            let mut store = platform.read_store("zsafe-storage");
            let mut values: Map<String, Value> = store.get("zsskd").and_then(Value::as_object).cloned().unwrap_or_default();
            match args.string(1) {
                Some(value) => {
                    values.insert(key, Value::String(safe_encrypt(&value)?));
                }
                None => {
                    values.remove(&key);
                }
            }
            store.insert("zsskd".into(), Value::Object(values));
            store.insert("zsskv".into(), Value::String("2".into()));
            platform.write_store("zsafe-storage", &store)?;
            Ok(BV::Undefined)
        }
        other => Err(format!("zsafestorage: unknown method {other}")),
    }
}

// MARK: secret store (Secret Service on Linux)

#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    Found(Vec<u8>),
    /// Neither service holds the item: the only answer that allows minting a new secret.
    NotFound,
    /// The store could not tell (locked, denied, no Secret Service): the item may exist.
    Failed(String),
}

pub trait SecretStore: Send + Sync {
    fn read(&self, account: &str) -> Lookup;
    fn write(&self, account: &str, data: &[u8]) -> Result<(), String>;
    fn delete_all(&self);
}

pub const SERVICE: &str = "app.olaz.secure";

/// In-memory store (tests; never used as a plaintext fallback in the app).
#[derive(Default)]
pub struct MemoryStore {
    pub items: Mutex<HashMap<String, Vec<u8>>>,
    pub fail: Mutex<Option<String>>,
}

impl SecretStore for MemoryStore {
    fn read(&self, account: &str) -> Lookup {
        if let Some(f) = self.fail.lock().unwrap().clone() {
            return Lookup::Failed(f);
        }
        match self.items.lock().unwrap().get(account) {
            Some(d) => Lookup::Found(d.clone()),
            None => Lookup::NotFound,
        }
    }
    fn write(&self, account: &str, data: &[u8]) -> Result<(), String> {
        if let Some(f) = self.fail.lock().unwrap().clone() {
            return Err(f);
        }
        self.items.lock().unwrap().insert(account.to_string(), data.to_vec());
        Ok(())
    }
    fn delete_all(&self) {
        self.items.lock().unwrap().clear();
    }
}

/// Secret Service (gnome-keyring / KWallet / Flatpak portal) via `oo7`. Attributes `{service, account}`.
pub struct SecretServiceStore;

impl SecretServiceStore {
    fn run<T>(f: impl std::future::Future<Output = T> + Send + 'static) -> T
    where
        T: Send + 'static,
    {
        let rt = olaz_host::net::runtime();
        let (tx, rx) = std::sync::mpsc::channel();
        rt.spawn(async move {
            let _ = tx.send(f.await);
        });
        rx.recv().expect("secret service task")
    }
}

impl SecretStore for SecretServiceStore {
    fn read(&self, account: &str) -> Lookup {
        let account = account.to_string();
        Self::run(async move {
            let keyring = match oo7::Keyring::new().await {
                Ok(k) => k,
                Err(e) => return Lookup::Failed(format!("Secret Service unavailable: {e}")),
            };
            if let Err(e) = keyring.unlock().await {
                return Lookup::Failed(format!("Keychain read failed ({e})"));
            }
            let attrs = HashMap::from([("service", SERVICE), ("account", account.as_str())]);
            match keyring.search_items(&attrs).await {
                Ok(items) => match items.first() {
                    None => Lookup::NotFound,
                    Some(item) => match item.secret().await {
                        Ok(secret) => Lookup::Found(secret.as_bytes().to_vec()),
                        Err(e) => Lookup::Failed(format!("Keychain read failed ({e})")),
                    },
                },
                Err(e) => Lookup::Failed(format!("Keychain read failed ({e})")),
            }
        })
    }

    fn write(&self, account: &str, data: &[u8]) -> Result<(), String> {
        let account = account.to_string();
        let data = data.to_vec();
        Self::run(async move {
            let keyring = oo7::Keyring::new().await.map_err(|e| format!("Keychain write failed ({e})"))?;
            let attrs = HashMap::from([("service", SERVICE), ("account", account.as_str())]);
            let _ = keyring.delete(&attrs).await;
            let label = format!("Olaz secure key ({})", account.trim_start_matches("zprotocol."));
            keyring.create_item(&label, &attrs, data, true).await.map_err(|e| format!("Keychain write failed ({e})"))
        })
    }

    fn delete_all(&self) {
        Self::run(async move {
            if let Ok(keyring) = oo7::Keyring::new().await {
                let attrs = HashMap::from([("service", SERVICE)]);
                let _ = keyring.delete(&attrs).await;
            }
        })
    }
}

// MARK: keychain cipher

type HmacSha256 = Hmac<Sha256>;
const INFO: &[u8] = b"ZprotocolSecureKDF";
const TYPE_RAW: u8 = 0x49;

fn derive(root: &[u8], salt: &[u8]) -> ([u8; 32], [u8; 32]) {
    let hk = hkdf::Hkdf::<Sha256>::new(Some(salt), root);
    let mut okm = [0u8; 64];
    hk.expand(INFO, &mut okm).expect("hkdf length");
    let (mut enc, mut mac) = ([0u8; 32], [0u8; 32]);
    enc.copy_from_slice(&okm[..32]);
    mac.copy_from_slice(&okm[32..]);
    (enc, mac)
}

fn root_key(store: &dyn SecretStore, identifier: &str, create: bool) -> Result<Vec<u8>, String> {
    let account = format!("zprotocol.{}", identifier.to_lowercase());
    match store.read(&account) {
        Lookup::Found(k) => Ok(k),
        // A key that exists but cannot be read now must never be shadowed by a fresh one.
        Lookup::Failed(status) => Err(if status.starts_with("Keychain") { status } else { format!("Keychain read failed ({status})") }),
        Lookup::NotFound => {
            if !create {
                return Err(format!("No secret for {identifier}"));
            }
            let key = random_bytes(32);
            store.write(&account, &key)?;
            Ok(key)
        }
    }
}

/// `[0x49][iv16][salt16][ct][hmac32]`, HMAC over `ct || iv || salt`.
pub fn keychain_encrypt(store: &dyn SecretStore, identifier: &str, items: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, String> {
    let root = root_key(store, identifier, true)?;
    items
        .iter()
        .map(|plain| {
            let (salt, iv) = (random_bytes(16), random_bytes(16));
            let (enc, mac) = derive(&root, &salt);
            let ct = aes_cbc(plain, &enc, &iv, true)?;
            let mut m = HmacSha256::new_from_slice(&mac).unwrap();
            m.update(&ct);
            m.update(&iv);
            m.update(&salt);
            let tag = m.finalize().into_bytes();
            Ok([&[TYPE_RAW][..], &iv, &salt, &ct, &tag].concat())
        })
        .collect()
}

pub fn keychain_decrypt(store: &dyn SecretStore, identifier: &str, items: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, String> {
    let root = root_key(store, identifier, false)?;
    items
        .iter()
        .map(|sealed| {
            if sealed.len() < 65 {
                return Err("Invalid encrypted data length".to_string());
            }
            let (iv, salt) = (&sealed[1..17], &sealed[17..33]);
            let (ct, tag) = (&sealed[33..sealed.len() - 32], &sealed[sealed.len() - 32..]);
            let (enc, mac) = derive(&root, salt);
            let mut m = HmacSha256::new_from_slice(&mac).unwrap();
            m.update(ct);
            m.update(iv);
            m.update(salt);
            m.verify_slice(tag).map_err(|_| "Invalid MAC".to_string())?;
            aes_cbc(ct, &enc, iv, false)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use olaz_core::paths::XdgPaths;

    #[test]
    fn zsafe_vector_and_round_trip() {
        // Same fixed key/IV as macOS: values are portable between platforms.
        let sealed = safe_encrypt("hello").unwrap();
        assert_eq!(safe_decrypt(&sealed).unwrap(), "hello");
        let node = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("node")).find(|n| n.is_file()));
        if let Some(node) = node {
            let js = "const c=require('crypto');const e=c.createCipheriv('aes-256-cbc',Buffer.from('d9c07b9de9915af401607d3f360211ac'),Buffer.from([155,72,37,195,9,217,80,42,145,102,171,75,214,176,134,18]));console.log(Buffer.concat([e.update('hello'),e.final()]).toString('base64'))";
            let out = std::process::Command::new(node).arg("-e").arg(js).output().unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), sealed);
        }
        assert_eq!(safe_decrypt("!!!"), Err("bad decrypt".into()));
    }

    #[test]
    fn zsafe_store_methods() {
        let dir = tempfile::tempdir().unwrap();
        let p = ZaloPlatform::new("1", XdgPaths::rooted(dir.path()));
        assert_eq!(safe_storage_execute(&p, "set", &[BV::str("a.b"), BV::str("x")]).unwrap(), BV::Undefined);
        assert!(!p.store_path("zsafe-storage").exists(), "dotted keys write nothing");
        safe_storage_execute(&p, "set", &[BV::str("k"), BV::str("secret")]).unwrap();
        let store = p.read_store("zsafe-storage");
        assert_eq!(store["zsskv"], "2");
        assert_ne!(store["zsskd"]["k"], "secret");
        assert_eq!(safe_storage_execute(&p, "get", &[BV::str("k")]).unwrap(), BV::str("secret"));
        safe_storage_execute(&p, "set", &[BV::str("k")]).unwrap();
        assert_eq!(safe_storage_execute(&p, "get", &[BV::str("k")]).unwrap(), BV::Undefined);
        assert_eq!(safe_storage_execute(&p, "nope", &[]).unwrap_err(), "zsafestorage: unknown method nope");
    }

    #[test]
    fn keychain_round_trip_and_failures() {
        let store = MemoryStore::default();
        let sealed = keychain_encrypt(&store, "Device", &[b"one".to_vec(), vec![]]).unwrap();
        assert_eq!(sealed[0][0], 0x49);
        assert!(store.items.lock().unwrap().contains_key("zprotocol.device"));
        assert_eq!(keychain_decrypt(&store, "DEVICE", &sealed).unwrap(), vec![b"one".to_vec(), vec![]]);
        let mut tampered = sealed[0].clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert_eq!(keychain_decrypt(&store, "device", &[tampered]).unwrap_err(), "Invalid MAC");
        assert_eq!(keychain_decrypt(&store, "device", &[vec![0; 10]]).unwrap_err(), "Invalid encrypted data length");
        assert_eq!(keychain_decrypt(&store, "other", &sealed).unwrap_err(), "No secret for other");
        // A failing store must never mint a new key.
        *store.fail.lock().unwrap() = Some("locked".into());
        assert!(keychain_encrypt(&store, "fresh", &[b"x".to_vec()]).unwrap_err().starts_with("Keychain read failed"));
        *store.fail.lock().unwrap() = None;
        assert!(!store.items.lock().unwrap().contains_key("zprotocol.fresh"));
        store.delete_all();
        assert!(store.items.lock().unwrap().is_empty());
    }

    /// An item sealed with a known root key opens (fixed vector built with Node's crypto).
    #[test]
    fn opens_items_sealed_elsewhere() {
        let store = MemoryStore::default();
        store.items.lock().unwrap().insert("zprotocol.dev".into(), vec![7u8; 32]);
        let node = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("node")).find(|n| n.is_file()));
        let Some(node) = node else { return };
        let js = r#"const c=require('crypto');const root=Buffer.alloc(32,7),salt=Buffer.alloc(16,1),iv=Buffer.alloc(16,2);
const okm=Buffer.from(c.hkdfSync('sha256',root,salt,'ZprotocolSecureKDF',64));const enc=okm.subarray(0,32),mac=okm.subarray(32);
const e=c.createCipheriv('aes-256-cbc',enc,iv);const ct=Buffer.concat([e.update('trusted'),e.final()]);
const tag=c.createHmac('sha256',mac).update(Buffer.concat([ct,iv,salt])).digest();
console.log(Buffer.concat([Buffer.from([0x49]),iv,salt,ct,tag]).toString('base64'))"#;
        let out = std::process::Command::new(node).arg("-e").arg(js).output().unwrap();
        let item = STANDARD.decode(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
        assert_eq!(keychain_decrypt(&store, "dev", &[item]).unwrap(), vec![b"trusted".to_vec()]);
    }
}

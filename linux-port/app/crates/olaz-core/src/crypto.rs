//! `$znode.crypto` — byte-for-byte parity with Node. Port of `CryptoBridge.swift`.

use std::collections::HashMap;
use std::sync::Mutex;

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, KeyIvInit, StreamCipher, generic_array::GenericArray};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use crate::value::{Args, BV};

pub type CryptoResult<T> = Result<T, String>;

const LENIENT: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent).with_decode_allow_trailing_bits(true),
);

// MARK: Node encodings

/// `bytes(value, encoding)`.
pub fn bytes(value: Option<&BV>, encoding: Option<&str>) -> CryptoResult<Vec<u8>> {
    match value {
        Some(BV::Bytes(b)) | Some(BV::ArrayBuffer(b)) => Ok(b.clone()),
        Some(BV::String(s)) => {
            let enc = encoding.unwrap_or("utf8").to_lowercase();
            Ok(match enc.as_str() {
                "utf8" | "utf-8" => s.as_bytes().to_vec(),
                "hex" => {
                    let chars: Vec<char> = s.chars().collect();
                    let mut out = Vec::with_capacity(chars.len() / 2);
                    let mut i = 0;
                    while i + 2 <= chars.len() {
                        let pair: String = chars[i..i + 2].iter().collect();
                        match i16::from_str_radix(&pair, 16) {
                            Ok(v) if (0..=255).contains(&v) => out.push(v as u8),
                            _ => break,
                        }
                        i += 2;
                    }
                    out
                }
                "base64" => {
                    let filtered: String =
                        s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '=').collect();
                    let trimmed = filtered.trim_end_matches('=');
                    LENIENT.decode(trimmed).unwrap_or_default()
                }
                "base64url" => {
                    let mut t = s.replace('-', "+").replace('_', "/");
                    while t.len() % 4 != 0 {
                        t.push('=');
                    }
                    STANDARD.decode(t).unwrap_or_default()
                }
                "binary" | "latin1" | "ascii" => s.chars().map(|c| (c as u32 & 0xff) as u8).collect(),
                "ucs2" | "ucs-2" | "utf16le" | "utf-16le" => s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect(),
                _ => return Err(format!("Unknown encoding: {}", encoding.unwrap_or(""))),
            })
        }
        None | Some(BV::Null) | Some(BV::Undefined) => Ok(Vec::new()),
        Some(other) => Err(format!("Unsupported crypto input {}", type_name(other))),
    }
}

fn type_name(v: &BV) -> &'static str {
    match v {
        BV::Bool(_) | BV::Number(_) => "__NSCFNumber",
        BV::Array(_) => "__NSArrayI",
        BV::Object(_) => "__NSDictionaryI",
        _ => "Any",
    }
}

/// `encode(data, encoding)`.
pub fn encode(data: Vec<u8>, encoding: Option<&str>) -> BV {
    let Some(enc) = encoding else { return BV::ArrayBuffer(data) };
    BV::String(match enc.to_lowercase().as_str() {
        "hex" => hex(&data),
        "base64" => STANDARD.encode(&data),
        "base64url" => URL_SAFE_NO_PAD.encode(&data),
        "binary" | "latin1" | "ascii" => data.iter().map(|&b| b as char).collect(),
        "ucs2" | "ucs-2" | "utf16le" | "utf-16le" => {
            if data.len() % 2 != 0 {
                String::new()
            } else {
                let units: Vec<u16> = data.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                String::from_utf16(&units).unwrap_or_default()
            }
        }
        _ => String::from_utf8_lossy(&data).into_owned(),
    })
}

pub fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

// MARK: Block cipher over the three key sizes

enum Aes {
    A128(aes::Aes128),
    A192(aes::Aes192),
    A256(aes::Aes256),
}

impl Aes {
    fn new(key: &[u8]) -> CryptoResult<Self> {
        Ok(match key.len() {
            16 => Aes::A128(aes::Aes128::new_from_slice(key).unwrap()),
            24 => Aes::A192(aes::Aes192::new_from_slice(key).unwrap()),
            32 => Aes::A256(aes::Aes256::new_from_slice(key).unwrap()),
            _ => return Err("Invalid key length".into()),
        })
    }
    fn encrypt(&self, block: &mut [u8]) {
        let b = GenericArray::from_mut_slice(block);
        match self {
            Aes::A128(c) => c.encrypt_block(b),
            Aes::A192(c) => c.encrypt_block(b),
            Aes::A256(c) => c.encrypt_block(b),
        }
    }
    fn decrypt(&self, block: &mut [u8]) {
        let b = GenericArray::from_mut_slice(block);
        match self {
            Aes::A128(c) => c.decrypt_block(b),
            Aes::A192(c) => c.decrypt_block(b),
            Aes::A256(c) => c.decrypt_block(b),
        }
    }
}

enum CtrStream {
    A128(ctr::Ctr128BE<aes::Aes128>),
    A192(ctr::Ctr128BE<aes::Aes192>),
    A256(ctr::Ctr128BE<aes::Aes256>),
}

impl CtrStream {
    fn new(key: &[u8], iv: &[u8]) -> CryptoResult<Self> {
        let e = |_| "Invalid key length".to_string();
        Ok(match key.len() {
            16 => CtrStream::A128(ctr::Ctr128BE::new_from_slices(key, iv).map_err(e)?),
            24 => CtrStream::A192(ctr::Ctr128BE::new_from_slices(key, iv).map_err(e)?),
            32 => CtrStream::A256(ctr::Ctr128BE::new_from_slices(key, iv).map_err(e)?),
            _ => return Err("Invalid key length".into()),
        })
    }
    fn apply(&mut self, data: &mut [u8]) {
        match self {
            CtrStream::A128(c) => c.apply_keystream(data),
            CtrStream::A192(c) => c.apply_keystream(data),
            CtrStream::A256(c) => c.apply_keystream(data),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Cbc,
    Ctr,
    Ecb,
    Gcm,
}

enum CipherEngine {
    Block { aes: Aes, chain: [u8; 16], pending: Vec<u8> },
    Ctr(CtrStream),
    Gcm,
}

/// A stateful cipher (`createCipheriv` / `createDecipheriv`).
pub struct CipherSession {
    pub encrypting: bool,
    algorithm: String,
    mode: Mode,
    key: Vec<u8>,
    iv: Vec<u8>,
    auto_padding: bool,
    engine: CipherEngine,
    buffered: Vec<u8>,
    pub auth_tag: Option<Vec<u8>>,
    pub aad: Vec<u8>,
    held_back: Vec<u8>,
}

impl CipherSession {
    pub fn new(encrypting: bool, algorithm: &str, key: Vec<u8>, iv: Vec<u8>) -> CryptoResult<Self> {
        let algorithm = algorithm.to_lowercase();
        let mode = if algorithm.ends_with("-cbc") {
            Mode::Cbc
        } else if algorithm.ends_with("-ctr") {
            Mode::Ctr
        } else if algorithm.ends_with("-ecb") {
            Mode::Ecb
        } else if algorithm.ends_with("-gcm") {
            Mode::Gcm
        } else {
            return Err(format!("Unsupported cipher {algorithm}"));
        };
        let mut s = CipherSession {
            encrypting,
            algorithm,
            mode,
            key,
            iv,
            auto_padding: true,
            engine: CipherEngine::Gcm,
            buffered: Vec::new(),
            auth_tag: None,
            aad: Vec::new(),
            held_back: Vec::new(),
        };
        if mode != Mode::Gcm {
            s.make_engine()?;
        }
        Ok(s)
    }

    pub fn is_gcm(&self) -> bool {
        self.mode == Mode::Gcm
    }

    fn strips_padding(&self) -> bool {
        !self.encrypting && self.auto_padding && matches!(self.mode, Mode::Cbc | Mode::Ecb)
    }

    fn make_engine(&mut self) -> CryptoResult<()> {
        let expected = self.algorithm.split('-').nth(1).and_then(|n| n.parse::<usize>().ok()).map(|n| n / 8).unwrap_or(self.key.len());
        if self.key.len() != expected {
            return Err("Invalid key length".into());
        }
        if self.mode != Mode::Ecb && self.iv.len() != 16 {
            return Err("Invalid initialization vector".into());
        }
        self.engine = match self.mode {
            Mode::Ctr => CipherEngine::Ctr(CtrStream::new(&self.key, &self.iv)?),
            _ => {
                let mut chain = [0u8; 16];
                if self.mode == Mode::Cbc {
                    chain.copy_from_slice(&self.iv);
                }
                CipherEngine::Block { aes: Aes::new(&self.key)?, chain, pending: Vec::new() }
            }
        };
        Ok(())
    }

    pub fn set_auto_padding(&mut self, on: bool) -> CryptoResult<()> {
        self.auto_padding = on;
        if !self.is_gcm() {
            self.held_back.clear();
            self.make_engine()?;
        }
        Ok(())
    }

    fn process_blocks(&mut self, input: &[u8]) -> Vec<u8> {
        let (encrypting, mode) = (self.encrypting, self.mode);
        let CipherEngine::Block { aes, chain, pending } = &mut self.engine else { return Vec::new() };
        pending.extend_from_slice(input);
        let full = pending.len() / 16 * 16;
        let mut out: Vec<u8> = pending.drain(..full).collect();
        for block in out.chunks_mut(16) {
            match (mode, encrypting) {
                (Mode::Cbc, true) => {
                    for (b, c) in block.iter_mut().zip(chain.iter()) {
                        *b ^= c;
                    }
                    aes.encrypt(block);
                    chain.copy_from_slice(block);
                }
                (Mode::Cbc, false) => {
                    let mut prev = [0u8; 16];
                    prev.copy_from_slice(block);
                    aes.decrypt(block);
                    for (b, c) in block.iter_mut().zip(chain.iter()) {
                        *b ^= c;
                    }
                    *chain = prev;
                }
                (_, true) => aes.encrypt(block),
                (_, false) => aes.decrypt(block),
            }
        }
        out
    }

    pub fn update(&mut self, input: &[u8]) -> CryptoResult<Vec<u8>> {
        if self.is_gcm() {
            self.buffered.extend_from_slice(input);
            return Ok(Vec::new());
        }
        let out = match &mut self.engine {
            CipherEngine::Ctr(c) => {
                let mut data = input.to_vec();
                c.apply(&mut data);
                data
            }
            CipherEngine::Block { .. } => self.process_blocks(input),
            CipherEngine::Gcm => Vec::new(),
        };
        if !self.strips_padding() {
            return Ok(out);
        }
        let mut available = std::mem::take(&mut self.held_back);
        available.extend_from_slice(&out);
        let keep = available.len().min(16);
        self.held_back = available.split_off(available.len() - keep);
        Ok(available)
    }

    pub fn finalize(&mut self) -> CryptoResult<Vec<u8>> {
        if self.is_gcm() {
            return self.finish_gcm();
        }
        let fail = || if self.encrypting { "cipher final failed".to_string() } else { "bad decrypt".to_string() };
        let out = match &mut self.engine {
            CipherEngine::Ctr(_) | CipherEngine::Gcm => Vec::new(),
            CipherEngine::Block { pending, .. } => {
                let pads = self.encrypting && self.auto_padding && matches!(self.mode, Mode::Cbc | Mode::Ecb);
                if pads {
                    let p = 16 - pending.len() % 16;
                    let mut block = std::mem::take(pending);
                    block.extend(std::iter::repeat_n(p as u8, p));
                    self.process_blocks(&block)
                } else if !pending.is_empty() {
                    return Err(fail());
                } else {
                    Vec::new()
                }
            }
        };
        if !self.strips_padding() {
            return Ok(out);
        }
        let mut block = std::mem::take(&mut self.held_back);
        block.extend_from_slice(&out);
        let pad = block.last().copied().unwrap_or(0) as usize;
        if block.len() != 16 || !(1..=16).contains(&pad) || !block[16 - pad..].iter().all(|&b| b as usize == pad) {
            return Err("bad decrypt".into());
        }
        block.truncate(16 - pad);
        Ok(block)
    }

    fn finish_gcm(&mut self) -> CryptoResult<Vec<u8>> {
        use aes_gcm::aead::{Aead, Payload};
        use aes_gcm::{AesGcm, aead::consts::U12};
        let auth_err = "Unsupported state or unable to authenticate data".to_string();
        if self.iv.len() != 12 {
            return Err("Invalid initialization vector".into());
        }
        let nonce = GenericArray::from_slice(&self.iv);
        macro_rules! run {
            ($cipher:ty) => {{
                let c = <AesGcm<$cipher, U12>>::new_from_slice(&self.key).map_err(|_| "Invalid key length".to_string())?;
                if self.encrypting {
                    let mut sealed =
                        c.encrypt(nonce, Payload { msg: &self.buffered, aad: &self.aad }).map_err(|_| "cipher final failed".to_string())?;
                    let tag = sealed.split_off(sealed.len() - 16);
                    self.auth_tag = Some(tag);
                    Ok(sealed)
                } else {
                    let Some(tag) = &self.auth_tag else { return Err(auth_err) };
                    let mut ct = self.buffered.clone();
                    ct.extend_from_slice(tag);
                    c.decrypt(nonce, Payload { msg: &ct, aad: &self.aad }).map_err(|_| auth_err)
                }
            }};
        }
        match self.key.len() {
            16 => run!(aes::Aes128),
            24 => run!(aes::Aes192),
            32 => run!(aes::Aes256),
            _ => Err("Invalid key length".into()),
        }
    }
}

pub fn digest(algorithm: &str, data: &[u8]) -> CryptoResult<Vec<u8>> {
    use sha2::Digest;
    Ok(match algorithm.to_lowercase().replace('-', "").as_str() {
        "md5" => md5::Md5::digest(data).to_vec(),
        "sha1" => sha1::Sha1::digest(data).to_vec(),
        "sha256" => sha2::Sha256::digest(data).to_vec(),
        "sha384" => sha2::Sha384::digest(data).to_vec(),
        "sha512" => sha2::Sha512::digest(data).to_vec(),
        _ => return Err(format!("Digest method not supported: {algorithm}")),
    })
}

pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut data = vec![0u8; n];
    getrandom::getrandom(&mut data).expect("system CSPRNG");
    data
}

/// Stateful handles (cipher and hash share one counter, increasing from 1).
#[derive(Default)]
pub struct CryptoState {
    sessions: HashMap<i64, CipherSession>,
    hashes: HashMap<i64, (String, Vec<u8>)>,
    next_handle: i64,
}

static STATE: Mutex<Option<CryptoState>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut CryptoState) -> R) -> R {
    let mut guard = STATE.lock().unwrap();
    let state = guard.get_or_insert_with(|| CryptoState { next_handle: 1, ..Default::default() });
    f(state)
}

fn next_handle(s: &mut CryptoState) -> i64 {
    let h = s.next_handle;
    s.next_handle += 1;
    h
}

/// `(data, algorithm, key, iv = zeros(16), encodings?, options?)` — keeps the IV-encoding quirk of the macOS implementation.
pub fn one_shot(encrypting: bool, args: &[BV]) -> CryptoResult<BV> {
    let empty = Default::default();
    let encodings = args.dict(4).unwrap_or(&empty);
    let enc = |k: &str| encodings.get(k).and_then(BV::as_str).map(str::to_string);
    let algorithm = args.string(1).unwrap_or_else(|| "aes-256-cbc".into());
    let key_encoding = enc("key");
    let key = bytes(args.get(2), key_encoding.as_deref().or(if args.string(2).is_some() { Some("utf8") } else { None }))?;
    let iv_arg = args.get(3).filter(|v| !v.is_nullish());
    let iv = match iv_arg {
        None => vec![0u8; 16],
        Some(v) => {
            let iv_enc = if encodings.contains_key("iv") { key_encoding.clone() } else { None };
            bytes(Some(v), Some(iv_enc.as_deref().unwrap_or("utf8")))?
        }
    };
    let input = bytes(args.first(), enc("input").as_deref())?;
    let mut session = CipherSession::new(encrypting, &algorithm, key, iv)?;
    let mut out = session.update(&input)?;
    out.extend(session.finalize()?);
    if !encrypting && std::env::var_os("OLAZ_DEBUG_RESPONSES").is_some() {
        if let Ok(serde_json::Value::Object(json)) = serde_json::from_slice::<serde_json::Value>(&out) {
            let data_keys: Vec<_> = json.get("data").and_then(|d| d.as_object()).map(|d| d.keys().cloned().collect()).unwrap_or_default();
            eprintln!(
                "[crypto] decrypted keys={:?} error_code={} data.keys={:?}",
                json.keys().collect::<Vec<_>>(),
                json.get("error_code").map(|v| v.to_string()).unwrap_or("-".into()),
                data_keys
            );
        }
    }
    Ok(encode(out, enc("output").as_deref()))
}

/// Dispatches one `$znode.crypto.*` sync member. `None` = not a crypto member.
pub fn handle(member: &str, args: &[BV]) -> Option<CryptoResult<BV>> {
    let a = args;
    Some(match member {
        "encrypt" => one_shot(true, a),
        "decrypt" => one_shot(false, a),
        "hash" => (|| {
            let data = bytes(a.first(), a.string(2).as_deref())?;
            Ok(encode(digest(&a.string(1).unwrap_or_else(|| "sha256".into()), &data)?, a.string(3).as_deref()))
        })(),
        "randomBytes" => Ok(BV::ArrayBuffer(random_bytes(a.int(0).unwrap_or(0).max(0) as usize))),
        "__cipherCreate" => (|| {
            let encrypting = a.string(0).as_deref() == Some("cipher");
            let key = bytes(a.get(2), Some("utf8"))?;
            let iv = bytes(a.get(3), Some("utf8"))?;
            let session = CipherSession::new(encrypting, &a.string(1).unwrap_or_default(), key, iv)?;
            Ok(with_state(|s| {
                let h = next_handle(s);
                s.sessions.insert(h, session);
                BV::int(h)
            }))
        })(),
        "__cipherUpdate" => with_state(|s| {
            let session = s.sessions.get_mut(&a.int(0).unwrap_or(0)).ok_or("cipher released")?;
            let out = session.update(&bytes(a.get(1), a.string(2).as_deref())?)?;
            Ok(encode(out, a.string(3).as_deref()))
        }),
        "__cipherFinal" => with_state(|s| {
            let h = a.int(0).unwrap_or(0);
            let session = s.sessions.get_mut(&h).ok_or("cipher released")?;
            let result = session.finalize();
            if !session.is_gcm() || !session.encrypting {
                s.sessions.remove(&h);
            }
            Ok(encode(result?, a.string(1).as_deref()))
        }),
        "__cipherOption" => with_state(|s| {
            let h = a.int(0).unwrap_or(0);
            let Some(session) = s.sessions.get_mut(&h) else { return Ok(BV::Undefined) };
            match a.string(1).as_deref() {
                Some("autoPadding") => session.set_auto_padding(a.bool(2).unwrap_or(true))?,
                Some("authTag") => session.auth_tag = Some(bytes(a.get(2), None)?),
                Some("aad") => session.aad = bytes(a.get(2), None)?,
                Some("getAuthTag") => {
                    let tag = session.auth_tag.clone();
                    s.sessions.remove(&h);
                    return Ok(tag.map(BV::ArrayBuffer).unwrap_or(BV::Undefined));
                }
                _ => {}
            }
            Ok(BV::Undefined)
        }),
        "__hashCreate" => Ok(with_state(|s| {
            let h = next_handle(s);
            s.hashes.insert(h, (a.string(0).unwrap_or_else(|| "sha256".into()), Vec::new()));
            BV::int(h)
        })),
        "__hashUpdate" => with_state(|s| {
            let data = bytes(a.get(1), a.string(2).as_deref())?;
            let entry = s.hashes.get_mut(&a.int(0).unwrap_or(0)).ok_or("Digest already called")?;
            entry.1.extend(data);
            Ok(BV::Undefined)
        }),
        "__hashDigest" => with_state(|s| {
            let (alg, data) = s.hashes.remove(&a.int(0).unwrap_or(0)).ok_or("Digest already called")?;
            Ok(encode(digest(&alg, &data)?, a.string(1).as_deref()))
        }),
        _ => return None,
    })
}

/// AES-256-CBC + PKCS#7 one-shot (`AESCBC.crypt`).
pub fn aes_cbc(input: &[u8], key: &[u8], iv: &[u8], encrypt: bool) -> CryptoResult<Vec<u8>> {
    let mut s = CipherSession::new(encrypt, "aes-256-cbc", key.to_vec(), iv.to_vec())?;
    let mut out = s.update(input)?;
    out.extend(s.finalize().map_err(|_| "bad decrypt".to_string())?);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn encs(pairs: &[(&str, &str)]) -> BV {
        BV::Object(pairs.iter().map(|(k, v)| (k.to_string(), BV::str(*v))).collect::<BTreeMap<_, _>>())
    }
    fn call(member: &str, args: Vec<BV>) -> CryptoResult<BV> {
        handle(member, &args).unwrap()
    }

    #[test]
    fn zcid_encrypt_matches_node() {
        let out = call(
            "encrypt",
            vec![
                BV::str("23,abc-imei,1792963077777"),
                BV::str("aes-256-cbc"),
                BV::str("3FC4F0D2AB50057BCE0D90D9187A22B1"),
                BV::Null,
                encs(&[("key", "utf-8"), ("input", "utf-8"), ("output", "hex")]),
            ],
        )
        .unwrap();
        assert_eq!(out, BV::str("a1a69db6a344da4d883865c637f979495dd86112a4534347b462259bfdeb641b"));
    }

    #[test]
    fn params_round_trip_base64() {
        let key = "DE71B7D4C57C0749158CB90D5E431AE3";
        let enc = call(
            "encrypt",
            vec![BV::str("{\"language\":\"vi\"}"), BV::str("aes-256-cbc"), BV::str(key), BV::Null, encs(&[("key", "utf-8"), ("input", "utf-8"), ("output", "base64")])],
        )
        .unwrap();
        assert_eq!(enc, BV::str("V5p1S6tTwaE91W8UDCagn9vVP9wUQ3QzA04wG7xh+5E="));
        let dec = call("decrypt", vec![enc, BV::str("aes-256-cbc"), BV::str(key), BV::Null, encs(&[("key", "utf-8"), ("input", "base64"), ("output", "utf-8")])]).unwrap();
        assert_eq!(dec, BV::str("{\"language\":\"vi\"}"));
    }

    #[test]
    fn streaming_ctr_and_gcm() {
        let ctr = call("__cipherCreate", vec![BV::str("cipher"), BV::str("aes-128-ctr"), BV::Bytes(vec![7; 16]), BV::Bytes(vec![1; 16])]).unwrap();
        let out = call("__cipherUpdate", vec![ctr, BV::str("hello zalo ctr mode!"), BV::str("utf8"), BV::str("hex")]).unwrap();
        assert_eq!(out, BV::str("324a84ef5fc3a21b11456b948b319aab1c6e924d"));

        let gcm = call("__cipherCreate", vec![BV::str("cipher"), BV::str("aes-256-gcm"), BV::Bytes(vec![3; 32]), BV::Bytes(vec![9; 12])]).unwrap();
        call("__cipherUpdate", vec![gcm.clone(), BV::str("gcm payload"), BV::str("utf8"), BV::Null]).unwrap();
        assert_eq!(call("__cipherFinal", vec![gcm.clone(), BV::str("hex")]).unwrap(), BV::str("636b00fbfe1c3505e56f0c"));
        let tag = call("__cipherOption", vec![gcm.clone(), BV::str("getAuthTag")]).unwrap();
        assert_eq!(tag, BV::ArrayBuffer(unhex("377ee9d6cfa423566629c0cfbb8eb885")));
        // released after getAuthTag
        assert_eq!(call("__cipherFinal", vec![gcm, BV::Null]), Err("cipher released".into()));
    }

    pub fn unhex(s: &str) -> Vec<u8> {
        bytes(Some(&BV::str(s)), Some("hex")).unwrap()
    }

    #[test]
    fn gcm_decrypt_with_tag() {
        let d = call("__cipherCreate", vec![BV::str("decipher"), BV::str("aes-256-gcm"), BV::Bytes(vec![3; 32]), BV::Bytes(vec![9; 12])]).unwrap();
        call("__cipherOption", vec![d.clone(), BV::str("authTag"), BV::Bytes(unhex("377ee9d6cfa423566629c0cfbb8eb885"))]).unwrap();
        call("__cipherUpdate", vec![d.clone(), BV::str("636b00fbfe1c3505e56f0c"), BV::str("hex"), BV::Null]).unwrap();
        assert_eq!(call("__cipherFinal", vec![d, BV::str("utf8")]).unwrap(), BV::str("gcm payload"));
        let bad = call("__cipherCreate", vec![BV::str("decipher"), BV::str("aes-256-gcm"), BV::Bytes(vec![3; 32]), BV::Bytes(vec![9; 12])]).unwrap();
        call("__cipherOption", vec![bad.clone(), BV::str("authTag"), BV::Bytes(vec![0; 16])]).unwrap();
        call("__cipherUpdate", vec![bad.clone(), BV::str("636b00fbfe1c3505e56f0c"), BV::str("hex"), BV::Null]).unwrap();
        assert!(call("__cipherFinal", vec![bad, BV::Null]).is_err());
        let notag = call("__cipherCreate", vec![BV::str("decipher"), BV::str("aes-128-gcm"), BV::Bytes(vec![3; 16]), BV::Bytes(vec![9; 12])]).unwrap();
        assert_eq!(call("__cipherFinal", vec![notag, BV::Null]), Err("Unsupported state or unable to authenticate data".into()));
    }

    #[test]
    fn hashes() {
        let h = call("__hashCreate", vec![BV::str("md5")]).unwrap();
        call("__hashUpdate", vec![h.clone(), BV::str("841630ae41f"), BV::Null]).unwrap();
        assert_eq!(call("__hashDigest", vec![h.clone(), BV::str("hex")]).unwrap(), BV::str("d9e97c18b27cd54e969268d9b39e8680"));
        assert_eq!(call("__hashDigest", vec![h, BV::str("hex")]), Err("Digest already called".into()));
        assert_eq!(
            call("hash", vec![BV::str("zalo"), BV::str("sha256"), BV::Null, BV::str("base64")]).unwrap(),
            BV::str("PpYUHCFwTpavYlfqQcD8aQzgUEyFKdxCTciyRbY0PA8=")
        );
        assert_eq!(call("hash", vec![BV::str("x"), BV::str("SHA-1"), BV::Null, BV::str("hex")]).unwrap(), BV::str("11f6ad8ec52a2984abaafd7c3b516503785c2072"));
        assert!(call("hash", vec![BV::str("x"), BV::str("whirlpool")]).unwrap_err().starts_with("Digest method not supported"));
    }

    #[test]
    fn bad_decrypt_throws() {
        let r = call(
            "decrypt",
            vec![BV::str("AAAAAAAAAAAAAAAAAAAAAA=="), BV::str("aes-256-cbc"), BV::str("DE71B7D4C57C0749158CB90D5E431AE3"), BV::Null, encs(&[("key", "utf-8"), ("input", "base64"), ("output", "utf-8")])],
        );
        assert_eq!(r, Err("bad decrypt".into()));
    }

    #[test]
    fn padding_strictness() {
        let key = vec![1u8; 32];
        let iv = vec![2u8; 16];
        for last in [0u8, 17] {
            let mut plain = vec![5u8; 16];
            plain[15] = last;
            let mut s = CipherSession::new(true, "aes-256-cbc", key.clone(), iv.clone()).unwrap();
            s.set_auto_padding(false).unwrap();
            let mut ct = s.update(&plain).unwrap();
            ct.extend(s.finalize().unwrap());
            assert_eq!(aes_cbc(&ct, &key, &iv, false), Err("bad decrypt".into()), "last={last}");
        }
        // inconsistent padding bytes
        let mut plain = vec![3u8; 16];
        plain[14] = 9;
        let mut s = CipherSession::new(true, "aes-256-cbc", key.clone(), iv.clone()).unwrap();
        s.set_auto_padding(false).unwrap();
        let ct = [s.update(&plain).unwrap(), s.finalize().unwrap()].concat();
        assert!(aes_cbc(&ct, &key, &iv, false).is_err());
    }

    #[test]
    fn streaming_cbc_lengths_round_trip() {
        let key = vec![9u8; 16];
        let iv = vec![4u8; 16];
        for len in [0usize, 1, 15, 16, 17, 33] {
            let plain: Vec<u8> = (0..len as u8).collect();
            let mut e = CipherSession::new(true, "aes-128-cbc", key.clone(), iv.clone()).unwrap();
            let mut ct = Vec::new();
            for chunk in plain.chunks(7) {
                ct.extend(e.update(chunk).unwrap());
            }
            ct.extend(e.finalize().unwrap());
            assert_eq!(ct.len(), (len / 16 + 1) * 16);
            let mut d = CipherSession::new(false, "aes-128-cbc", key.clone(), iv.clone()).unwrap();
            let mut pt = Vec::new();
            for chunk in ct.chunks(5) {
                pt.extend(d.update(chunk).unwrap());
            }
            pt.extend(d.finalize().unwrap());
            assert_eq!(pt, plain);
        }
        // no padding + partial block → final fails
        let mut e = CipherSession::new(true, "aes-128-ecb", key.clone(), vec![]).unwrap();
        e.set_auto_padding(false).unwrap();
        e.update(&[1, 2, 3]).unwrap();
        assert_eq!(e.finalize(), Err("cipher final failed".into()));
    }

    #[test]
    fn validation_errors() {
        assert_eq!(CipherSession::new(true, "aes-256-cbc", vec![0; 16], vec![0; 16]).err(), Some("Invalid key length".into()));
        assert_eq!(CipherSession::new(true, "aes-128-cbc", vec![0; 16], vec![0; 8]).err(), Some("Invalid initialization vector".into()));
        assert_eq!(CipherSession::new(true, "des-ede3", vec![0; 16], vec![0; 8]).err(), Some("Unsupported cipher des-ede3".into()));
        assert!(CipherSession::new(true, "AES-128-ECB", vec![0; 16], vec![]).is_ok());
        assert!(CipherSession::new(true, "aes-256-gcm", vec![0; 32], vec![0; 12]).is_ok());
        assert_eq!(call("__cipherOption", vec![BV::int(999_999), BV::str("aad"), BV::Null]).unwrap(), BV::Undefined);
        assert_eq!(call("__cipherUpdate", vec![BV::int(999_999), BV::str("x")]), Err("cipher released".into()));
    }

    #[test]
    fn iv_encoding_quirk_uses_key_encoding() {
        // encodings = {key:'hex', iv:'base64'} → the IV is decoded as **hex**.
        let key_hex = "00".repeat(32);
        let iv_hex = "01".repeat(16);
        let quirk = call(
            "encrypt",
            vec![BV::str("abc"), BV::str("aes-256-cbc"), BV::str(&key_hex), BV::str(&iv_hex), encs(&[("key", "hex"), ("iv", "base64"), ("output", "hex")])],
        )
        .unwrap();
        let expected = aes_cbc(b"abc", &[0; 32], &[1; 16], true).unwrap();
        assert_eq!(quirk, BV::String(hex(&expected)));
        // Without `iv` in encodings the IV string is utf8.
        let utf8_iv = "0123456789abcdef";
        let plain = call("encrypt", vec![BV::str("abc"), BV::str("aes-256-cbc"), BV::str(&key_hex), BV::str(utf8_iv), encs(&[("key", "hex"), ("output", "hex")])]).unwrap();
        assert_eq!(plain, BV::String(hex(&aes_cbc(b"abc", &[0; 32], utf8_iv.as_bytes(), true).unwrap())));
    }

    #[test]
    fn encodings() {
        assert_eq!(bytes(Some(&BV::str("0aZZff")), Some("hex")).unwrap(), vec![10]);
        assert_eq!(bytes(Some(&BV::str("abc")), Some("hex")).unwrap(), vec![0xab]);
        assert_eq!(bytes(Some(&BV::str("aGk=\n")), Some("base64")).unwrap(), b"hi");
        assert_eq!(bytes(Some(&BV::str("aGk")), Some("base64")).unwrap(), b"hi");
        assert_eq!(bytes(Some(&BV::str("-_8")), Some("base64url")).unwrap(), vec![0xfb, 0xff]);
        assert_eq!(bytes(Some(&BV::str("é")), Some("latin1")).unwrap(), vec![0xe9]);
        assert_eq!(bytes(Some(&BV::str("A")), Some("utf16le")).unwrap(), vec![0x41, 0]);
        assert_eq!(bytes(None, None).unwrap(), Vec::<u8>::new());
        assert!(bytes(Some(&BV::str("x")), Some("klingon")).unwrap_err().starts_with("Unknown encoding: klingon"));
        assert!(bytes(Some(&BV::int(1)), None).unwrap_err().starts_with("Unsupported crypto input"));
        assert_eq!(encode(vec![0xfb, 0xff], Some("base64url")), BV::str("-_8"));
        assert_eq!(encode(vec![0xe9], Some("binary")), BV::str("é"));
        assert_eq!(encode(vec![0x41, 0], Some("ucs2")), BV::str("A"));
        assert_eq!(encode(vec![0x41], Some("ucs2")), BV::str(""));
        assert_eq!(encode(vec![0xff], Some("utf8")), BV::str("\u{fffd}"));
        assert_eq!(encode(vec![1], None), BV::ArrayBuffer(vec![1]));
    }

    #[test]
    fn handles_share_one_counter() {
        let a = call("__hashCreate", vec![]).unwrap().as_int().unwrap();
        let b = call("__cipherCreate", vec![BV::str("cipher"), BV::str("aes-128-ecb"), BV::Bytes(vec![0; 16]), BV::Null]).unwrap().as_int().unwrap();
        assert!(b > a);
    }

    /// Node parity for every mode × padding × length (skipped when `node` is unavailable).
    #[test]
    fn node_parity_matrix() {
        let Some(node) = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("node")).find(|n| n.is_file())) else { return };
        let script = r#"
const c=require('crypto');const out=[];
for(const alg of ['aes-128-cbc','aes-256-cbc','aes-192-ecb','aes-256-ctr','aes-128-gcm']){
 for(const pad of [true,false]){ for(const len of [0,1,15,16,17,33]){
  const key=Buffer.alloc(parseInt(alg.split('-')[1])/8,7), iv=alg.endsWith('ecb')?null:Buffer.alloc(alg.endsWith('gcm')?12:16,3);
  const data=Buffer.from(Array.from({length:len},(_, i)=>i*7%256));
  let r;try{const e=c.createCipheriv(alg,key,iv);e.setAutoPadding(pad);r=Buffer.concat([e.update(data),e.final()]).toString('hex');if(alg.endsWith('gcm'))r+='|'+e.getAuthTag().toString('hex');}catch(x){r='ERR';}
  out.push([alg,pad,len,r]);}}}
console.log(JSON.stringify(out));"#;
        let out = std::process::Command::new(node).arg("-e").arg(script).output().unwrap();
        let rows: Vec<(String, bool, usize, String)> = serde_json::from_slice(&out.stdout).unwrap();
        for (alg, pad, len, expected) in rows {
            let bits: usize = alg.split('-').nth(1).unwrap().parse().unwrap();
            let key = vec![7u8; bits / 8];
            let iv = if alg.ends_with("ecb") { vec![] } else { vec![3u8; if alg.ends_with("gcm") { 12 } else { 16 }] };
            let data: Vec<u8> = (0..len).map(|i| (i * 7 % 256) as u8).collect();
            let mut s = CipherSession::new(true, &alg, key, iv).unwrap();
            s.set_auto_padding(pad).unwrap();
            let got = (|| -> CryptoResult<String> {
                let mut r = s.update(&data)?;
                r.extend(s.finalize()?);
                let mut h = hex(&r);
                if s.is_gcm() {
                    h.push('|');
                    h.push_str(&hex(s.auth_tag.as_ref().unwrap()));
                }
                Ok(h)
            })()
            .unwrap_or_else(|_| "ERR".into());
            assert_eq!(got, expected, "{alg} pad={pad} len={len}");
        }
    }
}

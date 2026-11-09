//! SecureStream: stream decryption for `e2ee` / `pcloud` downloads.

use aes::cipher::{BlockEncrypt, KeyInit, KeyIvInit, StreamCipher, generic_array::GenericArray};
use base64::Engine;
use ghash::universal_hash::UniversalHash;

type Ctr = ctr::Ctr128BE<aes::Aes256>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Algorithm {
    Ctr,
    Gcm,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SecureStream {
    pub algorithm: Algorithm,
    pub key: Vec<u8>,
    pub iv: Vec<u8>,
    pub tag: Option<Vec<u8>>,
    pub block: bool,
    pub block_size: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    /// No transformer requested: bytes saved as received.
    None,
    Stream(SecureStream),
    /// A decryptable type with unusable cryptography: the download must fail (never save ciphertext).
    Invalid,
}

fn bytes(v: Option<&serde_json::Value>) -> Option<Vec<u8>> {
    match v? {
        serde_json::Value::String(s) => base64::engine::general_purpose::STANDARD.decode(s).ok(),
        serde_json::Value::Array(a) => Some(a.iter().map(|n| n.as_f64().unwrap_or(0.0) as i64 as u8).collect()),
        serde_json::Value::Object(o) => o.get("__bytes").and_then(|b| b.as_str()).and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok()),
        _ => None,
    }
}

pub fn parse(json: Option<&serde_json::Value>) -> Parsed {
    let Some(d) = json.and_then(|j| j.as_object()) else { return Parsed::None };
    match d.get("type").and_then(|t| t.as_str()) {
        Some("e2ee") | Some("pcloud") => {}
        _ => return Parsed::None,
    }
    let Some(c) = d.get("cryptography").and_then(|c| c.as_object()) else { return Parsed::Invalid };
    let algorithm = match c.get("alg").and_then(|a| a.as_str()) {
        Some("aes-256-ctr") => Algorithm::Ctr,
        Some("aes-256-gcm") => Algorithm::Gcm,
        _ => return Parsed::Invalid,
    };
    let (Some(key), Some(iv)) = (bytes(c.get("key")), bytes(c.get("iv"))) else { return Parsed::Invalid };
    if key.len() != 32 || iv.is_empty() {
        return Parsed::Invalid;
    }
    let tag = bytes(c.get("tag"));
    if algorithm == Algorithm::Gcm && tag.as_ref().map(Vec::len) != Some(16) {
        return Parsed::Invalid;
    }
    if algorithm == Algorithm::Ctr && iv.len() != 16 {
        return Parsed::Invalid;
    }
    let block = c.get("type").and_then(|t| t.as_str()) == Some("block");
    if algorithm == Algorithm::Gcm && block {
        return Parsed::Invalid;
    }
    let block_size = c.get("blockSize").and_then(|b| b.as_f64()).map(|b| b as i64).unwrap_or(4096).max(16) as usize;
    Parsed::Stream(SecureStream { algorithm, key, iv, tag, block, block_size })
}

#[derive(Debug)]
pub struct DecryptError;

fn ctr_apply(key: &[u8], iv: &[u8], data: &mut [u8]) {
    let mut c = Ctr::new_from_slices(key, iv).expect("key/iv sizes checked");
    c.apply_keystream(data);
}

/// Streaming AES-256-GCM decryption for any IV length (NIST SP 800-38D, empty AAD).
pub struct GcmStream {
    ctr: Ctr,
    ghash: ghash::GHash,
    tag_mask: [u8; 16],
    length: u64,
    pending: Vec<u8>,
}

impl GcmStream {
    pub fn new(key: &[u8], iv: &[u8]) -> Result<Self, DecryptError> {
        let cipher = aes::Aes256::new_from_slice(key).map_err(|_| DecryptError)?;
        let mut h = GenericArray::from([0u8; 16]);
        cipher.encrypt_block(&mut h);
        let j0: [u8; 16] = if iv.len() == 12 {
            let mut j = [0u8; 16];
            j[..12].copy_from_slice(iv);
            j[15] = 1;
            j
        } else {
            let mut g = ghash::GHash::new(&h);
            g.update_padded(iv);
            let mut lens = [0u8; 16];
            lens[8..].copy_from_slice(&((iv.len() as u64) * 8).to_be_bytes());
            g.update(&[GenericArray::from(lens)]);
            g.finalize().into()
        };
        let mut mask = GenericArray::from(j0);
        cipher.encrypt_block(&mut mask);
        let mut counter = j0;
        let c = u32::from_be_bytes(counter[12..].try_into().unwrap()).wrapping_add(1);
        counter[12..].copy_from_slice(&c.to_be_bytes());
        // GCM increments only the low 32 bits; Ctr128BE increments 128 bits — identical below 2^32 blocks (64 GiB).
        let ctr = Ctr::new_from_slices(key, &counter).map_err(|_| DecryptError)?;
        Ok(Self { ctr, ghash: ghash::GHash::new(&h), tag_mask: mask.into(), length: 0, pending: Vec::new() })
    }

    pub fn update(&mut self, ciphertext: &[u8]) -> Vec<u8> {
        self.length += ciphertext.len() as u64;
        self.pending.extend_from_slice(ciphertext);
        let whole = self.pending.len() / 16 * 16;
        if whole > 0 {
            let blocks: Vec<GenericArray<u8, _>> = self.pending[..whole].chunks(16).map(GenericArray::clone_from_slice).collect();
            self.ghash.update(&blocks);
            self.pending.drain(..whole);
        }
        let mut out = ciphertext.to_vec();
        self.ctr.apply_keystream(&mut out);
        out
    }

    pub fn verify(mut self, tag: &[u8]) -> bool {
        if !self.pending.is_empty() {
            let p = std::mem::take(&mut self.pending);
            self.ghash.update_padded(&p);
        }
        let mut lens = [0u8; 16];
        lens[8..].copy_from_slice(&(self.length * 8).to_be_bytes());
        self.ghash.update(&[GenericArray::from(lens)]);
        let digest: [u8; 16] = self.ghash.finalize().into();
        let mut diff = 0u8;
        for i in 0..16 {
            diff |= digest[i] ^ self.tag_mask[i] ^ tag.get(i).copied().unwrap_or(0);
        }
        diff == 0 && tag.len() == 16
    }
}

impl SecureStream {
    /// Whole-buffer decryption (photos).
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, DecryptError> {
        match self.algorithm {
            Algorithm::Ctr => {
                let mut out = data.to_vec();
                if !self.block {
                    ctr_apply(&self.key, &self.iv, &mut out);
                } else {
                    let whole = out.len() / self.block_size * self.block_size;
                    for chunk in out[..whole].chunks_mut(self.block_size) {
                        ctr_apply(&self.key, &self.iv, chunk);
                    }
                    // a trailing partial block passes through unchanged
                }
                Ok(out)
            }
            Algorithm::Gcm => {
                let tag = self.tag.as_ref().ok_or(DecryptError)?;
                let mut g = GcmStream::new(&self.key, &self.iv)?;
                let out = g.update(data);
                if g.verify(tag) { Ok(out) } else { Err(DecryptError) }
            }
        }
    }

    /// Streams `source` → `destination` (removed on failure); GCM verifies before returning.
    pub fn decrypt_file(&self, source: &std::path::Path, destination: &std::path::Path) -> Result<(), DecryptError> {
        use std::io::{Read, Write};
        let result = (|| -> Result<(), DecryptError> {
            let mut input = std::fs::File::open(source).map_err(|_| DecryptError)?;
            let mut output = std::fs::File::create(destination).map_err(|_| DecryptError)?;
            let chunk = if self.block { self.block_size * (1 << 20) / self.block_size.max(1) } else { 1 << 20 };
            let chunk = chunk.max(self.block_size);
            let mut buf = vec![0u8; chunk];
            let mut gcm = match self.algorithm {
                Algorithm::Gcm => Some(GcmStream::new(&self.key, &self.iv)?),
                Algorithm::Ctr => None,
            };
            let mut linear = (self.algorithm == Algorithm::Ctr && !self.block).then(|| Ctr::new_from_slices(&self.key, &self.iv).unwrap());
            loop {
                // fill the whole chunk so block boundaries line up
                let mut n = 0;
                while n < buf.len() {
                    let r = input.read(&mut buf[n..]).map_err(|_| DecryptError)?;
                    if r == 0 {
                        break;
                    }
                    n += r;
                }
                if n == 0 {
                    break;
                }
                let part = &mut buf[..n];
                let out = match (&mut gcm, &mut linear) {
                    (Some(g), _) => g.update(part),
                    (None, Some(c)) => {
                        c.apply_keystream(part);
                        part.to_vec()
                    }
                    (None, None) => self.decrypt(part)?,
                };
                output.write_all(&out).map_err(|_| DecryptError)?;
                if n < buf.len() {
                    break;
                }
            }
            if let Some(g) = gcm {
                if !g.verify(self.tag.as_deref().unwrap_or(&[])) {
                    return Err(DecryptError);
                }
            }
            output.sync_all().map_err(|_| DecryptError)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(destination);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::aead::{Aead, AeadCore, Payload};
    use serde_json::json;

    #[test]
    fn parse_rules() {
        let k = base64::engine::general_purpose::STANDARD.encode([1u8; 32]);
        let iv = base64::engine::general_purpose::STANDARD.encode([2u8; 16]);
        assert_eq!(parse(None), Parsed::None);
        assert_eq!(parse(Some(&json!({"type": "plain"}))), Parsed::None);
        assert_eq!(parse(Some(&json!({"type": "e2ee", "cryptography": {"alg": "AES-GCM", "key": k, "iv": iv}}))), Parsed::Invalid);
        assert_eq!(parse(Some(&json!({"type": "e2ee", "cryptography": {"alg": "aes-256-gcm", "key": k, "iv": iv}}))), Parsed::Invalid, "gcm needs a tag");
        let Parsed::Stream(s) = parse(Some(&json!({"type": "pcloud", "cryptography": {"alg": "aes-256-ctr", "key": k, "iv": iv, "type": "block", "blockSize": 8}}))) else { panic!() };
        assert!(s.block);
        assert_eq!(s.block_size, 16);
        assert_eq!(parse(Some(&json!({"type": "e2ee", "cryptography": {"alg": "aes-256-ctr", "key": [1, 2], "iv": iv}}))), Parsed::Invalid);
    }

    #[test]
    fn ctr_block_mode_passes_overflow_through() {
        let s = SecureStream { algorithm: Algorithm::Ctr, key: vec![3; 32], iv: vec![4; 16], tag: None, block: true, block_size: 16 };
        let plain: Vec<u8> = (0..40u8).collect();
        // encrypt = decrypt for CTR
        let enc = s.decrypt(&plain).unwrap();
        assert_eq!(&enc[32..], &plain[32..], "partial trailing block unchanged");
        assert_eq!(s.decrypt(&enc).unwrap(), plain);
        // each block restarts the cipher: block 2 encrypts like block 1 would
        let one = s.decrypt(&plain[16..32]).unwrap();
        assert_eq!(&enc[16..32], &one[..]);
    }

    #[test]
    fn gcm_stream_equals_one_shot() {
        let key = [9u8; 32];
        for iv_len in [12usize, 16] {
            for len in [0usize, 1, 15, 16, 17, 1000, 70_000] {
                let iv: Vec<u8> = (0..iv_len as u8).collect();
                let plain: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
                let sealed = if iv_len == 12 {
                    aes_gcm::Aes256Gcm::new_from_slice(&key).unwrap().encrypt(GenericArray::from_slice(&iv), Payload { msg: &plain, aad: &[] }).unwrap()
                } else {
                    type G16 = aes_gcm::AesGcm<aes::Aes256, aes_gcm::aead::consts::U16>;
                    let _ = <G16 as AeadCore>::NonceSize::default();
                    G16::new_from_slice(&key).unwrap().encrypt(GenericArray::from_slice(&iv), Payload { msg: &plain, aad: &[] }).unwrap()
                };
                let (ct, tag) = sealed.split_at(sealed.len() - 16);
                let s = SecureStream { algorithm: Algorithm::Gcm, key: key.to_vec(), iv: iv.clone(), tag: Some(tag.to_vec()), block: false, block_size: 4096 };
                assert_eq!(s.decrypt(ct).unwrap(), plain, "iv={iv_len} len={len}");
                // streamed in odd chunks
                let mut g = GcmStream::new(&key, &iv).unwrap();
                let mut out = Vec::new();
                for c in ct.chunks(7) {
                    out.extend(g.update(c));
                }
                assert!(g.verify(tag));
                assert_eq!(out, plain);
                let mut bad = tag.to_vec();
                bad[0] ^= 1;
                let s_bad = SecureStream { tag: Some(bad), ..s };
                assert!(s_bad.decrypt(ct).is_err());
            }
        }
    }

    #[test]
    fn decrypt_file_round_trip_and_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let s = SecureStream { algorithm: Algorithm::Ctr, key: vec![5; 32], iv: vec![6; 16], tag: None, block: false, block_size: 4096 };
        let plain: Vec<u8> = (0..3_000_000u32).map(|i| i as u8).collect();
        let enc = s.decrypt(&plain).unwrap();
        std::fs::write(dir.path().join("in"), &enc).unwrap();
        s.decrypt_file(&dir.path().join("in"), &dir.path().join("out")).unwrap();
        assert_eq!(std::fs::read(dir.path().join("out")).unwrap(), plain);
        let g = SecureStream { algorithm: Algorithm::Gcm, key: vec![5; 32], iv: vec![6; 12], tag: Some(vec![0; 16]), block: false, block_size: 4096 };
        assert!(g.decrypt_file(&dir.path().join("in"), &dir.path().join("out2")).is_err());
        assert!(!dir.path().join("out2").exists(), "unauthenticated output removed");
    }
}

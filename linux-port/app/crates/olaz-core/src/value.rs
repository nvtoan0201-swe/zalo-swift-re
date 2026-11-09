//! Values crossing the `$z*` bridge. Binary travels as `{__bytes: base64}`.

use std::collections::BTreeMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Number, Value};

#[derive(Debug, Clone, PartialEq, Default)]
pub enum BV {
    /// Swift `nil` → `{"__undefined": true}`.
    #[default]
    Undefined,
    /// JSON `null` / `NSNull` — kept distinct from `Undefined`.
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Bytes(Vec<u8>),
    /// Bytes returned as an `ArrayBuffer` (`{__bytes, __arraybuffer: true}`).
    ArrayBuffer(Vec<u8>),
    Array(Vec<BV>),
    Object(BTreeMap<String, BV>),
}

impl BV {
    /// `fromJS`: an object with a string `__bytes` → bytes (bad base64 → empty); recurse otherwise.
    pub fn from_js(value: Value) -> BV {
        match value {
            Value::Null => BV::Null,
            Value::Bool(b) => BV::Bool(b),
            Value::Number(n) => BV::Number(n),
            Value::String(s) => BV::String(s),
            Value::Array(a) => BV::Array(a.into_iter().map(BV::from_js).collect()),
            Value::Object(o) => {
                if let Some(Value::String(b64)) = o.get("__bytes") {
                    return BV::Bytes(STANDARD.decode(b64).unwrap_or_default());
                }
                BV::Object(o.into_iter().map(|(k, v)| (k, BV::from_js(v))).collect())
            }
        }
    }

    /// `toJS`.
    pub fn to_js(&self) -> Value {
        match self {
            BV::Undefined => undefined_marker(),
            BV::Null => Value::Null,
            BV::Bool(b) => Value::Bool(*b),
            BV::Number(n) => Value::Number(n.clone()),
            BV::String(s) => Value::String(s.clone()),
            BV::Bytes(b) => {
                let mut m = Map::new();
                m.insert("__bytes".into(), Value::String(STANDARD.encode(b)));
                Value::Object(m)
            }
            BV::ArrayBuffer(b) => {
                let mut m = Map::new();
                m.insert("__bytes".into(), Value::String(STANDARD.encode(b)));
                m.insert("__arraybuffer".into(), Value::Bool(true));
                Value::Object(m)
            }
            BV::Array(a) => Value::Array(a.iter().map(BV::to_js).collect()),
            BV::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), v.to_js())).collect()),
        }
    }

    /// Plain JSON (no bridge conversion) — for values that are already JSON-safe.
    pub fn json(value: Value) -> BV {
        match value {
            Value::Null => BV::Null,
            Value::Bool(b) => BV::Bool(b),
            Value::Number(n) => BV::Number(n),
            Value::String(s) => BV::String(s),
            Value::Array(a) => BV::Array(a.into_iter().map(BV::json).collect()),
            Value::Object(o) => BV::Object(o.into_iter().map(|(k, v)| (k, BV::json(v))).collect()),
        }
    }

    /// Converts to plain JSON for storage: bytes become `{__bytes}` like `toJS`, undefined → null.
    pub fn to_plain_json(&self) -> Value {
        match self {
            BV::Undefined => Value::Null,
            BV::Array(a) => Value::Array(a.iter().map(BV::to_plain_json).collect()),
            BV::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), v.to_plain_json())).collect()),
            other => other.to_js(),
        }
    }

    pub fn str(s: impl Into<String>) -> BV {
        BV::String(s.into())
    }
    pub fn int(i: i64) -> BV {
        BV::Number(i.into())
    }
    pub fn float(f: f64) -> BV {
        Number::from_f64(f).map(BV::Number).unwrap_or(BV::Null)
    }
    pub fn obj<I: IntoIterator<Item = (K, BV)>, K: Into<String>>(items: I) -> BV {
        BV::Object(items.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            BV::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_object(&self) -> Option<&BTreeMap<String, BV>> {
        match self {
            BV::Object(o) => Some(o),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&Vec<BV>> {
        match self {
            BV::Array(a) => Some(a),
            _ => None,
        }
    }
    /// `NSNumber.intValue` semantics: numbers truncate, booleans are 0/1.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            BV::Number(n) => n.as_i64().or_else(|| n.as_u64().map(|u| u as i64)).or_else(|| n.as_f64().map(|f| f as i64)),
            BV::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            BV::Number(n) => n.as_f64(),
            BV::Bool(b) => Some(*b as i64 as f64),
            _ => None,
        }
    }
    /// `NSNumber.boolValue`: numbers ≠ 0 are true.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            BV::Bool(b) => Some(*b),
            BV::Number(n) => n.as_f64().map(|f| f != 0.0),
            _ => None,
        }
    }
    pub fn get(&self, key: &str) -> Option<&BV> {
        self.as_object().and_then(|o| o.get(key))
    }
    pub fn is_nullish(&self) -> bool {
        matches!(self, BV::Null | BV::Undefined)
    }
}

pub fn undefined_marker() -> Value {
    let mut m = Map::new();
    m.insert("__undefined".into(), Value::Bool(true));
    Value::Object(m)
}

impl From<&str> for BV {
    fn from(s: &str) -> Self {
        BV::String(s.into())
    }
}
impl From<String> for BV {
    fn from(s: String) -> Self {
        BV::String(s)
    }
}
impl From<bool> for BV {
    fn from(b: bool) -> Self {
        BV::Bool(b)
    }
}
impl From<i64> for BV {
    fn from(i: i64) -> Self {
        BV::int(i)
    }
}
impl From<i32> for BV {
    fn from(i: i32) -> Self {
        BV::int(i as i64)
    }
}
impl From<usize> for BV {
    fn from(i: usize) -> Self {
        BV::int(i as i64)
    }
}
impl<T: Into<BV>> From<Vec<T>> for BV {
    fn from(v: Vec<T>) -> Self {
        BV::Array(v.into_iter().map(Into::into).collect())
    }
}
impl<T: Into<BV>> From<Option<T>> for BV {
    fn from(v: Option<T>) -> Self {
        v.map(Into::into).unwrap_or(BV::Undefined)
    }
}

/// Argument helpers: out of range → `None`.
pub trait Args {
    fn arg(&self, i: usize) -> Option<&BV>;
    fn string(&self, i: usize) -> Option<String> {
        self.arg(i).and_then(BV::as_str).map(str::to_string)
    }
    fn dict(&self, i: usize) -> Option<&BTreeMap<String, BV>> {
        self.arg(i).and_then(BV::as_object)
    }
    fn int(&self, i: usize) -> Option<i64> {
        self.arg(i).and_then(BV::as_int)
    }
    fn bool(&self, i: usize) -> Option<bool> {
        self.arg(i).and_then(BV::as_bool)
    }
    /// Bytes, or a string as UTF-8 bytes.
    fn data(&self, i: usize) -> Option<Vec<u8>> {
        match self.arg(i)? {
            BV::Bytes(b) | BV::ArrayBuffer(b) => Some(b.clone()),
            BV::String(s) => Some(s.as_bytes().to_vec()),
            _ => None,
        }
    }
}

impl Args for [BV] {
    fn arg(&self, i: usize) -> Option<&BV> {
        self.get(i)
    }
}
impl Args for Vec<BV> {
    fn arg(&self, i: usize) -> Option<&BV> {
        self.get(i)
    }
}

/// JS `String(n)` for a JSON number: integers without `.0` (the shared worker's key format).
pub fn js_number_string(n: &Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    let f = n.as_f64().unwrap_or(0.0);
    if f.fract() == 0.0 && f.abs() < 1e21 { format!("{}", f as i64) } else { format!("{f}") }
}

/// `"\(x)"` of a bridge value as the routers use it: strings as is, numbers like JS, null → "null".
pub fn interpolate(v: &BV) -> String {
    match v {
        BV::String(s) => s.clone(),
        BV::Number(n) => js_number_string(n),
        // NSNumber(bool) / NSNull descriptions, as Swift string interpolation prints them.
        BV::Bool(b) => if *b { "1".into() } else { "0".into() },
        BV::Null | BV::Undefined => "<null>".into(),
        other => other.to_js().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn to_js_rows() {
        assert_eq!(BV::Undefined.to_js(), json!({"__undefined": true}));
        assert_eq!(BV::Null.to_js(), Value::Null);
        assert_eq!(BV::Bytes(b"hi".to_vec()).to_js(), json!({"__bytes": "aGk="}));
        assert_eq!(BV::ArrayBuffer(b"hi".to_vec()).to_js(), json!({"__bytes": "aGk=", "__arraybuffer": true}));
        assert_eq!(BV::obj([("a", BV::Undefined), ("b", BV::int(1))]).to_js(), json!({"a": {"__undefined": true}, "b": 1}));
        assert_eq!(BV::Array(vec![BV::Undefined, BV::str("x")]).to_js(), json!([{"__undefined": true}, "x"]));
        assert_eq!(BV::float(1.5).to_js(), json!(1.5));
    }

    #[test]
    fn from_js_rows() {
        assert_eq!(BV::from_js(json!({"__bytes": "aGk="})), BV::Bytes(b"hi".to_vec()));
        assert_eq!(BV::from_js(json!({"__bytes": "!!!"})), BV::Bytes(vec![]));
        assert_eq!(BV::from_js(json!({"x": [{"__bytes": ""}]})), BV::obj([("x", BV::Array(vec![BV::Bytes(vec![])]))]));
        assert_eq!(BV::from_js(json!(null)), BV::Null);
        assert_eq!(BV::from_js(json!({"__bytes": 5})), BV::obj([("__bytes", BV::int(5))]));
    }

    #[test]
    fn round_trip() {
        let v = BV::obj([("d", BV::Bytes(vec![0, 255])), ("n", BV::Null), ("s", BV::str("é"))]);
        assert_eq!(BV::from_js(v.to_js()), v);
    }

    #[test]
    fn arg_helpers() {
        let args = vec![BV::str("s"), BV::float(2.9), BV::Bool(true), BV::Bytes(vec![1]), BV::obj([("k", BV::int(1))])];
        assert_eq!(args.string(0).as_deref(), Some("s"));
        assert_eq!(args.string(1), None);
        assert_eq!(args.int(1), Some(2));
        assert_eq!(args.int(2), Some(1));
        assert_eq!(args.bool(1), Some(true));
        assert_eq!(args.data(0), Some(b"s".to_vec()));
        assert_eq!(args.data(3), Some(vec![1]));
        assert_eq!(args.data(1), None);
        assert!(args.dict(4).is_some());
        assert_eq!(args.string(9), None);
    }

    #[test]
    fn number_strings() {
        assert_eq!(js_number_string(&Number::from(5)), "5");
        assert_eq!(js_number_string(&Number::from_f64(5.0).unwrap()), "5");
        assert_eq!(js_number_string(&Number::from_f64(5.5).unwrap()), "5.5");
        assert_eq!(interpolate(&BV::str("7")), "7");
    }
}

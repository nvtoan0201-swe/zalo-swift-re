//! IPC between the app and `olaz-webhost` (01 §2): `u32 LE length` + JSON object, one ordered stream.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bumped together by app and webhost; the app refuses a webhost reporting another build (05 §6).
pub const BUILD_ID: &str = concat!(env!("CARGO_PKG_VERSION"), "+ipc1");

/// Max frame size (the renderer can move tens of MB of base64 for media).
pub const MAX_FRAME: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum AppToHost {
    /// Create a view: scripts in injection order (the spec script is already inside `scripts`).
    Create { view: u32, ua: String, scripts: Vec<String>, debug: bool },
    Load { view: u32, url: String },
    /// Run an async function body with optional named arguments; answered by `evalResult`.
    Eval { view: u32, token: u64, body: String, #[serde(default)] args: Option<Value> },
    /// Fire-and-forget page call: an async function body with named arguments.
    Call { view: u32, body: String, args: Value },
    Reply { view: u32, id: u64, #[serde(default)] value: Option<Value>, #[serde(default)] error: Option<String> },
    /// Answer to a `sync` request (UTF-8 JSON body).
    SyncReply { view: u32, id: u64, body: String },
    /// Files for the next file chooser of the view; `None` clears the pending pick.
    Pick { view: u32, files: Option<Vec<String>> },
    Shutdown { view: u32 },
    Debug { view: u32 },
    /// Run JavaScript GC / memory pressure.
    Gc,
    Quit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum HostToApp {
    Hello { build: String, webkit: String, platform: String },
    /// `zlog` / `zstate` (no reply).
    Msg { view: u32, handler: String, body: Value },
    /// `znet` / `zbridge` (needs `reply`).
    Request { view: u32, handler: String, id: u64, body: Value },
    /// Blocking bridge-sync; the scheme request is finished when `syncReply` arrives.
    Sync { view: u32, id: u64, body: String },
    EvalResult { view: u32, token: u64, #[serde(default)] value: Option<Value>, #[serde(default)] error: Option<String> },
    Page { view: u32, event: String, #[serde(default)] error: Option<String>, #[serde(default)] url: Option<String> },
    /// The page opened a file chooser; `taken` = it was answered from the pending pick.
    Chooser { view: u32, multiple: bool, taken: usize },
    /// A main-frame http(s) navigation the host blocked: the app opens it in the system browser.
    OpenExternal { view: u32, url: String },
    NotFound { view: u32, path: String },
    Log { line: String },
    Crashed { view: u32, reason: String },
}

pub fn write_frame<W: Write, T: Serialize>(w: &mut W, frame: &T) -> io::Result<()> {
    let data = serde_json::to_vec(frame).map_err(io::Error::other)?;
    w.write_all(&(data.len() as u32).to_le_bytes())?;
    w.write_all(&data)?;
    w.flush()
}

/// Reads one frame; `Ok(None)` on a clean EOF at a frame boundary.
pub fn read_frame<R: Read, T: for<'de> Deserialize<'de>>(r: &mut R) -> io::Result<Option<T>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame too large: {len}")));
    }
    let mut data = vec![0u8; len];
    r.read_exact(&mut data)?;
    serde_json::from_slice(&data).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_in_order() {
        let frames = vec![
            HostToApp::Hello { build: BUILD_ID.into(), webkit: "2.52".into(), platform: "gtk".into() },
            HostToApp::Request { view: 1, handler: "znet".into(), id: 7, body: json!({"op": "http"}) },
            HostToApp::Sync { view: 1, id: 8, body: "{}".into() },
        ];
        let mut buf = Vec::new();
        for f in &frames {
            write_frame(&mut buf, f).unwrap();
        }
        let mut r = &buf[..];
        let mut got = Vec::new();
        while let Some(f) = read_frame::<_, HostToApp>(&mut r).unwrap() {
            got.push(f);
        }
        assert_eq!(got, frames);
    }

    #[test]
    fn tags() {
        let v = serde_json::to_value(AppToHost::SyncReply { view: 2, id: 3, body: "{}".into() }).unwrap();
        assert_eq!(v, json!({"t": "syncReply", "view": 2, "id": 3, "body": "{}"}));
        let e: HostToApp = serde_json::from_value(json!({"t": "evalResult", "view": 1, "token": 4})).unwrap();
        assert_eq!(e, HostToApp::EvalResult { view: 1, token: 4, value: None, error: None });
    }

    #[test]
    fn truncated_frame_is_an_error() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &AppToHost::Gc).unwrap();
        buf.pop();
        assert!(read_frame::<_, AppToHost>(&mut &buf[..]).is_err());
        assert!(read_frame::<_, AppToHost>(&mut &[][..]).unwrap().is_none());
    }
}

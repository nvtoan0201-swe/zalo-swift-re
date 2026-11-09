//! Video / audio playback: GStreamer `playbin` → `appsink` BGRA frames. GStreamer is loaded at run time
//! (`dlopen`), so building needs no GStreamer development files and a system without it degrades to "open with another app".
//! Only a handful of stable C entry points are used.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

type Ptr = *mut c_void;

const GST_STATE_NULL: c_int = 1;
const GST_STATE_PAUSED: c_int = 3;
const GST_STATE_PLAYING: c_int = 4;
const GST_FORMAT_TIME: c_int = 3;
const GST_SEEK_FLAG_FLUSH: c_int = 1 << 0;
const GST_SEEK_FLAG_ACCURATE: c_int = 1 << 1;
const GST_MESSAGE_EOS: u32 = 1 << 0;
const GST_MESSAGE_ERROR: u32 = 1 << 1;
const GST_MESSAGE_ELEMENT: u32 = 1 << 15;
const GST_MAP_READ: u32 = 1;

#[repr(C)]
struct GstMapInfo {
    memory: Ptr,
    flags: u32,
    data: *mut u8,
    size: usize,
    maxsize: usize,
    user_data: [Ptr; 4],
    reserved: [Ptr; 4],
}

#[repr(C)]
struct GError {
    domain: u32,
    code: c_int,
    message: *mut c_char,
}

/// GstMessage starts with a GstMiniObject (type, refcount, lockstate, flags, 4 fn pointers, priv data) then `type`.
#[repr(C)]
struct GstMessageHead {
    mini_type: usize,
    refcount: c_int,
    lockstate: c_int,
    flags: u32,
    copy: Ptr,
    dispose: Ptr,
    free: Ptr,
    priv_uint: u32,
    priv_pointer: Ptr,
    msg_type: u32,
}

struct Api {
    _core: libloading::Library,
    _app: libloading::Library,
    _gobject: libloading::Library,
    factory_make: unsafe extern "C" fn(*const c_char, *const c_char) -> Ptr,
    caps_from_string: unsafe extern "C" fn(*const c_char) -> Ptr,
    object_set: unsafe extern "C" fn(Ptr, *const c_char, ...),
    init_check: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char, *mut *mut GError) -> c_int,
    parse_launch: unsafe extern "C" fn(*const c_char, *mut *mut GError) -> Ptr,
    bin_get_by_name: unsafe extern "C" fn(Ptr, *const c_char) -> Ptr,
    set_state: unsafe extern "C" fn(Ptr, c_int) -> c_int,
    query_position: unsafe extern "C" fn(Ptr, c_int, *mut i64) -> c_int,
    query_duration: unsafe extern "C" fn(Ptr, c_int, *mut i64) -> c_int,
    seek_simple: unsafe extern "C" fn(Ptr, c_int, c_int, i64) -> c_int,
    get_bus: unsafe extern "C" fn(Ptr) -> Ptr,
    bus_timed_pop_filtered: unsafe extern "C" fn(Ptr, u64, u32) -> Ptr,
    message_parse_error: unsafe extern "C" fn(Ptr, *mut *mut GError, *mut *mut c_char),
    mini_object_unref: unsafe extern "C" fn(Ptr),
    object_unref: unsafe extern "C" fn(Ptr),
    sample_get_buffer: unsafe extern "C" fn(Ptr) -> Ptr,
    sample_get_caps: unsafe extern "C" fn(Ptr) -> Ptr,
    caps_get_structure: unsafe extern "C" fn(Ptr, u32) -> Ptr,
    structure_get_int: unsafe extern "C" fn(Ptr, *const c_char, *mut c_int) -> c_int,
    message_get_structure: unsafe extern "C" fn(Ptr) -> Ptr,
    structure_get_name: unsafe extern "C" fn(Ptr) -> *const c_char,
    buffer_map: unsafe extern "C" fn(Ptr, *mut GstMapInfo, u32) -> c_int,
    buffer_unmap: unsafe extern "C" fn(Ptr, *mut GstMapInfo),
    app_sink_try_pull_sample: unsafe extern "C" fn(Ptr, u64) -> Ptr,
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn load() -> Result<Api, String> {
    unsafe {
        let core = libloading::Library::new("libgstreamer-1.0.so.0").map_err(|e| e.to_string())?;
        let app = libloading::Library::new("libgstapp-1.0.so.0").map_err(|e| e.to_string())?;
        let gobject = libloading::Library::new("libgobject-2.0.so.0").map_err(|e| e.to_string())?;
        macro_rules! sym {
            ($lib:expr, $name:literal) => {
                *$lib.get(concat!($name, "\0").as_bytes()).map_err(|e| format!("{}: {e}", $name))?
            };
        }
        let api = Api {
            factory_make: sym!(core, "gst_element_factory_make"),
            caps_from_string: sym!(core, "gst_caps_from_string"),
            object_set: sym!(gobject, "g_object_set"),
            init_check: sym!(core, "gst_init_check"),
            parse_launch: sym!(core, "gst_parse_launch"),
            bin_get_by_name: sym!(core, "gst_bin_get_by_name"),
            set_state: sym!(core, "gst_element_set_state"),
            query_position: sym!(core, "gst_element_query_position"),
            query_duration: sym!(core, "gst_element_query_duration"),
            seek_simple: sym!(core, "gst_element_seek_simple"),
            get_bus: sym!(core, "gst_element_get_bus"),
            bus_timed_pop_filtered: sym!(core, "gst_bus_timed_pop_filtered"),
            message_parse_error: sym!(core, "gst_message_parse_error"),
            mini_object_unref: sym!(core, "gst_mini_object_unref"),
            object_unref: sym!(core, "gst_object_unref"),
            sample_get_buffer: sym!(core, "gst_sample_get_buffer"),
            sample_get_caps: sym!(core, "gst_sample_get_caps"),
            caps_get_structure: sym!(core, "gst_caps_get_structure"),
            structure_get_int: sym!(core, "gst_structure_get_int"),
            message_get_structure: sym!(core, "gst_message_get_structure"),
            structure_get_name: sym!(core, "gst_structure_get_name"),
            buffer_map: sym!(core, "gst_buffer_map"),
            buffer_unmap: sym!(core, "gst_buffer_unmap"),
            app_sink_try_pull_sample: sym!(app, "gst_app_sink_try_pull_sample"),
            _core: core,
            _app: app,
            _gobject: gobject,
        };
        let mut err: *mut GError = std::ptr::null_mut();
        if (api.init_check)(std::ptr::null_mut(), std::ptr::null_mut(), &mut err) == 0 {
            return Err(gerror_text(err).unwrap_or_else(|| "gst_init failed".into()));
        }
        Ok(api)
    }
}

unsafe fn gerror_text(err: *mut GError) -> Option<String> {
    if err.is_null() {
        return None;
    }
    let msg = unsafe { (*err).message };
    (!msg.is_null()).then(|| unsafe { CStr::from_ptr(msg) }.to_string_lossy().into_owned())
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(|e| e.clone())
}

/// Whether GStreamer could be loaded on this system.
pub fn available() -> bool {
    api().is_ok()
}

/// A decoded video frame, BGRA, `width * height * 4` bytes (stride already removed).
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Arc<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlayerEvent {
    Ended,
    Error(String),
}

/// What the player tells its UI about the stream (shared, read on each render).
#[derive(Default)]
pub struct PlayerState {
    pub frame: Option<Frame>,
    pub frame_seq: u64,
    pub position_ns: i64,
    pub duration_ns: i64,
    pub playing: bool,
    pub event: Option<PlayerEvent>,
}

pub struct Player {
    pipeline: usize,
    pub state: Arc<Mutex<PlayerState>>,
    stop: Arc<AtomicBool>,
    commands: Sender<Command>,
}

enum Command {
    Play,
    Pause,
    Seek(i64),
}

/// Text shown when the system lacks a decoder for the file.
pub const MISSING_DECODER: &str = "Máy chưa có bộ giải mã cho nội dung này. Hãy cài gói gstreamer1.0-libav (bản Flatpak: org.freedesktop.Platform.codecs-extra) rồi thử lại.";

fn friendly_error(text: &str) -> String {
    let t = text.to_lowercase();
    if t.contains("missing") || t.contains("no suitable plugins") || t.contains("decoder") || t.contains("not-negotiated") || t.contains("not-linked") || t.contains("no element") {
        MISSING_DECODER.into()
    } else {
        format!("Không phát được nội dung này ({text}).")
    }
}

fn file_uri(path: &std::path::Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Player {
    /// Opens `path` and starts playing. Frames and events arrive in `state`; `on_change` runs (on the player thread) whenever
    /// something the UI shows changed.
    pub fn open(path: &std::path::Path, on_change: impl Fn() + Send + 'static) -> Result<Player, String> {
        let api = api()?;
        let pipeline = unsafe { (api.factory_make)(c"playbin".as_ptr(), c"olazplayer".as_ptr()) };
        if pipeline.is_null() {
            return Err("GStreamer playbin is not installed".into());
        }
        let sink = unsafe { (api.factory_make)(c"appsink".as_ptr(), c"olazsink".as_ptr()) };
        let uri = CString::new(file_uri(path)).map_err(|e| e.to_string())?;
        unsafe {
            if !sink.is_null() {
                let caps = (api.caps_from_string)(c"video/x-raw,format=BGRA".as_ptr());
                (api.object_set)(sink, c"caps".as_ptr(), caps, c"max-buffers".as_ptr(), 2u32, c"drop".as_ptr(), 1 as c_int, c"sync".as_ptr(), 1 as c_int, std::ptr::null::<c_char>());
                (api.mini_object_unref)(caps);
                // playbin takes the floating reference of the sink.
                (api.object_set)(pipeline, c"video-sink".as_ptr(), sink, std::ptr::null::<c_char>());
            }
            (api.object_set)(pipeline, c"uri".as_ptr(), uri.as_ptr(), std::ptr::null::<c_char>());
        }
        let bus = unsafe { (api.get_bus)(pipeline) };
        let state = Arc::new(Mutex::new(PlayerState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx): (Sender<Command>, Receiver<Command>) = channel();
        unsafe { (api.set_state)(pipeline, GST_STATE_PLAYING) };
        state.lock().unwrap().playing = true;
        let (p, s, b) = (pipeline as usize, sink as usize, bus as usize);
        let (st, sp) = (state.clone(), stop.clone());
        std::thread::Builder::new()
            .name("olaz-player".into())
            .spawn(move || run(api, p, s, b, st, sp, rx, on_change))
            .map_err(|e| e.to_string())?;
        Ok(Player { pipeline: p, state, stop, commands: tx })
    }

    pub fn play(&self) {
        let _ = self.commands.send(Command::Play);
    }
    pub fn pause(&self) {
        let _ = self.commands.send(Command::Pause);
    }
    pub fn seek(&self, position_ns: i64) {
        let _ = self.commands.send(Command::Seek(position_ns.max(0)));
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.pipeline;
    }
}

#[allow(clippy::too_many_arguments)]
fn run(api: &'static Api, pipeline: usize, sink: usize, bus: usize, state: Arc<Mutex<PlayerState>>, stop: Arc<AtomicBool>, rx: Receiver<Command>, on_change: impl Fn()) {
    let (pipeline, sink, bus) = (pipeline as Ptr, sink as Ptr, bus as Ptr);
    let mut ticks = 0u32;
    let mut missing_plugin = false;
    while !stop.load(Ordering::SeqCst) {
        while let Ok(cmd) = rx.try_recv() {
            unsafe {
                match cmd {
                    Command::Play => {
                        // Replay from the start once it ended.
                        if state.lock().unwrap().event == Some(PlayerEvent::Ended) {
                            (api.seek_simple)(pipeline, GST_FORMAT_TIME, GST_SEEK_FLAG_FLUSH | GST_SEEK_FLAG_ACCURATE, 0);
                            state.lock().unwrap().event = None;
                        }
                        (api.set_state)(pipeline, GST_STATE_PLAYING);
                        state.lock().unwrap().playing = true;
                    }
                    Command::Pause => {
                        (api.set_state)(pipeline, GST_STATE_PAUSED);
                        state.lock().unwrap().playing = false;
                    }
                    Command::Seek(ns) => {
                        (api.seek_simple)(pipeline, GST_FORMAT_TIME, GST_SEEK_FLAG_FLUSH | GST_SEEK_FLAG_ACCURATE, ns);
                        let mut s = state.lock().unwrap();
                        if s.event == Some(PlayerEvent::Ended) {
                            s.event = None;
                        }
                        s.position_ns = ns;
                    }
                }
            }
            on_change();
        }
        // Frames.
        let mut changed = false;
        if !sink.is_null() {
            let sample = unsafe { (api.app_sink_try_pull_sample)(sink, 40_000_000) };
            if !sample.is_null() {
                if let Some(frame) = unsafe { read_frame(api, sample) } {
                    let mut s = state.lock().unwrap();
                    s.frame = Some(frame);
                    s.frame_seq += 1;
                    changed = true;
                }
                unsafe { (api.mini_object_unref)(sample) };
            }
        } else {
            std::thread::sleep(Duration::from_millis(40));
        }
        // Bus.
        let msg = unsafe { (api.bus_timed_pop_filtered)(bus, 0, GST_MESSAGE_EOS | GST_MESSAGE_ERROR | GST_MESSAGE_ELEMENT) };
        if !msg.is_null() && unsafe { (*(msg as *const GstMessageHead)).msg_type } == GST_MESSAGE_ELEMENT {
            // playbin announces a missing decoder with a `missing-plugin` element message before the generic stream error.
            let st = unsafe { (api.message_get_structure)(msg) };
            if !st.is_null() && unsafe { CStr::from_ptr((api.structure_get_name)(st)) }.to_bytes() == b"missing-plugin" {
                missing_plugin = true;
            }
            unsafe { (api.mini_object_unref)(msg) };
        } else if !msg.is_null() {
            let kind = unsafe { (*(msg as *const GstMessageHead)).msg_type };
            let event = if kind == GST_MESSAGE_ERROR && missing_plugin {
                PlayerEvent::Error(MISSING_DECODER.into())
            } else if kind == GST_MESSAGE_ERROR {
                let mut err: *mut GError = std::ptr::null_mut();
                let mut debug: *mut c_char = std::ptr::null_mut();
                unsafe { (api.message_parse_error)(msg, &mut err, &mut debug) };
                let text = unsafe { gerror_text(err) }.unwrap_or_default();
                let dbg = if debug.is_null() { String::new() } else { unsafe { CStr::from_ptr(debug) }.to_string_lossy().into_owned() };
                PlayerEvent::Error(friendly_error(&format!("{text} {dbg}")))
            } else {
                PlayerEvent::Ended
            };
            unsafe { (api.mini_object_unref)(msg) };
            let mut s = state.lock().unwrap();
            s.playing = false;
            s.event = Some(event);
            changed = true;
        }
        // Position / duration about 4× a second.
        ticks += 1;
        if ticks % 6 == 0 {
            let (mut pos, mut dur) = (0i64, 0i64);
            unsafe {
                (api.query_position)(pipeline, GST_FORMAT_TIME, &mut pos);
                (api.query_duration)(pipeline, GST_FORMAT_TIME, &mut dur);
            }
            let mut s = state.lock().unwrap();
            if pos != s.position_ns || dur != s.duration_ns {
                s.position_ns = pos.max(0);
                s.duration_ns = dur.max(0);
                changed = true;
            }
        }
        if changed {
            on_change();
        }
    }
    unsafe {
        (api.set_state)(pipeline, GST_STATE_NULL);
        (api.object_unref)(bus);
        (api.object_unref)(pipeline);
    }
}

unsafe fn read_frame(api: &Api, sample: Ptr) -> Option<Frame> {
    let caps = unsafe { (api.sample_get_caps)(sample) };
    let buffer = unsafe { (api.sample_get_buffer)(sample) };
    if caps.is_null() || buffer.is_null() {
        return None;
    }
    let s = unsafe { (api.caps_get_structure)(caps, 0) };
    let (mut w, mut h) = (0, 0);
    unsafe {
        (api.structure_get_int)(s, c"width".as_ptr(), &mut w);
        (api.structure_get_int)(s, c"height".as_ptr(), &mut h);
    }
    if w <= 0 || h <= 0 {
        return None;
    }
    let mut info: GstMapInfo = unsafe { std::mem::zeroed() };
    if unsafe { (api.buffer_map)(buffer, &mut info, GST_MAP_READ) } == 0 {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    let row = w * 4;
    let stride = if h > 0 { info.size / h } else { row };
    let data = unsafe { std::slice::from_raw_parts(info.data, info.size) };
    let mut out = Vec::with_capacity(row * h);
    if stride >= row {
        for y in 0..h {
            out.extend_from_slice(&data[y * stride..y * stride + row]);
        }
    }
    unsafe { (api.buffer_unmap)(buffer, &mut info) };
    (out.len() == row * h).then(|| Frame { width: w as u32, height: h as u32, bgra: Arc::new(out) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_and_errors() {
        assert_eq!(file_uri(std::path::Path::new("/tmp/a b/ả.mp4")), "file:///tmp/a%20b/%E1%BA%A3.mp4");
        assert_eq!(friendly_error("Your GStreamer installation is missing a plug-in."), MISSING_DECODER);
        assert!(friendly_error("Resource not found.").starts_with("Không phát được"));
    }

    /// Plays a generated VP8 WebM through the real pipeline (skipped when GStreamer or the encoder is absent).
    #[test]
    fn plays_a_webm_file() {
        if !available() {
            eprintln!("GStreamer not available; skipped");
            return;
        }
        let dir = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
        let file = dir.path().join("clip.webm");
        let ok = std::process::Command::new("gst-launch-1.0")
            .args(["-q", "videotestsrc", "num-buffers=30", "!", "video/x-raw,width=160,height=90,framerate=30/1", "!", "vp8enc", "!", "webmmux", "!", "filesink"])
            .arg(format!("location={}", file.display()))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("cannot generate a test clip; skipped");
            return;
        }
        let player = Player::open(&file, || {}).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            {
                let s = player.state.lock().unwrap();
                if let Some(PlayerEvent::Error(e)) = &s.event {
                    panic!("playback error: {e}");
                }
                if s.event == Some(PlayerEvent::Ended) {
                    let f = s.frame.clone().expect("a frame was decoded");
                    assert_eq!((f.width, f.height, f.bgra.len()), (160, 90, 160 * 90 * 4));
                    assert!(s.frame_seq >= 10, "frames: {}", s.frame_seq);
                    break;
                }
            }
            assert!(std::time::Instant::now() < deadline, "playback did not finish");
            std::thread::sleep(Duration::from_millis(100));
        }
        // Play after the end replays from the start.
        player.play();
        std::thread::sleep(Duration::from_millis(300));
        let s = player.state.lock().unwrap();
        assert_eq!(s.event, None);
        assert!(s.playing);
    }

    #[test]
    fn reports_a_missing_decoder() {
        if !available() || ["av1dec", "dav1ddec", "aomdec"].iter().any(|e| std::process::Command::new("gst-inspect-1.0").arg(e).output().is_ok_and(|o| o.status.success())) {
            eprintln!("no GStreamer, or an AV1 decoder is installed; skipped");
            return;
        }
        let dir = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
        let vp8 = dir.path().join("clip.mkv");
        let ok = std::process::Command::new("gst-launch-1.0")
            .args(["-q", "videotestsrc", "num-buffers=10", "!", "vp8enc", "!", "matroskamux", "!", "filesink"])
            .arg(format!("location={}", vp8.display()))
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("cannot generate a test clip; skipped");
            return;
        }
        // Same length codec id, so the EBML stays valid: the file now claims AV1, which nothing here decodes.
        let bytes = std::fs::read(&vp8).unwrap();
        let at = bytes.windows(5).position(|w| w == b"V_VP8").unwrap();
        let mut fake = bytes.clone();
        fake[at..at + 5].copy_from_slice(b"V_AV1");
        let file = dir.path().join("fake-av1.mkv");
        std::fs::write(&file, fake).unwrap();
        let player = Player::open(&file, || {}).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(ev) = player.state.lock().unwrap().event.clone() {
                assert_eq!(ev, PlayerEvent::Error(MISSING_DECODER.into()));
                break;
            }
            assert!(std::time::Instant::now() < deadline, "no error reported");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
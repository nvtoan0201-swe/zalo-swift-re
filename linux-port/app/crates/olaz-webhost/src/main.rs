//! `olaz-webhost`: hosts the official Zalo renderer in hidden WebKitGTK views.
//!
//! This build uses **WebKitGTK 4.1** (GTK 3) — the
//! WebKit flavour available on the target systems — instead of WPE/WebKitGTK 6. Views live in
//! `GtkOffscreenWindow`s, which are mapped and visible to WebKit, so timers are never throttled.
//!
//! The process never talks to the network: the network shim routes every remote request to the app.
//! Frames to/from the app: `olaz-ipc`, one ordered stream over an inherited socket.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::io::Read;
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::mpsc;

use gtk::prelude::*;
use olaz_core::asar::AsarArchive;
use olaz_core::scheme::{self, Served};
use olaz_ipc::{AppToHost, HostToApp};
use serde_json::Value;
use webkit2gtk::{
    CacheModel, FileChooserRequestExt, HardwareAccelerationPolicy, LoadEvent, NavigationPolicyDecision,
    NavigationPolicyDecisionExt, PolicyDecisionExt, PolicyDecisionType, SecurityManagerExt, SettingsExt,
    URIRequestExt, URISchemeRequest, URISchemeRequestExt, URISchemeResponse, URISchemeResponseExt, UserContentInjectedFrames,
    UserContentManager, UserContentManagerExt, UserScript, UserScriptInjectionTime, WebContext, WebContextExt, WebView, WebViewExt,
    WebsiteDataManager,
};

struct Args {
    fd: i32,
    asar: String,
    data_dir: String,
    cache_dir: String,
    debug: bool,
    visible: bool,
}

fn parse_args() -> Args {
    let mut a = Args { fd: -1, asar: String::new(), data_dir: String::new(), cache_dir: String::new(), debug: false, visible: false };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--fd" => a.fd = it.next().and_then(|v| v.parse().ok()).unwrap_or(-1),
            "--asar" => a.asar = it.next().unwrap_or_default(),
            "--data" => a.data_dir = it.next().unwrap_or_default(),
            "--cache" => a.cache_dir = it.next().unwrap_or_default(),
            "--debug" => a.debug = true,
            "--visible" => a.visible = true,
            _ => {}
        }
    }
    if std::env::var("OLAZ_WEBHOST").as_deref() == Ok("gtk-visible") {
        a.visible = true;
    }
    a
}

struct View {
    web_view: WebView,
    window: gtk::Window,
    pending_pick: Option<Vec<String>>,
}

/// A `WebKitScriptMessageReply` kept until the app answers (+ the JSC context to build the value in).
struct PendingReply {
    reply: *mut webkit2gtk_sys::WebKitScriptMessageReply,
    context: *mut javascriptcore_rs_sys::JSCContext,
}

impl Drop for PendingReply {
    fn drop(&mut self) {
        unsafe {
            webkit2gtk_sys::webkit_script_message_reply_unref(self.reply);
            glib::gobject_ffi::g_object_unref(self.context as *mut _);
        }
    }
}

struct Host {
    out: mpsc::Sender<HostToApp>,
    asar: AsarArchive,
    debug: bool,
    visible: bool,
    context: WebContext,
    views: HashMap<u32, View>,
    pending_sync: HashMap<u64, URISchemeRequest>,
    pending_replies: HashMap<u64, PendingReply>,
    next_id: u64,
}

type Shared = Rc<RefCell<Host>>;

thread_local! {
    static HOST: RefCell<Option<Shared>> = const { RefCell::new(None) };
}

fn host() -> Shared {
    HOST.with(|h| h.borrow().clone().expect("host"))
}

fn send(frame: HostToApp) {
    let _ = host().borrow().out.send(frame);
}

fn next_id() -> u64 {
    let h = host();
    let mut h = h.borrow_mut();
    h.next_id += 1;
    h.next_id
}

fn view_id_of(web_view: &WebView) -> Option<u32> {
    host().borrow().views.iter().find(|(_, v)| &v.web_view == web_view).map(|(id, _)| *id)
}

#[allow(deprecated)]
fn main() {
    let args = parse_args();
    if args.fd < 0 || args.asar.is_empty() {
        eprintln!("usage: olaz-webhost --fd <n> --asar <app.asar> --data <dir> --cache <dir> [--debug]");
        std::process::exit(2);
    }
    // The app owns the network: WebKit must not need a proxy, sandbox paths or GPU for hidden views.
    if !args.visible {
        // SAFETY: before any thread is started.
        unsafe { std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1") };
    }
    gtk::init().expect("gtk init");

    let socket = unsafe { UnixStream::from_raw_fd(args.fd) };
    let asar = match AsarArchive::open(std::path::Path::new(&args.asar)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("olaz-webhost: {e}");
            std::process::exit(3);
        }
    };

    // Writer thread: frames leave in the order they were produced.
    let (out_tx, out_rx) = mpsc::channel::<HostToApp>();
    let mut writer = socket.try_clone().expect("socket clone");
    std::thread::spawn(move || {
        for frame in out_rx {
            if olaz_ipc::write_frame(&mut writer, &frame).is_err() {
                break;
            }
        }
    });

    let manager = WebsiteDataManager::builder()
        .base_data_directory(args.data_dir.clone())
        .base_cache_directory(args.cache_dir.clone())
        .build();
    let context = WebContext::with_website_data_manager(&manager);
    context.set_cache_model(CacheModel::DocumentViewer);
    if let Some(security) = context.security_manager() {
        security.register_uri_scheme_as_secure(scheme::SCHEME);
        security.register_uri_scheme_as_cors_enabled(scheme::SCHEME);
    }
    context.register_uri_scheme(scheme::SCHEME, handle_scheme_request);

    let shared: Shared = Rc::new(RefCell::new(Host {
        out: out_tx,
        asar,
        debug: args.debug,
        visible: args.visible,
        context,
        views: HashMap::new(),
        pending_sync: HashMap::new(),
        pending_replies: HashMap::new(),
        next_id: 0,
    }));
    HOST.with(|h| *h.borrow_mut() = Some(shared));

    let version = format!(
        "{}.{}.{}",
        unsafe { webkit2gtk_sys::webkit_get_major_version() },
        unsafe { webkit2gtk_sys::webkit_get_minor_version() },
        unsafe { webkit2gtk_sys::webkit_get_micro_version() }
    );
    send(HostToApp::Hello { build: olaz_ipc::BUILD_ID.into(), webkit: version, platform: "gtk".into() });

    // Reader thread → GTK main loop, strictly in order.
    let (in_tx, in_rx) = glib::MainContext::channel::<Option<AppToHost>>(glib::Priority::DEFAULT);
    let mut reader = socket;
    std::thread::spawn(move || {
        loop {
            match olaz_ipc::read_frame::<_, AppToHost>(&mut reader) {
                Ok(Some(frame)) => {
                    if in_tx.send(Some(frame)).is_err() {
                        break;
                    }
                }
                _ => {
                    let _ = in_tx.send(None);
                    break;
                }
            }
        }
    });
    in_rx.attach(None, |frame| {
        match frame {
            Some(frame) => handle_frame(frame),
            None => gtk::main_quit(),
        }
        glib::ControlFlow::Continue
    });
    gtk::main();
}

fn handle_frame(frame: AppToHost) {
    match frame {
        AppToHost::Create { view, ua, scripts, debug: _ } => create_view(view, &ua, &scripts),
        AppToHost::Load { view, url } => {
            if let Some(v) = host().borrow().views.get(&view) {
                v.web_view.load_uri(&url);
            }
        }
        AppToHost::Eval { view, token, body, args } => run_js(view, &body, args, Some(token)),
        AppToHost::Call { view, body, args } => run_js(view, &body, Some(args), None),
        AppToHost::Reply { view: _, id, value, error } => {
            let pending = host().borrow_mut().pending_replies.remove(&id);
            if let Some(p) = pending {
                unsafe {
                    match error {
                        Some(msg) => {
                            let c = CString::new(msg.replace('\0', "")).unwrap();
                            webkit2gtk_sys::webkit_script_message_reply_return_error_message(p.reply, c.as_ptr());
                        }
                        None => {
                            let json = value.map(|v| v.to_string()).unwrap_or_else(|| "null".into());
                            let c = CString::new(json).unwrap();
                            let js = javascriptcore_rs_sys::jsc_value_new_from_json(p.context, c.as_ptr());
                            webkit2gtk_sys::webkit_script_message_reply_return_value(p.reply, js);
                            if !js.is_null() {
                                glib::gobject_ffi::g_object_unref(js as *mut _);
                            }
                        }
                    }
                }
            }
        }
        AppToHost::SyncReply { view: _, id, body } => {
            let request = host().borrow_mut().pending_sync.remove(&id);
            if let Some(request) = request {
                finish(&request, scheme::json_response(body.into_bytes()));
            }
        }
        AppToHost::Pick { view, files } => {
            if let Some(v) = host().borrow_mut().views.get_mut(&view) {
                v.pending_pick = files;
            }
        }
        AppToHost::Shutdown { view } => {
            let removed = host().borrow_mut().views.remove(&view);
            if let Some(v) = removed {
                v.web_view.stop_loading();
                v.web_view.load_html("", None);
                if let Some(ucm) = v.web_view.user_content_manager() {
                    for name in ["zlog", "zstate", "znet", "zbridge"] {
                        ucm.unregister_script_message_handler(name);
                    }
                }
                unsafe { v.window.destroy() };
            }
        }
        AppToHost::Debug { view } => show_debug(view),
        AppToHost::Gc => {
            for v in host().borrow().views.values() {
                let _ = v;
            }
        }
        AppToHost::Quit => gtk::main_quit(),
    }
}

fn create_view(view: u32, ua: &str, scripts: &[String]) {
    let ucm = UserContentManager::new();
    for source in scripts {
        let script = UserScript::new(
            source,
            UserContentInjectedFrames::AllFrames,
            UserScriptInjectionTime::Start,
            &[],
            &[],
        );
        ucm.add_script(&script);
    }
    // zlog / zstate: no reply.
    for name in ["zlog", "zstate"] {
        ucm.register_script_message_handler(name);
        let handler = name.to_string();
        ucm.connect_script_message_received(Some(name), move |_, result| {
            let Some(value) = result.js_value() else { return };
            let body = js_to_json(&value);
            if handler == "zstate" && !body.is_object() {
                return; // StateHandler forwards dictionaries only
            }
            send(HostToApp::Msg { view, handler: handler.clone(), body });
        });
    }
    // znet / zbridge: with reply, page world.
    for name in ["znet", "zbridge"] {
        let c_name = CString::new(name).unwrap();
        unsafe {
            webkit2gtk_sys::webkit_user_content_manager_register_script_message_handler_with_reply(
                ucm.as_ptr() as *mut _,
                c_name.as_ptr(),
                std::ptr::null(),
            );
            let signal = CString::new(format!("script-message-with-reply-received::{name}")).unwrap();
            let data = Box::into_raw(Box::new((view, name.to_string()))) as *mut c_void;
            glib::gobject_ffi::g_signal_connect_data(
                ucm.as_ptr() as *mut _,
                signal.as_ptr(),
                Some(std::mem::transmute::<*const (), unsafe extern "C" fn()>(on_message_with_reply as *const ())),
                data,
                None,
                0,
            );
        }
    }

    let web_view: WebView = glib::Object::builder()
        .property("web-context", &host().borrow().context)
        .property("user-content-manager", &ucm)
        .build();
    if let Some(settings) = WebViewExt::settings(&web_view) {
        settings.set_user_agent(Some(ua));
        settings.set_enable_developer_extras(true);
        settings.set_javascript_can_open_windows_automatically(false);
        settings.set_enable_write_console_messages_to_stdout(false);
        settings.set_enable_page_cache(false);
        settings.set_enable_back_forward_navigation_gestures(false);
        settings.set_javascript_can_access_clipboard(true);
        settings.set_enable_media_stream(true);
        if !host().borrow().visible {
            settings.set_hardware_acceleration_policy(HardwareAccelerationPolicy::Never);
        }
    }

    web_view.connect_load_changed(move |wv, event| {
        let Some(id) = view_id_of(wv) else { return };
        let ev = match event {
            LoadEvent::Started => "started",
            LoadEvent::Finished => "finished",
            _ => return,
        };
        send(HostToApp::Page { view: id, event: ev.into(), error: None, url: wv.uri().map(|u| u.to_string()) });
    });
    web_view.connect_load_failed(move |wv, _event, uri, error| {
        if let Some(id) = view_id_of(wv) {
            send(HostToApp::Page { view: id, event: "failed".into(), error: Some(error.message().to_string()), url: Some(uri.to_string()) });
        }
        false
    });
    web_view.connect_web_process_terminated(move |wv, reason| {
        if let Some(id) = view_id_of(wv) {
            send(HostToApp::Crashed { view: id, reason: format!("{reason:?}") });
        }
        wv.reload();
    });
    web_view.connect_decide_policy(move |wv, decision, kind| {
        if kind != PolicyDecisionType::NavigationAction && kind != PolicyDecisionType::NewWindowAction {
            return false;
        }
        let Some(nav) = decision.downcast_ref::<NavigationPolicyDecision>() else { return false };
        let Some(mut action) = nav.navigation_action() else { return false };
        let uri = action.request().and_then(|r| r.uri()).map(|u| u.to_string()).unwrap_or_default();
        let scheme_of = uri.split(':').next().unwrap_or("").to_lowercase();
        if matches!(scheme_of.as_str(), "zalo-app" | "about" | "blob" | "data") && kind == PolicyDecisionType::NavigationAction {
            decision.use_();
            return true;
        }
        let user_link = matches!(
            action.navigation_type(),
            webkit2gtk::NavigationType::LinkClicked | webkit2gtk::NavigationType::FormSubmitted
        );
        if kind == PolicyDecisionType::NavigationAction && !user_link && action.frame_name().is_some() {
            // A named subframe loading remote content (embeds): allowed like macOS's non-main-frame rule.
            decision.use_();
            return true;
        }
        if matches!(scheme_of.as_str(), "http" | "https") {
            if let Some(id) = view_id_of(wv) {
                send(HostToApp::OpenExternal { view: id, url: uri });
            }
        }
        decision.ignore();
        true
    });
    web_view.connect_create(|_, _| None);
    web_view.connect_run_file_chooser(move |wv, request| {
        let Some(id) = view_id_of(wv) else { return false };
        let pick = host().borrow_mut().views.get_mut(&id).and_then(|v| v.pending_pick.take());
        let multiple = request.selects_multiple();
        match pick.filter(|f| !f.is_empty()) {
            Some(files) => {
                let chosen: Vec<&str> = if multiple { files.iter().map(String::as_str).collect() } else { vec![files[0].as_str()] };
                request.select_files(&chosen);
                send(HostToApp::Chooser { view: id, multiple, taken: chosen.len() });
            }
            None => {
                request.cancel();
                send(HostToApp::Chooser { view: id, multiple, taken: 0 });
            }
        }
        true
    });

    let visible = host().borrow().visible;
    let window: gtk::Window = if visible {
        let w = gtk::Window::new(gtk::WindowType::Toplevel);
        w.set_title("Zalo renderer (debug)");
        w
    } else {
        gtk::OffscreenWindow::new().upcast()
    };
    window.set_default_size(1100, 720);
    window.add(&web_view);
    window.show_all();
    host().borrow_mut().views.insert(view, View { web_view, window, pending_pick: None });
}

/// Moves a hidden view into a real window ("Hiện renderer Zalo chính thức").
fn show_debug(view: u32) {
    let h = host();
    let mut h = h.borrow_mut();
    let Some(v) = h.views.get_mut(&view) else { return };
    if v.window.is::<gtk::OffscreenWindow>() {
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("Zalo renderer (debug)");
        window.set_default_size(1100, 720);
        v.window.remove(&v.web_view);
        window.add(&v.web_view);
        window.show_all();
        let old = std::mem::replace(&mut v.window, window);
        unsafe { old.destroy() };
        // Closing the debug window must not destroy the renderer: hide it instead.
        v.window.connect_delete_event(|w, _| {
            w.hide();
            glib::Propagation::Stop
        });
    } else {
        v.window.present();
    }
}

fn js_to_json(value: &javascriptcore::Value) -> Value {
    use javascriptcore::ValueExt;
    if value.is_undefined() {
        return Value::Null;
    }
    value.to_json(0).and_then(|s| serde_json::from_str(s.as_str()).ok()).unwrap_or(Value::Null)
}

unsafe extern "C" fn on_message_with_reply(
    _ucm: *mut webkit2gtk_sys::WebKitUserContentManager,
    value: *mut javascriptcore_rs_sys::JSCValue,
    reply: *mut webkit2gtk_sys::WebKitScriptMessageReply,
    data: *mut c_void,
) -> glib::ffi::gboolean {
    let (view, handler) = unsafe { &*(data as *const (u32, String)) };
    let body = unsafe {
        let json = javascriptcore_rs_sys::jsc_value_to_json(value, 0);
        if json.is_null() {
            Value::Null
        } else {
            let parsed = serde_json::from_str(CStr::from_ptr(json as *const c_char).to_str().unwrap_or("null")).unwrap_or(Value::Null);
            glib::ffi::g_free(json as *mut _);
            parsed
        }
    };
    let id = next_id();
    unsafe {
        webkit2gtk_sys::webkit_script_message_reply_ref(reply);
        let context = javascriptcore_rs_sys::jsc_value_get_context(value);
        glib::gobject_ffi::g_object_ref(context as *mut _);
        host().borrow_mut().pending_replies.insert(id, PendingReply { reply, context });
    }
    send(HostToApp::Request { view: *view, handler: handler.clone(), id, body });
    glib::ffi::GTRUE
}

/// Runs `body` as an async function; named arguments arrive JSON-encoded (one string GVariant) and are
/// destructured inside, so values reach the page exactly as JSON (like `callAsyncJavaScript(arguments:)`).
fn run_js(view: u32, body: &str, args: Option<Value>, token: Option<u64>) {
    let Some(web_view) = host().borrow().views.get(&view).map(|v| v.web_view.clone()) else {
        if let Some(token) = token {
            send(HostToApp::EvalResult { view, token, value: None, error: Some("no view".into()) });
        }
        return;
    };
    let (source, variant) = match args.as_ref().and_then(Value::as_object) {
        Some(map) if !map.is_empty() => {
            let names: Vec<&str> = map.keys().map(String::as_str).collect();
            let dict = glib::VariantDict::new(None);
            dict.insert_value("__olazArgs", &Value::Object(map.clone()).to_string().to_variant());
            (format!("const {{{}}} = JSON.parse(__olazArgs);\n{body}", names.join(",")), Some(dict.end()))
        }
        _ => (body.to_string(), None),
    };
    web_view.call_async_javascript_function(&source, variant.as_ref(), None, None, None::<&gio::Cancellable>, move |result| {
        let Some(token) = token else { return };
        let frame = match result {
            Ok(value) => HostToApp::EvalResult { view, token, value: Some(js_to_json(&value)), error: None },
            Err(e) => HostToApp::EvalResult { view, token, value: None, error: Some(e.message().to_string()) },
        };
        send(frame);
    });
}

fn handle_scheme_request(request: &URISchemeRequest) {
    let path = request.path().map(|p| p.to_string()).unwrap_or_else(|| "/".into());
    let view = request.web_view().and_then(|wv| view_id_of(&wv)).unwrap_or(0);
    let served = {
        let h = host();
        let h = h.borrow();
        let mut logs = Vec::new();
        let served = scheme::serve(&h.asar, &path, h.debug, &mut |line| logs.push(line));
        drop(h);
        for line in logs {
            if let Some(p) = line.strip_prefix("[404] ") {
                send(HostToApp::NotFound { view, path: p.to_string() });
            } else {
                send(HostToApp::Log { line });
            }
        }
        served
    };
    match served {
        Served::BridgeSync => {
            let mut body = Vec::new();
            if let Some(stream) = request.http_body() {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    match stream.read(&mut buf[..], None::<&gio::Cancellable>) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => body.extend_from_slice(&buf[..n]),
                    }
                }
            }
            let id = next_id();
            host().borrow_mut().pending_sync.insert(id, request.clone());
            send(HostToApp::Sync { view, id, body: String::from_utf8_lossy(&body).into_owned() });
        }
        Served::Response(response) => finish(request, response),
    }
}

fn finish(request: &URISchemeRequest, response: scheme::SchemeResponse) {
    let len = response.body.len() as i64;
    let bytes = glib::Bytes::from_owned(response.body);
    let stream = gio::MemoryInputStream::from_bytes(&bytes);
    let resp = URISchemeResponse::new(&stream, len);
    resp.set_status(response.status as u32, None);
    if let Some((_, ct)) = response.headers.iter().find(|(k, _)| k == "Content-Type") {
        resp.set_content_type(ct);
    }
    if !response.headers.is_empty() {
        let headers = soup::MessageHeaders::new(soup::MessageHeadersType::Response);
        for (k, v) in &response.headers {
            headers.append(k, v);
        }
        resp.set_http_headers(headers);
    }
    request.finish_with_response(&resp);
}

#[allow(dead_code)]
fn read_all(mut r: impl Read) -> Vec<u8> {
    let mut v = Vec::new();
    let _ = r.read_to_end(&mut v);
    v
}

//! `navigator.connection` feed: NetworkManager over the system D-Bus. Every change runs
//! `window.__zaloNet.connectionChanged(info)` in the page (and again after each page load). Without NetworkManager the
//! polyfill defaults stay.

use std::rc::Rc;

use olaz_core::exec::spawn;
use serde_json::{Value, json};

use crate::engine::ZaloEngine;

/// NetworkManager's view, raw.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NmState {
    pub primary_type: String,
    /// NMState (70 = connected global, 60 = site, 50 = local, ≤ 40 = not connected).
    pub state: u32,
    /// NMConnectivityState (4 = full).
    pub connectivity: u32,
    /// NMMetered (1 = yes, 3 = guess yes).
    pub metered: u32,
}

/// The `connectionChanged` payload (same fields as NWPathMonitor gave on macOS).
pub fn connection_info(s: &NmState) -> Value {
    let connected = s.state >= 50;
    if !connected {
        return json!({"type": "none", "effectiveType": "slow-2g", "saveData": false});
    }
    let kind = match s.primary_type.as_str() {
        "802-11-wireless" | "wifi" => "wifi",
        "802-3-ethernet" | "ethernet" => "ethernet",
        "gsm" | "cdma" | "wwan" => "cellular",
        _ => "other",
    };
    let constrained = matches!(s.metered, 1 | 3);
    json!({"type": kind, "effectiveType": if constrained { "3g" } else { "4g" }, "saveData": constrained})
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager", default_service = "org.freedesktop.NetworkManager", default_path = "/org/freedesktop/NetworkManager")]
trait NetworkManager {
    #[zbus(property)]
    fn primary_connection_type(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn connectivity(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn metered(&self) -> zbus::Result<u32>;
}

async fn read(p: &NetworkManagerProxy<'_>) -> NmState {
    NmState {
        primary_type: p.primary_connection_type().await.unwrap_or_default(),
        state: p.state().await.unwrap_or(70),
        connectivity: p.connectivity().await.unwrap_or(4),
        metered: p.metered().await.unwrap_or(0),
    }
}

pub fn start(engine: &Rc<ZaloEngine>) {
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Value>();
    olaz_host::net::runtime().spawn(async move {
        use futures::StreamExt;
        let Ok(conn) = zbus::Connection::system().await else { return };
        let Ok(proxy) = NetworkManagerProxy::new(&conn).await else { return };
        let mut last = Value::Null;
        let mut send = |s: NmState| {
            let info = connection_info(&s);
            if info != last {
                last = info.clone();
                let _ = tx.unbounded_send(info);
            }
        };
        send(read(&proxy).await);
        let a = proxy.receive_state_changed().await.map(|_| ());
        let b = proxy.receive_primary_connection_type_changed().await.map(|_| ());
        let c = proxy.receive_metered_changed().await.map(|_| ());
        let mut changes = futures::stream::select(futures::stream::select(a, b), c);
        while changes.next().await.is_some() {
            send(read(&proxy).await);
        }
    });
    let weak = Rc::downgrade(engine);
    spawn(&engine.exec, async move {
        use futures::StreamExt;
        while let Some(info) = rx.next().await {
            let Some(me) = weak.upgrade() else { return };
            olaz_core::log::write(format!("network {}", info["type"].as_str().unwrap_or("-")));
            me.state.borrow_mut().connection_info = Some(info.clone());
            if let Some(h) = me.host() {
                h.connection_changed(info);
            }
        }
    });
}

impl ZaloEngine {
    /// A freshly loaded page starts from the polyfill defaults: give it the current state.
    pub(crate) fn republish_connection(&self) {
        let info = self.state.borrow().connection_info.clone();
        if let (Some(info), Some(h)) = (info, self.host()) {
            h.connection_changed(info);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_network_manager() {
        let s = |t: &str, state: u32, metered: u32| NmState { primary_type: t.into(), state, connectivity: 4, metered };
        assert_eq!(connection_info(&s("802-11-wireless", 70, 0)), json!({"type": "wifi", "effectiveType": "4g", "saveData": false}));
        assert_eq!(connection_info(&s("802-3-ethernet", 70, 4))["type"], "ethernet");
        assert_eq!(connection_info(&s("gsm", 70, 1)), json!({"type": "cellular", "effectiveType": "3g", "saveData": true}));
        assert_eq!(connection_info(&s("vpn", 60, 3))["effectiveType"], "3g");
        assert_eq!(connection_info(&s("", 20, 0)), json!({"type": "none", "effectiveType": "slow-2g", "saveData": false}));
    }
}

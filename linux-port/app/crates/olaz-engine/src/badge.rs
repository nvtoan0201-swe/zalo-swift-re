//! Unread badge on the dock icon: `com.canonical.Unity.LauncherEntry.Update` on the session bus (read by GNOME Dash to Dock,
//! KDE, Plank…), addressed to the `app.olaz.linux.desktop` entry. The count comes from the official client's badge.

use std::collections::HashMap;

/// `application://<APP_ID>.desktop`.
pub fn app_uri() -> String {
    format!("application://{}.desktop", olaz_core::APP_ID)
}

/// The `a{sv}` properties of an update.
pub fn properties(count: i64) -> HashMap<&'static str, zbus::zvariant::Value<'static>> {
    let mut m = HashMap::new();
    m.insert("count", zbus::zvariant::Value::from(count.max(0)));
    m.insert("count-visible", zbus::zvariant::Value::from(count > 0));
    m
}

/// Publishes `count` (0 hides the badge). Fire and forget.
pub fn publish(count: i64) {
    olaz_host::net::runtime().spawn(async move {
        let Ok(conn) = connection().await else { return };
        let _ = conn.emit_signal(None::<()>, "/app/olaz/linux", "com.canonical.Unity.LauncherEntry", "Update", &(app_uri(), properties(count))).await;
    });
}

async fn connection() -> zbus::Result<zbus::Connection> {
    static CONN: tokio::sync::OnceCell<zbus::Connection> = tokio::sync::OnceCell::const_new();
    CONN.get_or_try_init(zbus::Connection::session).await.cloned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn props() {
        let p = super::properties(3);
        assert_eq!(p["count"], zbus::zvariant::Value::from(3i64));
        assert_eq!(p["count-visible"], zbus::zvariant::Value::from(true));
        assert_eq!(super::properties(-1)["count-visible"], zbus::zvariant::Value::from(false));
    }
}

//! Pure building blocks shared by every Olaz crate: asar + scheme patches, bridge values, crypto parity,
//! cookie jar, paths, logging, the single-threaded executor seam.

pub mod asar;
pub mod clock;
pub mod cookies;
pub mod crypto;
pub mod exec;
pub mod fingerprint;
pub mod log;
pub mod paths;
pub mod scheme;
pub mod scripts;
pub mod value;

pub use value::{Args, BV};

/// Desktop app id: window `app_id`/WM class, `.desktop` file name, Flatpak id.
pub const APP_ID: &str = "app.olaz.linux";

/// macOS client identity.
pub const CLIENT_TYPE: &str = "23";
pub const CLIENT_VERSION: &str = "694";

/// `"ZaloPC-darwin-<type>v<version>"` — keep `darwin`.
pub fn user_agent(client_type: &str, client_version: &str) -> String {
    format!("ZaloPC-darwin-{client_type}v{client_version}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn ua() {
        assert_eq!(super::user_agent(super::CLIENT_TYPE, super::CLIENT_VERSION), "ZaloPC-darwin-23v694");
    }
}

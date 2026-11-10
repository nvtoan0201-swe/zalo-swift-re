//! The Electron main process of Zalo PC, ported: IPC channels, stores, secrets,
//! shared-worker routing, file system bridge.

pub mod clipboard;
pub mod fs;
pub mod main_process;
pub mod platform;
pub mod secrets;
pub mod sqlite_host;
pub mod worker_router;

pub use main_process::{BridgeModule, MainEvent, MainProcess, install_bridge, page_query};
pub use platform::{Settings, ZaloPlatform};

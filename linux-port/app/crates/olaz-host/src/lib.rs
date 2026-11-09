//! App side of the official renderer: webhost process + views, `$z*` bridge, network proxy.

pub mod bridge;
pub mod host;
pub mod net;

pub use bridge::{BridgeResult, PageSink, Replier, ZaloBridge, none};
pub use host::{ClientHost, Page, PageEvent, WebHostConfig, WebHostProcess};
pub use net::NetworkProxy;

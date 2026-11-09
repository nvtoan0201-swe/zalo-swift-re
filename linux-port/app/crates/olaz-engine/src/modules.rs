//! Feature snapshot routing and main-process modules (hook points of the feature modules).

use std::rc::Rc;

use olaz_main::MainProcess;
use serde_json::{Map, Value};

use crate::engine::ZaloEngine;

/// Snapshot keys routed before chat/conversations, in the macOS engine's order (first match wins).
pub const FEATURE_KEYS: &[&str] = &[
    "convActions",
    "conversationExtras",
    "zcloud",
    "poll",
    "compose",
    "pins",
    "contacts",
    "notification",
    "call",
    "transfer",
    "transferAvailable",
];

pub fn route_feature_snapshot(engine: &ZaloEngine, key: &str, state: &Map<String, Value>) {
    crate::features::handle(engine, key, state);
}

/// Bridge modules of the main process (downloads, notifications…).
pub fn install_modules(main: &Rc<MainProcess>, engine: &Rc<ZaloEngine>) {
    crate::features::install_modules(main, engine);
}

pub fn drop_message_prompts(engine: &ZaloEngine) {
    crate::features::drop_message_prompts(engine);
}

pub fn note_poll_messages(engine: &ZaloEngine, chat: &Value) {
    crate::features::note_poll_messages(engine, chat);
}

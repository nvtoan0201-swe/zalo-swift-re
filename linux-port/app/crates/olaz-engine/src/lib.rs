//! ZaloEngine: the single object the UI binds to. GPUI-free: state lives in `RefCell`s mutated on the
//! executor thread; views subscribe to `Change`s.

pub mod badge;
pub mod card_actions;
pub mod chat;
pub mod cloud;
pub mod collate;
pub mod composer;
pub mod contacts;
pub mod conversation_actions;
pub mod conversations;
pub mod dev;
pub mod engine;
pub mod features;
pub mod fixtures;
pub mod group_create;
pub mod media;
pub mod message_actions;
pub mod models;
pub mod modules;
pub mod network_monitor;
pub mod polls;
pub mod shared_media;
pub mod stickers;
pub mod transfer;
pub mod notifications;

pub use engine::{Change, ConversationSummary, LoginState, LoginStep, Phase, Profile, RuntimeSetup, Services, ZaloEngine};

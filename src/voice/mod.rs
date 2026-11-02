//! Voice Mode: talk to coding sessions through GPT-Live.
//!
//! A pinned browser tab holds the microphone and speaker and talks to OpenAI
//! over WebRTC. This module serves that page, relays its events, and runs the
//! tools OpenAI's backend model calls against the dashboard's sessions.

pub mod browser;
pub mod call;
pub mod config;
pub mod digest;
pub mod host;
pub mod log;
pub mod server;
pub mod service;
pub mod tools;
pub mod xai;

pub use call::{Line, Who};
pub use service::{set_viewing, Draft, PanelStatus, Phase, Toggle, VoiceService};
pub use tools::{HistoryHit, Turn, VoiceHost};

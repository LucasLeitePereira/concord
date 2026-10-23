//! sala-core library: all business logic lives here; `main.rs` is a thin entry point.

pub mod audio_device;
pub mod cli;
pub mod config;
pub mod game_detector;
pub mod identity;
pub mod jitter;
pub mod net;
#[cfg(windows)]
pub mod platform;
pub mod session;
pub mod voice;

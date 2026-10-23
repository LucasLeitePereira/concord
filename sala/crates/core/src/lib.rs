//! sala-core library: all business logic lives here; `main.rs` is a thin entry point.

#[cfg(windows)]
pub mod app;
pub mod audio_device;
pub mod cli;
pub mod config;
pub mod game_detector;
pub mod identity;
#[cfg(windows)]
pub mod ipc;
pub mod jitter;
pub mod net;
#[cfg(windows)]
pub mod platform;
pub mod session;
#[cfg(windows)]
pub mod ui_process;
pub mod voice;

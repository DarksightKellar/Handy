//! The learning layer: Handy learns from the corrections made to pasted text.
//! See `docs/learning/DESIGN.md`.

pub mod commands;
pub mod core;
pub mod jev;
pub mod notice;
pub mod notice_window;
#[cfg(target_os = "macos")]
#[path = "watcher_macos.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "watcher_windows.rs"]
mod platform;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[path = "watcher_other.rs"]
mod platform;
pub mod service;
pub mod store;
pub mod watch;

pub use service::LearningService;

/// Asks the focused app to show its text to the platform's accessibility
/// API, so a watch after the coming paste can read it (Electron and Chromium
/// apps hide it until asked). Runs on its own thread: the call waits on another process.
pub fn prepare_focused_app() {
    std::thread::spawn(platform::expose_focused_app_text);
}

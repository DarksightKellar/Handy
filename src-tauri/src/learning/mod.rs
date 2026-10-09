//! The learning layer: Handy learns from the corrections made to pasted text.
//! See `docs/learning/DESIGN.md`.

// Pieces of the layer that later tasks connect (the watcher, the frontend) are
// not all called yet.
#![allow(dead_code)]

pub mod commands;
pub mod core;
pub mod notice;
#[path = "watcher_other.rs"]
mod platform;
pub mod service;
pub mod store;

pub use service::LearningService;

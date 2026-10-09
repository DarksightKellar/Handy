//! Pure learning logic. It imports no tauri, rusqlite, reqwest or objc2 code, so
//! every test here runs with plain values and fakes.

pub mod diff;
pub mod learner;
pub mod region;
pub mod rules;
pub mod sound;
pub mod text;

//! The learning layer: Handy learns from the corrections made to pasted text.
//! See `docs/learning/DESIGN.md`.

// Task 5 wires this module into the rest of the app; until then nothing outside
// the module (and its tests) calls it.
#![allow(dead_code)]

pub mod core;

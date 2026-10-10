//! Text-box access on platforms without a watcher. The learning layer still
//! learns from edits made in the history window; nothing watches a text box.

use super::watch::{FieldReader, NoField};

/// The focused text box. No platform support here, so there never is one.
pub fn focused_field() -> Result<Box<dyn FieldReader>, NoField> {
    Err(NoField::Unsupported)
}

/// The app in front, for log lines. Not known here.
pub fn front_app_name() -> String {
    "unknown app".to_string()
}

/// Writes a swap into the focused text box. Not supported here.
pub fn replace_in_focused(
    _region: &str,
    _find: &str,
    _replace_with: &str,
    _nth: usize,
) -> Result<(), String> {
    Err("Swapping in the text box is not supported on this platform".to_string())
}

/// Asks the focused app to expose its text. Nothing to do here.
pub fn expose_focused_app_text() {}

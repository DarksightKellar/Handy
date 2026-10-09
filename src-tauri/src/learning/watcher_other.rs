//! Text-box access on platforms without a watcher. The learning layer still
//! learns from edits made in the history window; nothing watches a text box.

/// Writes a swap into the focused text box. Not supported here.
pub fn replace_in_focused(
    _region: &str,
    _find: &str,
    _replace_with: &str,
    _nth: usize,
) -> Result<(), String> {
    Err("Swapping in the text box is not supported on this platform".to_string())
}

//! Events the learning layer sends to the notice panel (Task 8).
//!
//! The panel only shows them; every decision is made in Rust. The window that
//! hosts the panel is in `notice_window.rs`.

use serde::{Deserialize, Serialize};
use specta::Type;

/// One thing the user should be told about. Serialises with a `kind` tag, for
/// example `{"kind":"Learned","rule_id":3,"from":"Calvin","to":"Kelvin"}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(tag = "kind")]
pub enum LearningNotice {
    /// A new rule was learned from an edit. The panel offers Undo
    /// (`learning_undo(rule_id)`).
    Learned {
        rule_id: i64,
        from: String,
        to: String,
    },
    /// A rule was ambiguous and Handy pasted `chosen`. The panel offers
    /// `alternative` (`learning_swap(session_id, application_index)`).
    Alternative {
        session_id: u64,
        application_index: usize,
        chosen: String,
        alternative: String,
    },
    /// A swap could not be written into the text box. `alternative` is on the
    /// clipboard instead.
    SwapFailed { alternative: String },
}

/// Where notices go. The app handle is the real sink; tests use a list.
pub trait NoticeSink {
    fn notice(&self, notice: LearningNotice);
}

impl NoticeSink for tauri::AppHandle {
    /// Sends the notice to the windows and shows the notice panel.
    fn notice(&self, notice: LearningNotice) {
        super::notice_window::dispatch(self, notice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_serialise_with_a_kind_tag() {
        let learned = serde_json::to_value(LearningNotice::Learned {
            rule_id: 3,
            from: "Calvin".into(),
            to: "Kelvin".into(),
        })
        .unwrap();
        assert_eq!(
            learned,
            serde_json::json!({"kind":"Learned","rule_id":3,"from":"Calvin","to":"Kelvin"})
        );

        let alt = serde_json::to_value(LearningNotice::Alternative {
            session_id: 7,
            application_index: 1,
            chosen: "Eres".into(),
            alternative: "heiress".into(),
        })
        .unwrap();
        assert_eq!(
            alt,
            serde_json::json!({"kind":"Alternative","session_id":7,"application_index":1,
                "chosen":"Eres","alternative":"heiress"})
        );

        let failed = serde_json::to_value(LearningNotice::SwapFailed {
            alternative: "heiress".into(),
        })
        .unwrap();
        assert_eq!(
            failed,
            serde_json::json!({"kind":"SwapFailed","alternative":"heiress"})
        );
    }
}

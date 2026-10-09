//! Tauri commands for the "Learned" settings page, history edits, Undo and Swap.

use std::sync::Arc;

use tauri::{AppHandle, Manager};

use super::core::learner::LearnEvent;
use super::core::rules::Rule;
use super::core::text::tokenize;
use super::service::LearningService;
use crate::managers::history::{HistoryEntry, HistoryManager};
use crate::settings;

/// A rule side may hold at most this many words.
const MAX_PHRASE_WORDS: usize = 4;

fn service(app: &AppHandle) -> Result<Arc<LearningService>, String> {
    app.try_state::<Arc<LearningService>>()
        .map(|s| Arc::clone(&s))
        .ok_or_else(|| "The learning layer is not available".to_string())
}

/// The text a history edit is measured against: what was pasted, or for an
/// older entry the best text Handy has.
pub fn history_baseline(entry: &HistoryEntry) -> &str {
    entry
        .pasted_text
        .as_deref()
        .or(entry.post_processed_text.as_deref())
        .unwrap_or(&entry.transcription_text)
}

/// Checks the text of one side of a rule. Returns it trimmed.
fn valid_side<'a>(text: &'a str, name: &str) -> Result<&'a str, String> {
    let text = text.trim();
    let words = tokenize(text).len();
    if words == 0 {
        return Err(format!("The \"{name}\" text is empty"));
    }
    if words > MAX_PHRASE_WORDS {
        return Err(format!(
            "The \"{name}\" text has more than {MAX_PHRASE_WORDS} words"
        ));
    }
    Ok(text)
}

#[tauri::command]
#[specta::specta]
pub async fn learning_list_rules(app: AppHandle) -> Result<Vec<Rule>, String> {
    service(&app)?.store().all().map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn learning_update_rule(
    app: AppHandle,
    id: i64,
    from: String,
    to: String,
    paused: bool,
) -> Result<Rule, String> {
    let from = valid_side(&from, "from")?;
    let to = valid_side(&to, "to")?;
    service(&app)?
        .store()
        .update(id, from, to, paused, chrono::Utc::now().timestamp())
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn learning_delete_rule(app: AppHandle, id: i64) -> Result<(), String> {
    service(&app)?.store().delete(id).map_err(|e| e.to_string())
}

/// Undo of a "Learned" notice: removes the rule that was just learned.
#[tauri::command]
#[specta::specta]
pub async fn learning_undo(app: AppHandle, rule_id: i64) -> Result<(), String> {
    service(&app)?
        .store()
        .delete(rule_id)
        .map_err(|e| e.to_string())
}

/// Swaps one word of the last paste for its other option. On failure the other
/// option is on the clipboard and a `SwapFailed` notice was sent.
#[tauri::command]
#[specta::specta]
pub async fn learning_swap(
    app: AppHandle,
    session_id: u64,
    application_index: usize,
) -> Result<(), String> {
    let service = service(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        service.swap_in_app(&app, session_id, application_index)
    })
    .await
    .map_err(|e| format!("Swap task failed: {e}"))?
}

/// Learns from an edit made to a history entry in the history window, and
/// stores the edit on the entry. Returns the rules this edit created.
#[tauri::command]
#[specta::specta]
pub async fn learning_from_history_edit(
    app: AppHandle,
    entry_id: i64,
    edited: String,
) -> Result<Vec<Rule>, String> {
    let history = app
        .try_state::<Arc<HistoryManager>>()
        .map(|h| Arc::clone(&h))
        .ok_or_else(|| "History is not available".to_string())?;
    let entry = history
        .get_entry_by_id(entry_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("History entry {entry_id} not found"))?;

    let baseline = history_baseline(&entry).to_string();
    // The same edit saved twice, or no edit at all, teaches nothing new.
    if entry.edited_text.as_deref() == Some(edited.as_str()) || edited == baseline {
        return Ok(Vec::new());
    }

    let mut learned = Vec::new();
    if settings::get_settings(&app).learning_enabled {
        let service = service(&app)?;
        let sink = app.clone();
        let edited_for_learning = edited.clone();
        let events = tauri::async_runtime::spawn_blocking(move || {
            service.record_history_edit(&sink, &baseline, &edited_for_learning)
        })
        .await
        .map_err(|e| format!("Learning task failed: {e}"))?;
        learned = events
            .into_iter()
            .filter_map(|event| match event {
                LearnEvent::Learned(rule) => Some(rule),
                _ => None,
            })
            .collect();
    }

    history
        .set_edited_text(entry_id, &edited)
        .map_err(|e| e.to_string())?;
    Ok(learned)
}

// ---- settings --------------------------------------------------------------

#[tauri::command]
#[specta::specta]
pub fn change_learning_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.learning_enabled = enabled;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_learning_watch_seconds_setting(app: AppHandle, seconds: u32) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.learning_watch_seconds = seconds.clamp(5, 120);
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_jev_api_key_setting(app: AppHandle, key: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.jev_api_key = key.trim().into();
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_jev_confidence_threshold_setting(
    app: AppHandle,
    threshold: f64,
) -> Result<(), String> {
    if !threshold.is_finite() {
        return Err("The threshold must be a number".to_string());
    }
    let mut settings = settings::get_settings(&app);
    settings.jev_confidence_threshold = threshold.clamp(0.5, 0.99);
    settings::write_settings(&app, settings);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        transcription: &str,
        post_processed: Option<&str>,
        pasted: Option<&str>,
    ) -> HistoryEntry {
        HistoryEntry {
            id: 1,
            file_name: "handy-1.wav".into(),
            timestamp: 0,
            saved: false,
            title: "Recording".into(),
            transcription_text: transcription.into(),
            post_processed_text: post_processed.map(Into::into),
            post_process_prompt: None,
            post_process_requested: false,
            pasted_text: pasted.map(Into::into),
            edited_text: None,
        }
    }

    #[test]
    fn history_baseline_prefers_the_pasted_text() {
        assert_eq!(
            history_baseline(&entry("raw", Some("polished"), Some("pasted"))),
            "pasted"
        );
        assert_eq!(
            history_baseline(&entry("raw", Some("polished"), None)),
            "polished"
        );
        assert_eq!(history_baseline(&entry("raw", None, None)), "raw");
    }

    #[test]
    fn rule_sides_are_trimmed_and_limited_to_four_words() {
        assert_eq!(valid_side("  get hub ", "from"), Ok("get hub"));
        assert!(valid_side("   ", "from").is_err());
        assert!(valid_side("a b c d", "to").is_ok());
        assert!(valid_side("a b c d e", "to").is_err());
    }
}

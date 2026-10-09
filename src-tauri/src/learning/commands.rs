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

/// The text a history edit is learned against. The edit box starts from the
/// entry's earlier edit, so the new edit is measured from that text. Measured
/// from the paste, every save would count the earlier fixes a second time.
pub fn history_learning_baseline(entry: &HistoryEntry) -> &str {
    entry
        .edited_text
        .as_deref()
        .unwrap_or_else(|| history_baseline(entry))
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
    // What the edit box started from: the earlier edit, if there is one.
    let learn_from = history_learning_baseline(&entry).to_string();
    // The same edit saved twice teaches nothing new.
    if entry.edited_text.as_deref() == Some(edited.as_str()) {
        return Ok(Vec::new());
    }
    // An edit back to the pasted text teaches nothing, but it removes the old
    // edit, so History shows what is now true.
    if edited == baseline {
        if entry.edited_text.is_some() {
            history
                .clear_edited_text(entry_id)
                .map_err(|e| e.to_string())?;
        }
        return Ok(Vec::new());
    }

    let mut learned = Vec::new();
    if settings::get_settings(&app).learning_enabled {
        let service = service(&app)?;
        let sink = app.clone();
        let edited_for_learning = edited.clone();
        let events = tauri::async_runtime::spawn_blocking(move || {
            service.record_history_edit(&sink, &learn_from, &edited_for_learning)
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

    struct NoSink;
    impl crate::learning::notice::NoticeSink for NoSink {
        fn notice(&self, _: crate::learning::notice::LearningNotice) {}
    }

    #[test]
    fn a_history_edit_is_learned_against_the_earlier_edit() {
        let mut e = entry("raw", None, Some("Calvin met heiress."));
        assert_eq!(history_learning_baseline(&e), "Calvin met heiress.");
        e.edited_text = Some("Kelvin met heiress.".into());
        assert_eq!(history_learning_baseline(&e), "Kelvin met heiress.");
        // The "back to the paste" check still compares with the paste.
        assert_eq!(history_baseline(&e), "Calvin met heiress.");
    }

    #[test]
    fn two_successive_history_edits_count_a_fix_once() {
        use crate::learning::service::LearningService;
        use crate::learning::store::RuleStore;
        let service =
            LearningService::new(RuleStore::open_in_memory().unwrap(), Box::new(|_| None));
        let mut e = entry("raw", None, Some("Calvin met a ghost on Monday."));

        // First save: Calvin becomes Kelvin.
        let first = "Kelvin met a ghost on Monday.";
        service.record_history_edit(&NoSink, history_learning_baseline(&e), first);
        e.edited_text = Some(first.into());
        // Second save starts from the first edit and fixes one more word.
        let second = "Kelvin met a guest on Monday.";
        service.record_history_edit(&NoSink, history_learning_baseline(&e), second);

        let rules = service.store().all().unwrap();
        let calvin = rules.iter().find(|r| r.from == "Calvin").unwrap();
        assert_eq!(
            (calvin.hits, calvin.misses),
            (1, 0),
            "the first fix is counted once"
        );
    }

    #[test]
    fn two_successive_history_edits_that_repeat_a_known_rule_count_it_once() {
        use crate::learning::service::LearningService;
        use crate::learning::store::RuleStore;
        let service =
            LearningService::new(RuleStore::open_in_memory().unwrap(), Box::new(|_| None));
        // A rule that is already known: each edit that makes it is a hit.
        service.record_history_edit(&NoSink, "send it to Calvin", "send it to Kelvin");
        let mut e = entry("raw", None, Some("Calvin met heiress and Calvin left."));
        let first = "Kelvin met heiress and Kelvin left.";
        service.record_history_edit(&NoSink, history_learning_baseline(&e), first);
        e.edited_text = Some(first.into());
        service.record_history_edit(
            &NoSink,
            history_learning_baseline(&e),
            "Kelvin met Eres and Kelvin left.",
        );
        let rules = service.store().all().unwrap();
        let calvin = rules.iter().find(|r| r.from == "Calvin").unwrap();
        // 1 at birth, 2 for the two Calvins fixed in the first edit, none for
        // the second edit.
        assert_eq!(calvin.hits, 3);
    }
}

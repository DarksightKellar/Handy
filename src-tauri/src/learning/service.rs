//! The learning service: joins the core, the store, the decider and the
//! notices. It lives in Tauri state as `Arc<LearningService>`.
//!
//! Every method that touches the outside world takes its dependencies as
//! arguments (settings, a notice sink, a text writer), so the logic runs in
//! unit tests with fakes. The glue that holds an `AppHandle` is the thin
//! `*_app` layer at the bottom.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::Result;
use log::{debug, warn};
use tauri::{AppHandle, Manager};

use super::core::learner::{self, Application, Applied, Decider, LearnEvent, Observation};
use super::core::rules::{Rule, Verdict};
use super::core::text::{normalize_phrase, tokenize};
use super::jev::JevDecider;
use super::notice::{LearningNotice, NoticeSink};
use super::store::RuleStore;
use super::watch::{spawn_watch_after, WatchHandle, WatchTiming};
use crate::managers::history::HistoryManager;
use crate::settings::{get_settings, AppSettings};

/// Builds a decider for an API key. `None` means there is no decider.
pub type DeciderFactory = Box<dyn Fn(&str) -> Option<SharedDecider> + Send + Sync>;

pub type SharedDecider = Arc<dyn Decider + Send + Sync>;

/// Seconds a swap notice stays visible, and so how long the swap shortcut stays
/// registered.
pub const SWAP_NOTICE_SECS: u64 = 6;

/// Words of learned vocabulary fed to a Whisper prompt.
pub const PROMPT_WORD_BUDGET: usize = 150;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// One paste: what was pasted and which learned fixes it contains.
#[derive(Clone, Debug)]
pub struct PasteSession {
    pub id: u64,
    pub pasted: String,
    pub applications: Vec<Application>,
    pub history_id: Option<i64>,
    /// Applications the user already settled with a swap.
    settled: HashSet<usize>,
}

/// How a swap changes what the rule has learned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapOutcome {
    /// The user took the rule's `to`.
    ToRule,
    /// The user took the original text.
    ToOriginal,
}

/// What to write into the text box for a swap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapPlan {
    /// The text that was pasted, to find the paste in the text box.
    pub region: String,
    /// The text to replace (the pasted choice).
    pub find: String,
    /// Which occurrence of `find` inside `region`, counting from 0.
    pub nth: usize,
    /// The other option.
    pub replace_with: String,
    pub outcome: SwapOutcome,
}

impl PasteSession {
    pub fn new(
        id: u64,
        pasted: String,
        applications: Vec<Application>,
        history_id: Option<i64>,
    ) -> Self {
        Self {
            id,
            pasted,
            applications,
            history_id,
            settled: HashSet::new(),
        }
    }

    /// The first application that still has a swap to offer.
    pub fn next_swap(&self) -> Option<usize> {
        self.applications
            .iter()
            .enumerate()
            .find(|(i, a)| a.alternative.is_some() && !self.settled.contains(i))
            .map(|(i, _)| i)
    }

    /// The applications the user did not settle by hand.
    fn live_applications(&self) -> Vec<Application> {
        self.applications
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.settled.contains(i))
            .map(|(_, a)| a.clone())
            .collect()
    }

    pub fn plan_swap(&self, index: usize) -> Result<SwapPlan, String> {
        let app = self
            .applications
            .get(index)
            .ok_or_else(|| format!("No learned word at position {index}"))?;
        let alternative = app
            .alternative
            .as_ref()
            .ok_or_else(|| "This word has no other option".to_string())?;
        if self.settled.contains(&index) {
            return Err("This word was already swapped".to_string());
        }
        if self.pasted.get(app.start..app.end) != Some(app.chosen.as_str()) {
            return Err("The pasted text no longer matches".to_string());
        }
        let nth = self.pasted[..app.start]
            .matches(app.chosen.as_str())
            .count();
        let outcome = if normalize_phrase(alternative) == normalize_phrase(&app.original) {
            SwapOutcome::ToOriginal
        } else {
            SwapOutcome::ToRule
        };
        Ok(SwapPlan {
            region: self.pasted.clone(),
            find: app.chosen.clone(),
            nth,
            replace_with: alternative.clone(),
            outcome,
        })
    }

    /// Records that the swap at `index` was written: the session text now holds
    /// the other option, later applications move along, and the application is
    /// settled so the end of the watch does not count it a second time.
    fn complete_swap(&mut self, index: usize) {
        let Some(app) = self.applications.get(index).cloned() else {
            return;
        };
        let Some(alternative) = app.alternative.clone() else {
            return;
        };
        self.pasted.replace_range(app.start..app.end, &alternative);
        let delta = alternative.len() as isize - (app.end - app.start) as isize;
        for (i, other) in self.applications.iter_mut().enumerate() {
            if i == index {
                other.end = other.start + alternative.len();
                other.alternative = Some(std::mem::replace(&mut other.chosen, alternative.clone()));
            } else if other.start >= app.end {
                other.start = other.start.saturating_add_signed(delta);
                other.end = other.end.saturating_add_signed(delta);
            }
        }
        self.settled.insert(index);
    }
}

pub struct LearningService {
    store: RuleStore,
    decider_factory: DeciderFactory,
    /// The decider for the last API key seen, so the HTTP client is reused.
    decider_cache: Mutex<Option<(String, Option<SharedDecider>)>>,
    next_session: AtomicU64,
    last: Mutex<Option<Arc<Mutex<PasteSession>>>>,
    /// The watch of the last paste, while it runs.
    watch: Mutex<Option<WatchHandle>>,
    /// Bumped each time the swap shortcut is requested or released, so a
    /// timer from an older notice does not release a newer one.
    swap_generation: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl LearningService {
    pub fn new(store: RuleStore, decider_factory: DeciderFactory) -> Self {
        Self {
            store,
            decider_factory,
            decider_cache: Mutex::new(None),
            next_session: AtomicU64::new(1),
            last: Mutex::new(None),
            watch: Mutex::new(None),
            swap_generation: AtomicU64::new(0),
        }
    }

    /// Opens the service on the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self::new(RuleStore::open(path)?, default_decider_factory()))
    }

    pub fn store(&self) -> &RuleStore {
        &self.store
    }

    pub fn new_session(
        &self,
        pasted: String,
        applications: Vec<Application>,
        history_id: Option<i64>,
    ) -> PasteSession {
        let id = self.next_session.fetch_add(1, Ordering::Relaxed);
        PasteSession::new(id, pasted, applications, history_id)
    }

    fn decider_for(&self, key: &str) -> Option<SharedDecider> {
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        let mut cache = lock(&self.decider_cache);
        if let Some((cached_key, decider)) = cache.as_ref() {
            if cached_key == key {
                return decider.clone();
            }
        }
        let decider = (self.decider_factory)(key);
        *cache = Some((key.to_string(), decider.clone()));
        decider
    }

    // ---- applying rules -------------------------------------------------

    /// Applies the learned rules to `text`. It fails open: when the rules
    /// cannot be read, the text comes back unchanged.
    ///
    /// This may call the decider, which blocks on the network for up to its
    /// timeout, so call it off the async runtime (`spawn_blocking`).
    pub fn apply(&self, settings: &AppSettings, text: &str) -> Applied {
        let decider = self.decider_for(&settings.jev_api_key);
        self.apply_with(
            text,
            decider.as_deref().map(|d| d as &dyn Decider),
            settings.jev_confidence_threshold,
        )
    }

    pub fn apply_with(
        &self,
        text: &str,
        decider: Option<&dyn Decider>,
        jev_threshold: f64,
    ) -> Applied {
        match self.store.all() {
            Ok(rules) => learner::apply(text, &rules, decider, jev_threshold),
            Err(e) => {
                warn!("Could not read learned rules; pasting the text unchanged: {e}");
                Applied {
                    text: text.to_string(),
                    applications: Vec::new(),
                }
            }
        }
    }

    /// The distinct learned words for a Whisper prompt, most used first, within
    /// `budget_words` words. Paused rules and rules that are mostly rejected
    /// are left out.
    pub fn prompt_words(&self, budget_words: usize) -> Vec<String> {
        let mut rules = match self.store.all() {
            Ok(rules) => rules,
            Err(e) => {
                warn!("Could not read learned rules for the prompt: {e}");
                return Vec::new();
            }
        };
        rules.retain(|r| matches!(r.verdict(), Verdict::Apply));
        // Stable: equal usage keeps the store's newest-first order.
        rules.sort_by_key(|r| std::cmp::Reverse(r.hits + r.applied));

        let mut seen = HashSet::new();
        let mut used = 0;
        let mut words = Vec::new();
        for rule in rules {
            let count = tokenize(&rule.to).len();
            if count == 0 || used + count > budget_words {
                continue;
            }
            if seen.insert(normalize_phrase(&rule.to)) {
                used += count;
                words.push(rule.to);
            }
        }
        words
    }

    // ---- learning from edits ---------------------------------------------

    /// Learns from an edit of pasted text. New rules are stored and announced;
    /// existing rules get a hit or a miss.
    pub fn record_edit(
        &self,
        sink: &dyn NoticeSink,
        pasted: &str,
        edited: &str,
        applications: &[Application],
    ) -> Vec<LearnEvent> {
        match self.store.all() {
            Ok(rules) => {
                let observations = learner::observe(pasted, edited, applications, &rules);
                self.record_observations(sink, observations)
            }
            Err(e) => {
                warn!("Could not read learned rules to learn from an edit: {e}");
                Vec::new()
            }
        }
    }

    /// Learns from an edit made in the history window. Handy no longer knows
    /// which fixes it made in that text, so it looks for the text of its rules
    /// in the paste: an edit that puts a rule's original back is a miss.
    /// Fixes the edit left alone are not counted as hits: the rule's text may
    /// have been in the paste for another reason.
    pub fn record_history_edit(
        &self,
        sink: &dyn NoticeSink,
        baseline: &str,
        edited: &str,
    ) -> Vec<LearnEvent> {
        let rules = match self.store.all() {
            Ok(rules) => rules,
            Err(e) => {
                warn!("Could not read learned rules to learn from a history edit: {e}");
                return Vec::new();
            }
        };
        let guessed = guess_applications(baseline, &rules);
        let guessed_ids: HashSet<i64> = guessed.iter().map(|a| a.rule_id).collect();
        let observations = learner::observe(baseline, edited, &guessed, &rules)
            .into_iter()
            .filter(|o| !matches!(o, Observation::Hit(id) if guessed_ids.contains(id)))
            .collect();
        self.record_observations(sink, observations)
    }

    fn record_observations(
        &self,
        sink: &dyn NoticeSink,
        observations: Vec<Observation>,
    ) -> Vec<LearnEvent> {
        let at = now();
        let mut events = Vec::new();
        let mut stored_new = HashSet::new();
        for observation in observations {
            match observation {
                Observation::New(new_rule) => {
                    let key = (
                        normalize_phrase(&new_rule.from),
                        normalize_phrase(&new_rule.to),
                    );
                    if !stored_new.insert(key) {
                        continue;
                    }
                    match self.store.insert(&new_rule, at) {
                        Ok(rule) => {
                            debug!("Learned a rule: id {}", rule.id);
                            sink.notice(LearningNotice::Learned {
                                rule_id: rule.id,
                                from: rule.from.clone(),
                                to: rule.to.clone(),
                            });
                            events.push(LearnEvent::Learned(rule));
                        }
                        Err(e) => warn!("Could not store a learned rule: {e}"),
                    }
                }
                Observation::Hit(rule_id) => match self.store.hit(rule_id, at) {
                    Ok(()) => events.push(LearnEvent::Reinforced { rule_id }),
                    Err(e) => warn!("Could not record a hit: {e}"),
                },
                Observation::Miss(rule_id) => match self.store.miss(rule_id, at) {
                    Ok(()) => events.push(LearnEvent::Rejected { rule_id }),
                    Err(e) => warn!("Could not record a miss: {e}"),
                },
            }
        }
        events
    }

    /// Ends a watch: learns from the last good reading of the pasted region.
    /// `region` is `None` when the watch saw nothing to learn from.
    pub fn conclude_watch(
        &self,
        sink: &dyn NoticeSink,
        session: &Mutex<PasteSession>,
        region: Option<&str>,
    ) -> Vec<LearnEvent> {
        let Some(edited) = region else {
            return Vec::new();
        };
        let (baseline, applications) = {
            let s = lock(session);
            (s.pasted.clone(), s.live_applications())
        };
        self.record_edit(sink, &baseline, edited, &applications)
    }

    // ---- swapping ---------------------------------------------------------

    /// The session of the last paste, when `session_id` is still it.
    fn session(&self, session_id: u64) -> Result<Arc<Mutex<PasteSession>>, String> {
        lock(&self.last)
            .as_ref()
            .filter(|s| lock(s).id == session_id)
            .cloned()
            .ok_or_else(|| "This notice has expired".to_string())
    }

    /// The last paste and its next swap, for the swap shortcut.
    pub fn next_swap(&self) -> Option<(u64, usize)> {
        let last = lock(&self.last).clone()?;
        let session = lock(&last);
        session.next_swap().map(|i| (session.id, i))
    }

    /// Swaps the pasted choice for the other option.
    ///
    /// `write` puts the swap into the text box; `copy` is the fallback when it
    /// cannot. On success the rule learns from the choice: taking the rule's
    /// text is a hit, taking the original is a miss.
    pub fn run_swap(
        &self,
        sink: &dyn NoticeSink,
        write: &dyn Fn(&SwapPlan) -> Result<(), String>,
        copy: &dyn Fn(&str) -> Result<(), String>,
        session_id: u64,
        application_index: usize,
    ) -> Result<(), String> {
        let session = self.session(session_id)?;
        let plan = lock(&session).plan_swap(application_index)?;
        match write(&plan) {
            Ok(()) => {
                let rule_id = {
                    let mut s = lock(&session);
                    let id = s.applications[application_index].rule_id;
                    s.complete_swap(application_index);
                    id
                };
                let result = match plan.outcome {
                    SwapOutcome::ToRule => self.store.hit(rule_id, now()),
                    SwapOutcome::ToOriginal => self.store.miss(rule_id, now()),
                };
                if let Err(e) = result {
                    warn!("Could not record the result of a swap: {e}");
                }
                Ok(())
            }
            Err(e) => {
                warn!("Swap failed: {e}");
                if let Err(copy_error) = copy(&plan.replace_with) {
                    warn!("Could not copy the other option either: {copy_error}");
                }
                sink.notice(LearningNotice::SwapFailed {
                    alternative: plan.replace_with.clone(),
                });
                Err(e)
            }
        }
    }

    // ---- sessions ---------------------------------------------------------

    /// Makes `session` the current paste and returns a handle to it.
    pub fn begin_session(&self, session: PasteSession) -> Arc<Mutex<PasteSession>> {
        let session = Arc::new(Mutex::new(session));
        *lock(&self.last) = Some(Arc::clone(&session));
        session
    }

    /// Counts a use of every rule applied in a paste, and tells the sink about
    /// each choice the user can still change.
    pub fn announce_paste(&self, sink: &dyn NoticeSink, session: &PasteSession) {
        for app in &session.applications {
            if let Err(e) = self.store.applied(app.rule_id) {
                warn!("Could not count a rule use: {e}");
            }
        }
        for (index, app) in session.applications.iter().enumerate() {
            if let Some(alternative) = &app.alternative {
                sink.notice(LearningNotice::Alternative {
                    session_id: session.id,
                    application_index: index,
                    chosen: app.chosen.clone(),
                    alternative: alternative.clone(),
                });
            }
        }
    }

    /// Notes that the swap shortcut was requested; returns the generation to
    /// pass to [`LearningService::swap_release_is_current`].
    pub fn swap_requested(&self) -> u64 {
        self.swap_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Whether no newer request has replaced `generation`. Releasing also
    /// counts as a request, so a stale timer finds a newer generation.
    pub fn swap_release_is_current(&self, generation: u64) -> bool {
        self.swap_generation.load(Ordering::SeqCst) == generation
    }
}

/// The decider factory for the app: Jev, for a non-empty API key.
pub fn default_decider_factory() -> DeciderFactory {
    Box::new(|key| JevDecider::new(key).map(|d| Arc::new(d) as SharedDecider))
}

/// Rules whose `to` appears in `text`, as if Handy had just applied them. This
/// is the nearest thing to the original applications once the paste is gone.
fn guess_applications(text: &str, rules: &[Rule]) -> Vec<Application> {
    let tokens = tokenize(text);
    let mut out = Vec::new();
    for rule in rules
        .iter()
        .filter(|r| !matches!(r.verdict(), Verdict::Keep))
    {
        let words: Vec<String> = tokenize(&rule.to).into_iter().map(|t| t.norm).collect();
        if words.is_empty() {
            continue;
        }
        for i in 0..tokens.len() {
            if i + words.len() > tokens.len()
                || !tokens[i..i + words.len()]
                    .iter()
                    .zip(&words)
                    .all(|(t, w)| &t.norm == w)
            {
                continue;
            }
            let (first, last) = (&tokens[i], &tokens[i + words.len() - 1]);
            let raw = &text[first.start..last.end];
            let lead = raw.len() - raw.trim_start_matches(|c: char| !c.is_alphanumeric()).len();
            let trail = raw.len() - raw.trim_end_matches(|c: char| !c.is_alphanumeric()).len();
            let (start, end) = (first.start + lead, last.end - trail);
            out.push(Application {
                rule_id: rule.id,
                start,
                end,
                original: rule.from.clone(),
                chosen: text[start..end].to_string(),
                alternative: None,
            });
        }
    }
    out
}

// ---- glue that holds an AppHandle -------------------------------------------

impl LearningService {
    /// Call right after a successful paste. It returns at once: the caller is
    /// the main thread, and everything slow runs on worker threads.
    pub fn after_paste(self: &Arc<Self>, app: &AppHandle, session: PasteSession) {
        let pasted = session.pasted.clone();
        let handle = self.begin_session(session);
        let snapshot = lock(&handle).clone();

        let service = Arc::clone(self);
        let announce_app = app.clone();
        std::thread::spawn(move || {
            service.announce_paste(&announce_app, &snapshot);
            if snapshot.next_swap().is_some() {
                service.show_swap_shortcut(&announce_app);
            }
        });

        self.start_watch(app, handle, pasted);
    }

    /// Watches the text box that received the paste. The watch of the paste
    /// before this one ends first and finishes learning before the new one
    /// reads anything, so the two never mix their texts.
    fn start_watch(
        self: &Arc<Self>,
        app: &AppHandle,
        session: Arc<Mutex<PasteSession>>,
        pasted: String,
    ) {
        let seconds = get_settings(app).learning_watch_seconds;
        let timing = WatchTiming::new(Duration::from_secs(u64::from(seconds)));

        let previous = lock(&self.watch).take();
        let service = Arc::clone(self);
        let app = app.clone();
        let handle = spawn_watch_after(
            previous,
            super::platform::focused_field,
            pasted,
            timing,
            crate::secure_input::is_enabled_now,
            move |region| service.finish_watch(&app, &session, region),
        );
        *lock(&self.watch) = Some(handle);
    }

    /// The end of a watch: learns from what the box held, and keeps the user's
    /// corrected text on the history entry.
    fn finish_watch(&self, app: &AppHandle, session: &Mutex<PasteSession>, region: Option<String>) {
        let events = self.conclude_watch(app, session, region.as_deref());
        let (Some(edited), Some(entry_id)) = (region, lock(session).history_id) else {
            return;
        };
        let changed = edited != lock(session).pasted;
        if !events.is_empty() && changed {
            if let Some(history) = app.try_state::<Arc<HistoryManager>>() {
                if let Err(e) = history.set_edited_text(entry_id, &edited) {
                    warn!("Could not store the edited text on the history entry: {e}");
                }
            }
        }
    }

    /// Registers the swap shortcut for as long as a swap notice is visible.
    fn show_swap_shortcut(self: &Arc<Self>, app: &AppHandle) {
        crate::shortcut::register_swap_shortcut(app);
        let generation = self.swap_requested();
        let service = Arc::clone(self);
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(SWAP_NOTICE_SECS));
            if service.swap_release_is_current(generation) {
                crate::shortcut::unregister_swap_shortcut(&app);
            }
        });
    }

    fn release_swap_shortcut(&self, app: &AppHandle) {
        self.swap_requested(); // makes any running timer stale
        crate::shortcut::unregister_swap_shortcut(app);
    }

    /// Swaps one word of the last paste in the focused text box. It blocks on
    /// Accessibility calls, so run it off the main thread and off the async
    /// runtime. When the swap cannot be written, the other option goes to the
    /// clipboard and a `SwapFailed` notice is sent.
    pub fn swap_in_app(
        self: &Arc<Self>,
        app: &AppHandle,
        session_id: u64,
        application_index: usize,
    ) -> Result<(), String> {
        let result = self.run_swap(
            app,
            &|plan| {
                super::platform::replace_in_focused(
                    &plan.region,
                    &plan.find,
                    &plan.replace_with,
                    plan.nth,
                )
            },
            &|text| crate::clipboard::write_text_to_clipboard(app, text),
            session_id,
            application_index,
        );
        if result.is_err() || self.next_swap().is_none() {
            self.release_swap_shortcut(app);
        }
        result
    }

    /// The swap shortcut: swaps the next word that still has an option.
    pub fn swap_next_in_app(self: &Arc<Self>, app: &AppHandle) {
        let Some((session_id, index)) = self.next_swap() else {
            debug!("Swap shortcut pressed with nothing to swap");
            self.release_swap_shortcut(app);
            return;
        };
        let service = Arc::clone(self);
        let app = app.clone();
        std::thread::spawn(move || {
            if let Err(e) = service.swap_in_app(&app, session_id, index) {
                warn!("Swap by shortcut failed: {e}");
            }
        });
    }
}

/// `custom` words first, then `learned` words that are not already there
/// (ignoring case). The result is the word list of a Whisper prompt.
pub fn merge_prompt_words(custom: &[String], learned: Vec<String>) -> Vec<String> {
    let mut seen: HashSet<String> = custom.iter().map(|w| w.trim().to_lowercase()).collect();
    let mut words = custom.to_vec();
    for word in learned {
        if seen.insert(word.trim().to_lowercase()) {
            words.push(word);
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learning::core::learner::NewRule;
    use crate::learning::core::rules::RuleKind;
    use crate::settings::get_default_settings;

    /// Collects notices.
    #[derive(Default)]
    struct Notices(Mutex<Vec<LearningNotice>>);
    impl NoticeSink for Notices {
        fn notice(&self, notice: LearningNotice) {
            self.0.lock().unwrap().push(notice);
        }
    }
    impl Notices {
        fn all(&self) -> Vec<LearningNotice> {
            self.0.lock().unwrap().clone()
        }
    }

    struct Fixed(Vec<f64>);
    impl Decider for Fixed {
        fn choose(&self, _: &str, _: &[String]) -> Option<Vec<f64>> {
            Some(self.0.clone())
        }
    }

    fn service() -> LearningService {
        LearningService::new(RuleStore::open_in_memory().unwrap(), Box::new(|_| None))
    }

    fn new_rule(from: &str, to: &str) -> NewRule {
        NewRule {
            from: from.into(),
            to: to.into(),
            kind: RuleKind::Word,
            source_sentence: format!("sentence with {to}"),
        }
    }

    fn add(service: &LearningService, from: &str, to: &str) -> Rule {
        service.store().insert(&new_rule(from, to), 1).unwrap()
    }

    #[test]
    fn history_edit_learns_and_next_transcript_is_fixed() {
        let service = service();
        let notices = Notices::default();

        let events =
            service.record_history_edit(&notices, "send it to Calvin.", "send it to Kelvin.");

        assert!(matches!(events.as_slice(), [LearnEvent::Learned(r)] if r.to == "Kelvin"));
        assert!(matches!(
            notices.all().as_slice(),
            [LearningNotice::Learned { from, to, .. }] if from == "Calvin" && to == "Kelvin"
        ));
        let next = service.apply_with("tell Calvin the news", None, 0.8);
        assert_eq!(next.text, "tell Kelvin the news");
    }

    #[test]
    fn the_same_correction_twice_in_one_edit_is_one_rule() {
        let service = service();
        let notices = Notices::default();

        service.record_history_edit(&notices, "Calvin met Calvin", "Kelvin met Kelvin");

        assert_eq!(service.store().all().unwrap().len(), 1);
        assert_eq!(notices.all().len(), 1);
    }

    #[test]
    fn a_change_of_mind_in_a_history_edit_is_not_learned() {
        let service = service();
        let notices = Notices::default();
        let events = service.record_history_edit(&notices, "meet on Tuesday", "meet on Friday");
        assert!(events.is_empty());
        assert!(service.store().all().unwrap().is_empty());
    }

    #[test]
    fn putting_a_rules_original_back_in_history_is_a_miss() {
        let service = service();
        let rule = add(&service, "heiress", "Eres");
        let notices = Notices::default();

        let events =
            service.record_history_edit(&notices, "the Eres arrived", "the heiress arrived");

        assert_eq!(events, vec![LearnEvent::Rejected { rule_id: rule.id }]);
        let after = service.store().all().unwrap();
        assert_eq!((after[0].hits, after[0].misses), (1, 1));
        assert!(notices.all().is_empty()); // no reverse rule is learned
    }

    #[test]
    fn leaving_text_alone_in_a_history_edit_is_not_a_hit() {
        let service = service();
        let rule = add(&service, "heiress", "Eres");
        let notices = Notices::default();

        // "Eres" is in the text, but another word is fixed.
        service.record_history_edit(&notices, "Eres met Calvin", "Eres met Kelvin");

        let after = service
            .store()
            .all()
            .unwrap()
            .into_iter()
            .find(|r| r.id == rule.id)
            .unwrap();
        assert_eq!(after.hits, 1);
    }

    #[test]
    fn apply_uses_the_decider_when_a_key_is_set() {
        let factory: DeciderFactory = Box::new(|key| {
            assert_eq!(key, "jev-key");
            Some(Arc::new(Fixed(vec![0.9, 0.1])) as Arc<dyn Decider + Send + Sync>)
        });
        let service = LearningService::new(RuleStore::open_in_memory().unwrap(), factory);
        let rule = add(&service, "heiress", "Eres");
        service.store().miss(rule.id, 2).unwrap(); // 1 hit, 1 miss: ambiguous

        let mut settings = get_default_settings();
        settings.jev_api_key = "jev-key".into();
        let applied = service.apply(&settings, "push the fix to heiress");

        assert_eq!(applied.text, "push the fix to Eres");
        assert!(applied.applications[0].alternative.is_none());
    }

    #[test]
    fn the_default_factory_builds_jev_only_for_a_key() {
        let factory = default_decider_factory();
        assert!(factory("jev-key").is_some());
        assert!(factory("").is_none());
    }

    #[test]
    fn apply_without_a_key_offers_the_alternative() {
        let service = service();
        let rule = add(&service, "heiress", "Eres");
        service.store().miss(rule.id, 2).unwrap();

        let applied = service.apply(&get_default_settings(), "push the fix to heiress");

        assert_eq!(
            applied.applications[0].alternative.as_deref(),
            Some("heiress")
        );
    }

    #[test]
    fn the_decider_is_built_once_per_key() {
        let builds = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&builds);
        let factory: DeciderFactory = Box::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Some(Arc::new(Fixed(vec![0.5, 0.5])) as Arc<dyn Decider + Send + Sync>)
        });
        let service = LearningService::new(RuleStore::open_in_memory().unwrap(), factory);
        let mut settings = get_default_settings();
        settings.jev_api_key = "k".into();
        service.apply(&settings, "a");
        service.apply(&settings, "b");
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        settings.jev_api_key = "k2".into();
        service.apply(&settings, "c");
        assert_eq!(builds.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn prompt_words_are_ordered_by_use_and_stay_in_budget() {
        let service = service();
        let a = add(&service, "calvin", "Kelvin");
        let b = add(&service, "heiress", "Eres");
        add(&service, "get hub", "Git Hub");
        for _ in 0..3 {
            service.store().hit(b.id, 2).unwrap();
        }
        service.store().applied(a.id).unwrap();

        assert_eq!(service.prompt_words(150), vec!["Eres", "Kelvin", "Git Hub"]);
        // "Git Hub" is two words, so a budget of 3 leaves it out.
        assert_eq!(service.prompt_words(3), vec!["Eres", "Kelvin"]);
        assert!(service.prompt_words(0).is_empty());
    }

    #[test]
    fn prompt_words_skip_paused_rejected_and_repeated_words() {
        let service = service();
        let paused = add(&service, "a", "Alpha");
        service
            .store()
            .update(paused.id, "a", "Alpha", true, 2)
            .unwrap();
        let rejected = add(&service, "b", "Beta");
        for _ in 0..4 {
            service.store().miss(rejected.id, 2).unwrap();
        }
        add(&service, "c", "Gamma");
        add(&service, "gama", "gamma"); // same word, other case

        assert_eq!(service.prompt_words(150), vec!["gamma"]);
    }

    #[test]
    fn merge_prompt_words_keeps_custom_first_and_drops_repeats() {
        let custom = vec!["Kelvin".to_string(), "Handy".to_string()];
        let learned = vec!["kelvin".to_string(), "Eres".to_string(), "eres".to_string()];
        assert_eq!(
            merge_prompt_words(&custom, learned),
            vec!["Kelvin", "Handy", "Eres"]
        );
        assert!(merge_prompt_words(&[], vec![]).is_empty());
    }

    // ---- sessions and swaps ---------------------------------------------

    /// "push the fix to heiress" with an ambiguous rule that has `misses`
    /// misses. One miss (p = 0.5) pastes "Eres" and offers "heiress"; two
    /// (p = 0.33) paste "heiress" and offer "Eres".
    fn ambiguous_session_with(
        service: &LearningService,
        misses: u32,
    ) -> (i64, Arc<Mutex<PasteSession>>) {
        let rule = add(service, "heiress", "Eres");
        for _ in 0..misses {
            service.store().miss(rule.id, 2).unwrap();
        }
        let applied = service.apply_with("push the fix to heiress", None, 0.8);
        let session = service.new_session(applied.text, applied.applications, None);
        (rule.id, service.begin_session(session))
    }

    fn ambiguous_session(service: &LearningService) -> (i64, Arc<Mutex<PasteSession>>) {
        ambiguous_session_with(service, 1)
    }

    #[test]
    fn announce_paste_counts_uses_and_offers_each_alternative() {
        let service = service();
        let (rule_id, session) = ambiguous_session(&service);
        let notices = Notices::default();
        let session = lock(&session).clone();

        service.announce_paste(&notices, &session);

        let rule = &service.store().all().unwrap()[0];
        assert_eq!((rule.id, rule.applied), (rule_id, 1));
        assert_eq!(
            notices.all(),
            vec![LearningNotice::Alternative {
                session_id: session.id,
                application_index: 0,
                chosen: "Eres".into(),
                alternative: "heiress".into(),
            }]
        );
    }

    #[test]
    fn a_swap_to_the_rules_word_is_a_hit() {
        let service = service();
        let (rule_id, session) = ambiguous_session_with(&service, 2);
        assert_eq!(lock(&session).pasted, "push the fix to heiress");
        let id = lock(&session).id;
        let written = Mutex::new(Vec::new());

        let result = service.run_swap(
            &Notices::default(),
            &|plan| {
                written.lock().unwrap().push(plan.clone());
                Ok(())
            },
            &|_| panic!("no clipboard needed"),
            id,
            0,
        );

        assert!(result.is_ok());
        let plan = written.lock().unwrap()[0].clone();
        assert_eq!(
            (plan.find.as_str(), plan.replace_with.as_str(), plan.nth),
            ("heiress", "Eres", 0)
        );
        assert_eq!(plan.outcome, SwapOutcome::ToRule);
        let rule = service
            .store()
            .all()
            .unwrap()
            .into_iter()
            .find(|r| r.id == rule_id)
            .unwrap();
        assert_eq!((rule.hits, rule.misses), (2, 2));
        assert_eq!(lock(&session).pasted, "push the fix to Eres");
    }

    #[test]
    fn a_swap_back_to_the_original_is_a_miss() {
        let service = service();
        let (rule_id, session) = ambiguous_session(&service);
        let id = lock(&session).id;

        let result = service.run_swap(
            &Notices::default(),
            &|plan| {
                assert_eq!(plan.outcome, SwapOutcome::ToOriginal);
                assert_eq!(
                    (plan.find.as_str(), plan.replace_with.as_str()),
                    ("Eres", "heiress")
                );
                Ok(())
            },
            &|_| panic!("no clipboard needed"),
            id,
            0,
        );

        assert!(result.is_ok());
        let rule = service
            .store()
            .all()
            .unwrap()
            .into_iter()
            .find(|r| r.id == rule_id)
            .unwrap();
        assert_eq!((rule.hits, rule.misses), (1, 2));
    }

    #[test]
    fn a_failed_swap_copies_the_other_option_and_says_so() {
        let service = service();
        let (rule_id, session) = ambiguous_session(&service);
        let id = lock(&session).id;
        let notices = Notices::default();
        let copied = Mutex::new(None);

        let result = service.run_swap(
            &notices,
            &|_| Err("no focused text box".to_string()),
            &|text| {
                *copied.lock().unwrap() = Some(text.to_string());
                Ok(())
            },
            id,
            0,
        );

        assert_eq!(result, Err("no focused text box".to_string()));
        assert_eq!(copied.lock().unwrap().as_deref(), Some("heiress"));
        assert_eq!(
            notices.all(),
            vec![LearningNotice::SwapFailed {
                alternative: "heiress".into()
            }]
        );
        // The rule learns nothing from a swap that did not happen.
        let rule = service
            .store()
            .all()
            .unwrap()
            .into_iter()
            .find(|r| r.id == rule_id)
            .unwrap();
        assert_eq!((rule.hits, rule.misses), (1, 1));
        assert!(lock(&session).next_swap().is_some());
    }

    #[test]
    fn a_swap_of_an_old_session_is_refused() {
        let service = service();
        let (_, first) = ambiguous_session(&service);
        let first_id = lock(&first).id;
        let second = service.new_session("hello".into(), vec![], None);
        service.begin_session(second);

        let result = service.run_swap(
            &Notices::default(),
            &|_| panic!("must not write"),
            &|_| panic!("must not copy"),
            first_id,
            0,
        );
        assert!(result.is_err());
        assert!(service.next_swap().is_none());
    }

    #[test]
    fn a_word_swaps_once() {
        let service = service();
        let (_, session) = ambiguous_session(&service);
        let id = lock(&session).id;
        let ok = |_: &SwapPlan| Ok(());
        assert!(service
            .run_swap(&Notices::default(), &ok, &|_| Ok(()), id, 0)
            .is_ok());
        assert!(service
            .run_swap(&Notices::default(), &ok, &|_| Ok(()), id, 0)
            .is_err());
        assert!(service.next_swap().is_none());
    }

    #[test]
    fn swap_plan_finds_the_right_occurrence_and_later_words_move() {
        let service = service();
        let a = add(&service, "heiress", "Eres");
        let b = add(&service, "calvin", "Kelvin");
        service.store().miss(a.id, 2).unwrap();
        service.store().miss(b.id, 2).unwrap();
        let applied = service.apply_with("Eres met heiress and calvin", None, 0.8);
        // "heiress" and "calvin" are both ambiguous and both pasted as the rule's word.
        assert_eq!(applied.text, "Eres met Eres and Kelvin");
        let mut session = service.new_session(applied.text, applied.applications, None);

        let plan = session.plan_swap(0).unwrap();
        assert_eq!((plan.find.as_str(), plan.nth), ("Eres", 1)); // the second "Eres"

        session.complete_swap(0);
        assert_eq!(session.pasted, "Eres met heiress and Kelvin");
        let second = session.plan_swap(1).unwrap();
        assert_eq!((second.find.as_str(), second.nth), ("Kelvin", 0));
        assert_eq!(
            &session.pasted[session.applications[1].start..session.applications[1].end],
            "Kelvin"
        );
        assert_eq!(session.next_swap(), Some(1));
    }

    // ---- watch results ----------------------------------------------------

    #[test]
    fn a_watch_that_saw_an_edit_learns_it() {
        let service = service();
        let session = Mutex::new(service.new_session("send it to Calvin".into(), vec![], None));
        let notices = Notices::default();

        let events = service.conclude_watch(&notices, &session, Some("send it to Kelvin"));

        assert!(matches!(events.as_slice(), [LearnEvent::Learned(_)]));
        assert_eq!(notices.all().len(), 1);
    }

    #[test]
    fn a_watch_with_nothing_to_learn_changes_nothing() {
        let service = service();
        let rule = add(&service, "calvin", "Kelvin");
        let applied = service.apply_with("hi calvin", None, 0.8);
        let session = Mutex::new(service.new_session(applied.text, applied.applications, None));

        assert!(service
            .conclude_watch(&Notices::default(), &session, None)
            .is_empty());
        assert_eq!(service.store().all().unwrap()[0].hits, 1);

        // The text survived as pasted: the fix is confirmed.
        let events = service.conclude_watch(&Notices::default(), &session, Some("hi Kelvin"));
        assert_eq!(events, vec![LearnEvent::Reinforced { rule_id: rule.id }]);
        assert_eq!(service.store().all().unwrap()[0].hits, 2);
    }

    #[test]
    fn a_swapped_word_is_not_counted_again_when_the_watch_ends() {
        let service = service();
        let (rule_id, session) = ambiguous_session(&service);
        let id = lock(&session).id;
        service
            .run_swap(&Notices::default(), &|_| Ok(()), &|_| Ok(()), id, 0)
            .unwrap(); // swapped back to "heiress": one miss
        let notices = Notices::default();

        // The text box now holds the swapped text.
        let events = service.conclude_watch(&notices, &session, Some("push the fix to heiress"));

        assert!(events.is_empty());
        let rule = service
            .store()
            .all()
            .unwrap()
            .into_iter()
            .find(|r| r.id == rule_id)
            .unwrap();
        assert_eq!((rule.hits, rule.misses), (1, 2));
        assert!(notices.all().is_empty());
    }

    #[test]
    fn swap_generations_let_a_newer_notice_outlive_an_older_timer() {
        let service = service();
        let first = service.swap_requested();
        let second = service.swap_requested();
        assert!(!service.swap_release_is_current(first));
        assert!(service.swap_release_is_current(second));
    }
}

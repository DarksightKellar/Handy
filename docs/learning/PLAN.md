# Learning Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Handy learns from the corrections Kelvin makes to pasted text and applies them before the next paste, with every model.

**Architecture:** A pure Rust core (`src-tauri/src/learning/core/`) holds all the logic: diff, sound-alike check, rules, ambiguity, the watch state machine. Adapters (SQLite store, Jev HTTP, macOS Accessibility watcher) sit behind traits. A `LearningService` in Tauri state joins them and hooks into `actions::process_transcription_output` (apply) and the paste path (watch). The frontend gets a notice panel, a "Learned" settings page and an edit control in History.

**Tech Stack:** Rust (Tauri 2, rusqlite + rusqlite_migration, reqwest, strsim, objc2 + objc2-application-services), React + TypeScript + Tailwind + i18next, tauri-specta bindings.

**Spec:** `docs/learning/DESIGN.md` (decisions D1–D14).

## Global Constraints

- Build env on this Mac: `CMAKE_POLICY_VERSION_MINIMUM=3.5` for every cargo command.
- Run Rust tests from `src-tauri/`: `CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo test --lib learning`.
- Baseline: upstream test `clipboard::tests::external_script_does_not_wait_for_inherited_stdio` fails before any change. Leave it alone. Every other test must pass.
- `learning::core` imports nothing from `tauri`, `rusqlite`, `reqwest` or `objc2`.
- No `unwrap()`/`expect()` in non-test code. Errors are logged and the layer fails open: a learning failure must never block or change a paste.
- Never log user text unredacted: use `crate::utils::redact_text`.
- Phrases are at most 4 words on each side of a rule.
- Sound-alike rule: learn a hunk only if it is a spelling/case change or the old and new sound alike. Otherwise it is a change of mind and is ignored.
- Ambiguity band: a rule with `misses > 0` and `0.25 < p < 0.75`, where `p = hits / (hits + misses)`, is ambiguous. `p >= 0.75` applies the rule. `p <= 0.25` keeps the original.
- Jev threshold default 0.8. Watch default 30 s. Prompt-feed budget 150 words.
- All user-facing frontend strings go through i18next (`src/i18n/locales/en/translation.json`). ESLint enforces it.
- Bundle id stays `com.pais.handy`.
- Commit after each task. Stage explicit paths.

## Review Focus

1. Kelvin sends the Slack message (the box clears) before the watch ends → the last good reading must still be learned from.
2. Kelvin edits text _before_ the pasted region in the same box → the pasted region must still be found, or the reading ignored; never learn from text Handy did not paste.
3. A rule's `to` is a proper noun and the match is at sentence start, or `to` is lower-case at sentence start → casing stays right ("Eres is…", "okay, so…").
4. Two pastes in quick succession → the first watch finishes (learns) before the second starts; never mixes texts.
5. Accessibility not granted, or Secure Input on → no watch, no crash, no notice; paste unchanged.

Each line has a test in the owning task (marked **RF1**–**RF5**).

---

### Task 1: Core text, sound-alike check and word diff

**Files:**

- Create: `src-tauri/src/learning/mod.rs`, `src-tauri/src/learning/core/mod.rs`, `core/text.rs`, `core/sound.rs`, `core/diff.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod learning;`)

**Interfaces — Produces:**

```rust
// core/text.rs
pub struct Token { pub raw: String, pub norm: String, pub start: usize, pub end: usize } // byte offsets into the source
pub fn tokenize(s: &str) -> Vec<Token>;          // split on whitespace; norm = lowercase, leading/trailing punctuation stripped
pub fn normalize_phrase(s: &str) -> String;       // tokens' norm joined by single spaces
pub fn letters_only(s: &str) -> String;           // lowercase alphanumerics only ("Type Safe" -> "typesafe")
// core/sound.rs
pub enum EditClass { Spelling, SoundAlike, ChangeOfMind }
pub fn classify(old: &str, new: &str) -> EditClass;
// core/diff.rs
pub struct Hunk { pub old: String, pub new: String, pub old_start: usize, pub old_end: usize } // BYTE offsets of the hunk in `old` (used to test overlap with Application ranges)
pub fn word_hunks(old: &str, new: &str) -> Vec<Hunk>; // replacement hunks only; pure insertions/deletions are dropped
```

`classify` rules:

- `Spelling` when `letters_only(old) == letters_only(new)` and the raw text differs (case, spaces, hyphens), or when the normalized Levenshtein similarity of `letters_only` ≥ 0.75 _and_ both are a single token ("ok" → "okay" is caught by sound instead; that is fine).
- `SoundAlike` when the phonetic keys are close: key = per-word consonant skeleton (implement a small metaphone-style key: lowercase; map `ph→f, ck→k, c→k, q→k, x→ks, v→f, z→s, j→g`… drop vowels except a leading vowel which becomes `a`; drop `h` after a consonant and leading `h`; collapse repeats; concatenate words without spaces). Similar when `strsim::normalized_levenshtein(key_old, key_new) >= 0.6` or `strsim::normalized_levenshtein(letters_only(old), letters_only(new)) >= 0.6`.
- Else `ChangeOfMind`.
- Either side longer than 4 words → `ChangeOfMind`.

`word_hunks`: LCS over `Token.norm`. Equal-norm pairs whose `raw` letters differ in case only also emit a hunk (old raw → new raw) so capitals are learnt. Adjacent changed tokens merge into one hunk. Drop hunks where one side is empty. Strip punctuation from hunk text edges.

- [ ] **Step 1: Write the failing tests** in `core/sound.rs` and `core/diff.rs` (`#[cfg(test)] mod tests`):

```rust
// sound.rs
#[test] fn calvin_kelvin_sounds_alike() { assert!(matches!(classify("Calvin", "Kelvin"), EditClass::SoundAlike)); }
#[test] fn heiress_eres_sounds_alike() { assert!(matches!(classify("heiress", "Eres"), EditClass::SoundAlike)); }
#[test] fn jeff_jev_sounds_alike() { assert!(matches!(classify("Jeff", "Jev"), EditClass::SoundAlike)); }
#[test] fn get_hub_github_sounds_alike() { assert!(!matches!(classify("get hub", "GitHub"), EditClass::ChangeOfMind)); }
#[test] fn type_safe_is_spelling() { assert!(matches!(classify("type safe", "TypeSafe"), EditClass::Spelling)); }
#[test] fn eres_case_is_spelling() { assert!(matches!(classify("eres", "Eres"), EditClass::Spelling)); }
#[test] fn ok_okay_is_learnable() { assert!(!matches!(classify("ok", "okay"), EditClass::ChangeOfMind)); }
#[test] fn tuesday_friday_is_change_of_mind() { assert!(matches!(classify("Tuesday", "Friday"), EditClass::ChangeOfMind)); }
#[test] fn send_forward_is_change_of_mind() { assert!(matches!(classify("send", "forward"), EditClass::ChangeOfMind)); }
#[test] fn cat_dog_is_change_of_mind() { assert!(matches!(classify("cat", "dog"), EditClass::ChangeOfMind)); }
#[test] fn five_words_is_too_long() { assert!(matches!(classify("a b c d e", "a b c d f"), EditClass::ChangeOfMind)); }

// diff.rs
#[test] fn single_word_replacement() {
    let h = word_hunks("send it to Calvin today.", "send it to Kelvin today.");
    assert_eq!(h.len(), 1); assert_eq!(h[0].old, "Calvin"); assert_eq!(h[0].new, "Kelvin");
}
#[test] fn phrase_replacement_merges() {
    let h = word_hunks("push it to get hub now", "push it to GitHub now");
    assert_eq!(h.len(), 1); assert_eq!(h[0].old, "get hub"); assert_eq!(h[0].new, "GitHub");
}
#[test] fn case_only_change_is_a_hunk() {
    let h = word_hunks("open eres now", "open Eres now");
    assert_eq!(h.len(), 1); assert_eq!(h[0].old, "eres"); assert_eq!(h[0].new, "Eres");
}
#[test] fn punctuation_only_change_is_ignored() { assert!(word_hunks("hello world", "hello, world.").is_empty()); }
#[test] fn pure_insertion_is_ignored() { assert!(word_hunks("hello world", "hello big world").is_empty()); }
#[test] fn two_separate_hunks() {
    let h = word_hunks("Calvin pushed to heiress", "Kelvin pushed to Eres");
    assert_eq!(h.iter().map(|x| (x.old.as_str(), x.new.as_str())).collect::<Vec<_>>(), vec![("Calvin","Kelvin"),("heiress","Eres")]);
}
```

- [ ] **Step 2: Run, verify they fail** — `CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo test --lib learning::core` → compile errors / FAIL.
- [ ] **Step 3: Implement** `text.rs`, `sound.rs`, `diff.rs` to the interfaces above. Tune the key mapping until every test passes; do not loosen the change-of-mind tests.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** `feat(learning): core text, sound-alike check and word diff`.

---

### Task 2: Rules, application, ambiguity and the Learner

**Files:** Create `core/rules.rs`, `core/learner.rs`.

**Interfaces — Consumes:** Task 1. **Produces:**

```rust
// rules.rs
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, specta::Type)] pub enum RuleKind { Word, Phrase, Spelling }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct Rule { pub id: i64, pub from: String, pub to: String, pub kind: RuleKind,
  pub hits: u32, pub misses: u32, pub applied: u32, pub paused: bool,
  pub source_sentence: String, pub created_at: i64, pub updated_at: i64 }
pub enum Verdict { Apply, Keep, Ambiguous { p_to: f64 } }
impl Rule { pub fn verdict(&self) -> Verdict; }   // band from Global Constraints; paused → Keep
// learner.rs
pub trait Decider { fn choose(&self, sentence_with_blank: &str, options: &[String]) -> Option<Vec<f64>>; } // None = no decision
pub struct Application { pub rule_id: i64, pub start: usize, pub end: usize, // byte range in the OUTPUT text
  pub original: String, pub chosen: String, pub alternative: Option<String> } // alternative = Some when ambiguous and not settled by Jev
pub struct Applied { pub text: String, pub applications: Vec<Application> }
pub enum LearnEvent { Learned(Rule), Reinforced { rule_id: i64 }, Rejected { rule_id: i64 } }
pub struct NewRule { pub from: String, pub to: String, pub kind: RuleKind, pub source_sentence: String }
pub enum Observation { New(NewRule), Hit(i64), Miss(i64) }
pub fn apply(text: &str, rules: &[Rule], decider: Option<&dyn Decider>, jev_threshold: f64) -> Applied;
pub fn observe(pasted: &str, edited: &str, applications: &[Application], rules: &[Rule]) -> Vec<Observation>;
```

`apply`: match rules (not paused) on token-norm sequences, longest `from` first, left to right, no overlaps, whole tokens only. Keep the first token's leading punctuation and the last token's trailing punctuation. Casing: if `to` has any upper-case letter, use `to` verbatim; else if the match is at sentence start (first token of text, or previous token ends with `.`, `!`, `?`), capitalise its first letter; else use `to`. Verdict `Apply` → replace. `Keep` → leave. `Ambiguous` → if a decider is given, call it with the sentence (the text with the match replaced by `___`) and options `[to, original]`; if `probs[i] >= jev_threshold` take option i with `alternative: None`; otherwise (or no decider / decider returns None) take the higher-p option (`to` when `p_to >= 0.5`) with `alternative: Some(other)`.

`observe`: for each `word_hunks(pasted, edited)` hunk:

- If the hunk overlaps an application's range and `hunk.new` normalises to that application's `original` → `Miss(rule_id)`.
- Else if it overlaps an application and `hunk.new` normalises to that application's `chosen` → ignore.
- Else if an existing rule has the same normalised `from` and `to` → `Hit(rule_id)`.
- Else classify: `ChangeOfMind` → ignore; otherwise `New(NewRule{ from: hunk.old, to: hunk.new, kind: Spelling | Word (1 token) | Phrase, source_sentence: edited })`.
- Each application with no overlapping hunk → `Hit(rule_id)` (the fix was kept).

- [ ] **Step 1: Write the failing tests** (`learner.rs` tests; build `Rule` with a helper `fn rule(id, from, to, hits, misses) -> Rule`):

```rust
#[test] fn learned_rule_fixes_next_transcript() {
    let obs = observe("send it to Calvin.", "send it to Kelvin.", &[], &[]);
    let Observation::New(n) = &obs[0] else { panic!() };
    assert_eq!((n.from.as_str(), n.to.as_str()), ("Calvin", "Kelvin"));
    let r = rule(1, &n.from, &n.to, 1, 0);
    assert_eq!(apply("tell Calvin the news", &[r], None, 0.8).text, "tell Kelvin the news");
}
#[test] fn change_of_mind_is_not_learned() { assert!(observe("meet on Tuesday", "meet on Friday", &[], &[]).is_empty()); }
#[test] fn phrase_rule_applies() { assert_eq!(apply("push to get hub please", &[rule(1,"get hub","GitHub",1,0)], None, 0.8).text, "push to GitHub please"); }
#[test] fn punctuation_is_kept() { assert_eq!(apply("ask Calvin, then go.", &[rule(1,"Calvin","Kelvin",1,0)], None, 0.8).text, "ask Kelvin, then go."); }
#[test] fn rf3_proper_noun_at_sentence_start() { assert_eq!(apply("heiress is ready.", &[rule(1,"heiress","Eres",1,0)], None, 0.8).text, "Eres is ready."); }
#[test] fn rf3_lowercase_to_at_sentence_start_is_capitalised() { assert_eq!(apply("ok so. ok then", &[rule(1,"ok","okay",1,0)], None, 0.8).text, "Okay so. Okay then"); }
#[test] fn no_partial_word_match() { assert_eq!(apply("Calvinist views", &[rule(1,"Calvin","Kelvin",1,0)], None, 0.8).text, "Calvinist views"); }
#[test] fn paused_rule_does_not_apply() { let mut r = rule(1,"Calvin","Kelvin",1,0); r.paused = true; assert_eq!(apply("hi Calvin", &[r], None, 0.8).text, "hi Calvin"); }
#[test] fn reverted_fix_is_a_miss() {
    let rules = [rule(1,"heiress","Eres",1,0)];
    let a = apply("the heiress arrived", &rules, None, 0.8);
    assert_eq!(a.text, "the Eres arrived");
    let obs = observe(&a.text, "the heiress arrived", &a.applications, &rules);
    assert!(matches!(obs.as_slice(), [Observation::Miss(1)]));
}
#[test] fn kept_fix_is_a_hit() {
    let rules = [rule(1,"Calvin","Kelvin",1,0)];
    let a = apply("hi Calvin", &rules, None, 0.8);
    assert!(matches!(observe(&a.text, &a.text, &a.applications, &rules).as_slice(), [Observation::Hit(1)]));
}
#[test] fn ambiguous_without_decider_offers_alternative() {
    let a = apply("push the fix to heiress", &[rule(1,"heiress","Eres",1,1)], None, 0.8);
    assert_eq!(a.applications[0].alternative.as_deref(), Some("heiress"));
}
struct Fixed(Vec<f64>);
impl Decider for Fixed { fn choose(&self, _: &str, _: &[String]) -> Option<Vec<f64>> { Some(self.0.clone()) } }
#[test] fn sure_decider_settles_ambiguity() {
    let a = apply("push the fix to heiress", &[rule(1,"heiress","Eres",1,1)], Some(&Fixed(vec![0.9,0.1])), 0.8);
    assert_eq!(a.text, "push the fix to Eres"); assert!(a.applications[0].alternative.is_none());
}
#[test] fn unsure_decider_offers_alternative() {
    let a = apply("the heiress inherited", &[rule(1,"heiress","Eres",2,1)], Some(&Fixed(vec![0.6,0.4])), 0.8);
    assert!(a.applications[0].alternative.is_some());
}
#[test] fn mostly_rejected_rule_keeps_original() { assert_eq!(apply("the heiress", &[rule(1,"heiress","Eres",1,4)], None, 0.8).text, "the heiress"); }
```

- [ ] **Step 2: Run, verify fail.** **Step 3: Implement.** **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** `feat(learning): rules, ambiguity and the learner`.

---

### Task 3: The watch state machine (pure)

**Files:** Create `core/region.rs`.

**Produces:**

```rust
pub struct Watch { /* baseline, pasted_start, pasted_end, last_region: Option<String>, misses_in_row: u32 */ }
pub enum WatchStep { Continue, Finish(Option<String>) } // Finish(Some(edited_region)) or Finish(None) = nothing to learn
impl Watch {
    /// `value_after_paste` is the field value read just after the paste. Returns None when it does not contain `pasted`.
    pub fn start(value_after_paste: &str, pasted: &str) -> Option<Watch>;
    /// Feed each later reading. `None` reading = the element is gone or focus moved.
    pub fn feed(&mut self, reading: Option<&str>) -> WatchStep;
    pub fn finish(self) -> Option<String>; // timeout
}
```

Locate the pasted text in the baseline (last occurrence). Anchors: up to 24 chars of baseline before the region and up to 24 chars after it. A reading is "good" when the before-anchor occurs and the after-anchor occurs after it; the region is the text between them (trimmed of the anchor edges). Empty anchors match the string start/end. A good reading sets `last_region`. A reading that is not good increments `misses_in_row`; at 2 → `Finish(last_region)` (this is the send-and-clear case). `None` reading → `Finish(last_region)`. `finish()` returns `last_region`. A `last_region` equal to `pasted` returns `None` (nothing changed).

- [ ] **Step 1: Failing tests:**

```rust
#[test] fn rf1_send_clears_box_but_edit_is_kept() {
    let mut w = Watch::start("send it to Calvin", "send it to Calvin").unwrap();
    assert!(matches!(w.feed(Some("send it to Kelvin")), WatchStep::Continue));
    assert!(matches!(w.feed(Some("")), WatchStep::Continue));
    assert!(matches!(w.feed(Some("")), WatchStep::Finish(Some(ref s)) if s == "send it to Kelvin"));
}
#[test] fn rf2_edit_before_region_is_tolerated() {
    let mut w = Watch::start("Hi team. send it to Calvin", "send it to Calvin").unwrap();
    w.feed(Some("Hi all of the team. send it to Kelvin"));
    assert_eq!(w.finish().as_deref(), Some("send it to Kelvin"));
}
#[test] fn rf2_region_not_found_learns_nothing() {
    let mut w = Watch::start("abc send it to Calvin xyz", "send it to Calvin").unwrap();
    w.feed(Some("completely different text"));
    assert_eq!(w.finish(), None);
}
#[test] fn paste_missing_from_field_does_not_start() { assert!(Watch::start("unrelated", "send it").is_none()); }
#[test] fn unchanged_text_learns_nothing() { let w = Watch::start("hello there", "hello there").unwrap(); assert_eq!(w.finish(), None); }
#[test] fn focus_lost_finishes_with_last_good() {
    let mut w = Watch::start("x hello Calvin y", "hello Calvin").unwrap();
    w.feed(Some("x hello Kelvin y"));
    assert!(matches!(w.feed(None), WatchStep::Finish(Some(ref s)) if s == "hello Kelvin"));
}
```

- [ ] **Steps 2–4** as usual. **Step 5: Commit** `feat(learning): watch state machine`.

---

### Task 4: SQLite rule store

**Files:** Create `src-tauri/src/learning/store.rs`.

**Produces:**

```rust
pub struct RuleStore { conn: std::sync::Mutex<rusqlite::Connection> }
impl RuleStore {
    pub fn open(path: &Path) -> anyhow::Result<Self>;   // runs migrations
    pub fn open_in_memory() -> anyhow::Result<Self>;
    pub fn all(&self) -> anyhow::Result<Vec<Rule>>;     // newest first
    pub fn insert(&self, r: &NewRule, now: i64) -> anyhow::Result<Rule>; // hits = 1; if same normalised from+to exists, increments hits and returns it
    pub fn hit(&self, id: i64, now: i64) -> anyhow::Result<()>;
    pub fn miss(&self, id: i64, now: i64) -> anyhow::Result<()>;
    pub fn applied(&self, id: i64) -> anyhow::Result<()>;
    pub fn update(&self, id: i64, from: &str, to: &str, paused: bool, now: i64) -> anyhow::Result<Rule>;
    pub fn delete(&self, id: i64) -> anyhow::Result<()>;
}
```

Schema (`rusqlite_migration`, same pattern as `managers/history.rs`):
`CREATE TABLE rules (id INTEGER PRIMARY KEY AUTOINCREMENT, from_text TEXT NOT NULL, to_text TEXT NOT NULL, from_norm TEXT NOT NULL, to_norm TEXT NOT NULL, kind TEXT NOT NULL, hits INTEGER NOT NULL DEFAULT 1, misses INTEGER NOT NULL DEFAULT 0, applied INTEGER NOT NULL DEFAULT 0, paused BOOLEAN NOT NULL DEFAULT 0, source_sentence TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, UNIQUE(from_norm, to_norm));`

- [ ] Tests (in-memory): insert then all; insert duplicate increments hits; hit/miss/applied counters; update changes text and norms and paused; delete removes. Then implement, pass, commit `feat(learning): sqlite rule store`.

---

### Task 5: Service, settings, pipeline hook, history column, prompt feed

**Files:**

- Create: `src-tauri/src/learning/service.rs`, `src-tauri/src/learning/commands.rs`
- Modify: `src-tauri/src/settings.rs` (fields + defaults + default-settings JSON test), `src-tauri/src/actions.rs` (`process_transcription_output`, paste path), `src-tauri/src/managers/history.rs` (migration `ALTER TABLE transcription_history ADD COLUMN pasted_text TEXT;` and `ADD COLUMN edited_text TEXT;`, fields on `HistoryEntry`, `save_entry` gets `pasted_text: Option<String>`, new `set_edited_text(id, text)`), `src-tauri/src/managers/transcription.rs` (prompt feed at the `initial_prompt` site, ~line 1317), `src-tauri/src/lib.rs` (manage the service, register commands), `src/bindings.ts` (regenerated by running the app in debug or by the specta export path the repo uses).

**Settings** (all `#[serde(default = ...)]`): `learning_enabled: bool = true`, `learning_watch_seconds: u32 = 30`, `jev_api_key: String = ""`, `jev_confidence_threshold: f64 = 0.8`. Binding `learning_swap` (name "Swap learned word", default macOS `ctrl+option+s`, others `ctrl+alt+s`), registered only while a swap notice is visible — follow the `register_cancel_shortcut` / `unregister_cancel_shortcut` pattern in `shortcut/mod.rs` and add it to `ACTION_MAP` in `actions.rs`.

**Service:**

```rust
pub struct LearningService { store: RuleStore, last: Mutex<Option<PasteSession>>, decider: Option<Box<dyn Decider + Send + Sync>> }
pub struct PasteSession { pub id: u64, pub pasted: String, pub applications: Vec<Application>, pub history_id: Option<i64> }
impl LearningService {
    pub fn apply(&self, app: &AppHandle, text: &str) -> Applied;     // fail-open: on error returns text unchanged
    pub fn after_paste(&self, app: &AppHandle, session: PasteSession); // finishes any running watch first (RF4), then starts the watcher (Task 6)
    pub fn observe_and_record(&self, app: &AppHandle, pasted: &str, edited: &str, applications: &[Application]) -> Vec<LearnEvent>; // writes to store, emits notices
    pub fn prompt_words(&self, budget_words: usize) -> Vec<String>; // distinct `to` values, ordered by hits+applied desc, within budget
}
```

In `process_transcription_output`: when `settings.learning_enabled`, run `apply` on `final_text` _before_ LLM post-processing; keep the `Applied` for the session. Return it in `ProcessedTranscription` (add `learning: Option<Applied>`). Save `pasted_text` = the final pasted text in history. After `utils::paste` returns `Ok`, call `after_paste`.

**Commands** (`commands.rs`, registered in `lib.rs` `collect_commands!`): `learning_list_rules() -> Vec<Rule>`, `learning_update_rule(id, from, to, paused) -> Rule`, `learning_delete_rule(id)`, `learning_undo(rule_id)` (deletes a just-learned rule), `learning_swap(session_id: u64, application_index: usize) -> Result<(), String>` (Task 6 writer), `learning_from_history_edit(entry_id: i64, edited: String) -> Vec<Rule>` (compares against `pasted_text`, falling back to `post_processed_text` then `transcription_text`; stores `edited_text`; emits a history `Updated` event).

- [ ] **Step 1: Failing tests:** settings defaults include the four fields; `prompt_words` respects the budget and order (service built with `RuleStore::open_in_memory()`); integration test `history_edit_learns_and_next_transcript_is_fixed`: `observe_and_record`-equivalent pure path on a store → `apply` on the next text gives the fixed word.
- [ ] Implement, pass all `cargo test --lib`, commit `feat(learning): service, settings and pipeline hook`.

---

### Task 6: macOS Accessibility watcher and swap writer

**Files:** Create `src-tauri/src/learning/watcher_macos.rs` (`#[cfg(target_os = "macos")]`), a no-op `watcher_other.rs` for other platforms. Modify `Cargo.toml` (macOS target deps: `objc2-application-services` and `objc2-core-foundation` with the AXUIElement features — check the crate docs for exact feature names).

**Produces:**

```rust
pub trait FieldReader: Send { fn read(&self) -> Option<String>; fn is_secure(&self) -> bool; }
pub fn focused_field() -> Option<Box<dyn FieldReader>>; // None when AX not trusted (AXIsProcessTrusted), no focused element, role AXSecureTextField, or no AXValue
pub fn replace_in_focused(original_region: &str, find: &str, replace_with: &str) -> Result<(), String>; // locate `find` inside the region, set AXSelectedTextRange, set AXSelectedText
pub fn spawn_watch(reader: Box<dyn FieldReader>, pasted: String, watch_for: Duration, secure_now: impl Fn() -> bool + Send + 'static, done: impl FnOnce(Option<String>) + Send + 'static) -> WatchHandle;
pub struct WatchHandle { /* stop flag + join handle */ } impl WatchHandle { pub fn finish_now(self); }
```

Loop: wait 250 ms after paste, read baseline → `Watch::start`. Then every 500 ms: if `secure_now()` (use `crate::secure_input` state) or `reader.is_secure()` → stop with `None` (RF5). Feed readings until `Finish` or timeout. Call `done` with the result. The focused element is captured once at start; if the focused element changes, feed `None`.

- [ ] **Step 1: Failing test** (platform-independent, with a fake `FieldReader` whose readings come from a `Vec`): `spawn_watch` with readings `["send it to Calvin", "send it to Kelvin", "", ""]` calls `done(Some("send it to Kelvin"))`; with `secure_now = || true` calls `done(None)` (RF5). Use a short poll interval injected for tests (`spawn_watch_with_interval`).
- [ ] Implement; wire into `LearningService::after_paste`; `AXIsProcessTrusted() == false` → no watch, debug log only (RF5).
- [ ] Commit `feat(learning): macOS accessibility watcher`.

---

### Task 7: Jev decider adapter

**Files:** Create `src-tauri/src/learning/jev.rs`.

API (verified 2026-10-09 from docs.typesafe.ai): `POST https://api.typesafe.ai/v1/systemone`, header `Authorization: Bearer <key>`, body:

```json
{
  "model": "jev-latest",
  "state": "push the fix to ___",
  "questions": {
    "word": {
      "type": "choice",
      "instructions": "The sentence was dictated by speech recognition. Which option did the speaker say in the blank?",
      "criteria": { "option_0": "Eres", "option_1": "heiress" }
    }
  }
}
```

Response: `answers.word.probabilities.option_0` etc. Errors: 401, 422, 429, 529. Timeout 1500 ms; any error → `None` (fail open to the alternative notice).

**Produces:** `pub struct JevDecider { key: String, client: reqwest::blocking::Client }` implementing `Decider`; `pub fn build_request(sentence: &str, options: &[String]) -> serde_json::Value`; `pub fn parse_probabilities(body: &serde_json::Value, n: usize) -> Option<Vec<f64>>`. The service builds a `JevDecider` only when `jev_api_key` is non-empty.

- [ ] Tests for `build_request` (keys `option_0..`, the model id, the blank) and `parse_probabilities` (the example response from the docs; a missing field → `None`). Implement, commit `feat(learning): Jev decider`.

---

### Task 8: Notice panel, Undo and Swap

**Files:** Create `src/notice/index.html`, `src/notice/main.tsx`, `src/notice/Notice.tsx`, `src/notice/Notice.css`; modify `vite.config.ts` (add `notice` input), `src-tauri/src/overlay.rs` or a new `src-tauri/src/learning/notice.rs` (create a second non-activating NSPanel "learning_notice", like `create_recording_overlay`, but it must accept mouse clicks), `src-tauri/tauri.conf.json`/capabilities if a new window label needs permissions.

Behaviour:

- Learned event → panel shows "Learned: Calvin → Kelvin" + **Undo** for 6 s. Undo calls `learning_undo(rule_id)`.
- Ambiguous application → panel shows "heiress → Eres?" style: the pasted choice and the other option + **Swap** and the shortcut hint for 6 s. Swap calls `learning_swap(session_id, index)`; while visible, the `learning_swap` binding is registered.
- Swap result: success → the rule gets a hit (alternative = `to`) or a miss (alternative = `original`); failure → copy the alternative to the clipboard and show "Could not swap. The other option is on the clipboard."
- Panel sits at the bottom centre above the recording overlay position, never takes focus.

- [ ] Manual check script in the task report (Rust side has a unit test that the notice payload serialises). Commit `feat(learning): notice panel with undo and swap`.

---

### Task 9: Learned settings page and history edit

**Files:** Create `src/components/settings/learned/LearnedSettings.tsx`; modify `src/components/settings/index.ts`, `src/components/Sidebar.tsx` (section `learned`, icon `GraduationCap` from lucide-react, always enabled), `src/components/settings/history/*` (an Edit button per entry → textarea → Save calls `learning_from_history_edit`; show `edited_text` when present), `src/i18n/locales/en/translation.json`.

Page content: toggle `learning_enabled`; number input watch seconds (5–120); Jev API key (password input) and threshold slider (0.5–0.99); the rule table: from → to, kind, hits/misses/applied, source sentence (tooltip), pause switch, edit (inline), delete. Empty state text. Uses existing settings UI components (`SettingContainer`, `ToggleSwitch`, etc. in `src/components/ui`).

- [ ] `bun run lint` and `bun run build` pass. Commit `feat(learning): learned page and history edit`.

---

### Task 10: Fork updater, install script, README

**Files:** Modify `src-tauri/tauri.conf.json` (`plugins.updater.endpoints` → `https://github.com/DarksightKellar/Handy/releases/latest/download/latest.json`, `pubkey` → a new key from `bun run tauri signer generate -w ~/.tauri/handy-fork.key`; never commit the private key). Create `scripts/install-local.sh` (build with `TAURI_SIGNING_PRIVATE_KEY` from the key file, quit Handy, replace `/Applications/Handy.app`, print the Accessibility reset steps from `BUILD.md`), add a "Learning layer" section to `README.md`.

- [ ] Full checks: `cargo test --lib`, `cargo clippy --all-targets`, `bun run lint`, `bun run build`, `bun run format:check`, `CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri build`.
- [ ] Commit, push `feat/learning-layer` to `origin`, open a PR against the fork's `main`.

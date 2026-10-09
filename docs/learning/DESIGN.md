# Handy learns from your corrections — design

Status: locked by Kelvin on 2026-10-09. This fork adds a learning layer to Handy,
like the personal dictionary in Wispr Flow.

## The problem

The speech model sometimes hears a word wrong ("Calvin" for "Kelvin", "heiress" for
"Eres"). Kelvin then fixes the text by hand where Handy pasted it. Upstream Handy does
not see the fix, so the same mistake comes back every time.

## Decisions (locked)

| id | Decision |
|----|----------|
| D1 | Handy sees each fix and records it. Kelvin fills in no form. |
| D2 | This is a fork of cjpais/Handy, not a companion app, because the fix must happen before the paste. |
| D3 | After a paste, Handy watches the focused text box (macOS Accessibility) for a set time (default 30 s). It learns a fix only when the old and new text sound alike, or differ only in spelling or capitals. A change of mind is not learned. |
| D4 | A notice says "Learned: X → Y" with an Undo button. |
| D5 | For apps that hide their text from Accessibility, Kelvin fixes the text in Handy's history window, and Handy learns from that edit. |
| D6 | Fixed rules apply first. When a rule has evidence both ways (Kelvin sometimes means the original word), the phrase is ambiguous. |
| D7 | Ambiguous + a Jev API key set: Handy asks Jev (TypeSafe AI) to choose. Jev's probability ≥ the threshold (default 0.8): use its choice. Otherwise, or with no key: paste the best guess and offer the other option. |
| D8 | Only the one sentence goes to Jev, as text. Audio never leaves the Mac. Nothing else calls a network service. |
| D9 | Handy pastes its best guess at once. A small notice offers the other option for a few seconds. A click or a shortcut swaps the text in the text box, and the choice teaches Handy. |
| D10 | Version 1 learns single words, phrases (up to 4 words), and spelling and capitals. It does not seed from Kelvin's files. |
| D11 | A learned fix applies from the first time. |
| D12 | A "Learned" settings page lists each fix with its counts and source sentence. Kelvin can edit, pause, or delete each one. |
| D13 | The learning layer is model-agnostic. For models that take a prompt (Whisper family), the learned words also go into the prompt, most-used first, within a 150-word budget. |
| D14 | Public fork at github.com/DarksightKellar/Handy. The build replaces /Applications/Handy.app and keeps the bundle id `com.pais.handy`, so settings and history stay. The updater points at the fork's releases. |

## Where the layer sits

```
audio → model → TranscriptionManager (custom words, fillers, normalise)
      → actions::process_transcription_output
            → LearningService::apply          ← NEW: learned rules, ambiguity, Jev
            → optional LLM post-processing (upstream)
      → paste
      → LearningService::begin_watch          ← NEW: Accessibility watch of the text box
            → Learner::observe → new rules / evidence → notice
```

Rules apply to the cleaned text, after filler removal. That is the same text that Kelvin
sees and edits, so a rule learned from an edit always matches the next transcript.

## Modules

`src-tauri/src/learning/`

| File | Kind | Job |
|------|------|-----|
| `core/text.rs` | pure | Tokens, normalisation, case handling |
| `core/sound.rs` | pure | The sound-alike and spelling check |
| `core/diff.rs` | pure | Word diff between pasted and edited text → edit hunks |
| `core/rules.rs` | pure | `Rule`, matching, application, ambiguity score |
| `core/learner.rs` | pure | `Learner::apply` and `Learner::observe` |
| `core/region.rs` | pure | Find the pasted region inside a changing text-box value; the watch state machine |
| `store.rs` | adapter | SQLite rule store (`learning.db` in the app data dir) |
| `jev.rs` | adapter | Jev HTTP client behind the `Decider` trait |
| `watcher_macos.rs` | adapter | Accessibility: read the focused element, poll it, write a swap |
| `service.rs` | glue | Tauri state; joins the core, the store, the decider, the watcher and the notices |
| `commands.rs` | glue | Tauri commands for the settings page, history edits, Undo and Swap |

The core imports no Tauri, no SQLite and no network code. Every adapter is behind a trait,
so each core test runs with fakes.

## Privacy

- The watcher reads only the element that received the paste, and only during the watch.
- It stops at once when macOS Secure Input is on or the element is a secure text field.
- Logs redact text through `utils::redact_text`.
- Jev receives one sentence. No key, no call.

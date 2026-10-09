//! Applying rules to a transcript and learning from edits to a paste.

use std::time::{Duration, Instant};

use super::diff::word_hunks;
use super::rules::{Rule, RuleKind, Verdict};
use super::sound::{classify, EditClass};
use super::text::{normalize_phrase, tokenize, Token};

/// Chooses between options for a blank in a sentence (the Jev adapter).
pub trait Decider {
    /// `None` means no decision. Otherwise one probability per option.
    fn choose(&self, sentence_with_blank: &str, options: &[String]) -> Option<Vec<f64>>;
}

/// The most time all decider calls of one paste may use together. Dictation
/// must not wait on the network: past this, the best guess is pasted and the
/// other option is offered.
pub const DECIDER_BUDGET: Duration = Duration::from_millis(1500);

/// One rule applied to the output text.
#[derive(Clone, Debug, PartialEq)]
pub struct Application {
    pub rule_id: i64,
    /// Byte range of the replaced text in the OUTPUT text.
    pub start: usize,
    pub end: usize,
    pub original: String,
    pub chosen: String,
    /// The other option, when the rule was ambiguous and Jev did not settle it.
    pub alternative: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    pub text: String,
    pub applications: Vec<Application>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LearnEvent {
    Learned(Rule),
    Reinforced { rule_id: i64 },
    Rejected { rule_id: i64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewRule {
    pub from: String,
    pub to: String,
    pub kind: RuleKind,
    pub source_sentence: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Observation {
    New(NewRule),
    Hit(i64),
    Miss(i64),
}

/// A rule ready to match: its `from` as normalised tokens.
struct Matcher<'a> {
    rule: &'a Rule,
    words: Vec<String>,
}

/// Applies `rules` to `text`.
///
/// Rules that are not paused match on whole words, longest `from` first, left
/// to right, without overlap. Punctuation around a match is kept. An ambiguous
/// rule goes to `decider` when there is one; if the decider is not sure, the
/// likelier option is used and the other one is offered as `alternative`.
pub fn apply(
    text: &str,
    rules: &[Rule],
    decider: Option<&dyn Decider>,
    jev_threshold: f64,
) -> Applied {
    apply_within(text, rules, decider, jev_threshold, DECIDER_BUDGET)
}

/// [`apply`] with an explicit time budget for all decider calls together.
pub fn apply_within(
    text: &str,
    rules: &[Rule],
    decider: Option<&dyn Decider>,
    jev_threshold: f64,
    budget: Duration,
) -> Applied {
    let mut gate = DeciderGate::new(decider, budget);
    let mut matchers: Vec<Matcher> = rules
        .iter()
        .filter(|r| !r.paused)
        .filter_map(|rule| {
            let words: Vec<String> = tokenize(&rule.from).into_iter().map(|t| t.norm).collect();
            (!words.is_empty()).then_some(Matcher { rule, words })
        })
        .collect();
    // Stable sort: rules of equal length keep the caller's order.
    matchers.sort_by_key(|m| std::cmp::Reverse(m.words.len()));

    let tokens = tokenize(text);
    let mut out = String::with_capacity(text.len());
    let mut applications = Vec::new();
    let mut copied = 0; // bytes of `text` already copied into `out`
    let mut i = 0;
    while i < tokens.len() {
        let Some(m) = matchers.iter().find(|m| matches_at(&tokens, i, &m.words)) else {
            i += 1;
            continue;
        };
        let last = i + m.words.len() - 1;
        let at_sentence_start = i == 0 || ends_sentence(&tokens[i - 1].raw);
        let first = &tokens[i];
        let end_token = &tokens[last];
        // The match proper, without the first token's leading and the last
        // token's trailing punctuation.
        let start = first.start + (first.raw.len() - first.raw.trim_start_matches(is_edge).len());
        let end =
            end_token.end - (end_token.raw.len() - end_token.raw.trim_end_matches(is_edge).len());
        let original = &text[start..end];
        let to = cased(&m.rule.to, at_sentence_start);

        let (chosen, alternative) = match m.rule.verdict() {
            Verdict::Keep => (original.to_string(), None),
            Verdict::Apply => (to, None),
            Verdict::Ambiguous { p_to } => {
                let blank = sentence_with_blank(text, start, end);
                decide(p_to, &to, original, &blank, &mut gate, jev_threshold)
            }
        };

        let is_keep = matches!(m.rule.verdict(), Verdict::Keep);
        let is_no_change = chosen == original && alternative.is_none();
        if !is_keep && !is_no_change {
            out.push_str(&text[copied..start]);
            let out_start = out.len();
            out.push_str(&chosen);
            applications.push(Application {
                rule_id: m.rule.id,
                start: out_start,
                end: out.len(),
                original: original.to_string(),
                chosen,
                alternative,
            });
            copied = end;
        }
        i = last + 1;
    }
    out.push_str(&text[copied..]);
    Applied {
        text: out,
        applications,
    }
}

fn is_edge(c: char) -> bool {
    !c.is_alphanumeric()
}

fn matches_at(tokens: &[Token], at: usize, words: &[String]) -> bool {
    at + words.len() <= tokens.len()
        && tokens[at..at + words.len()]
            .iter()
            .zip(words)
            .all(|(t, w)| &t.norm == w)
}

fn ends_sentence(raw: &str) -> bool {
    raw.ends_with(['.', '!', '?'])
}

/// A `to` with a capital is used as written (a proper noun). A lower-case `to`
/// is capitalised at the start of a sentence.
fn cased(to: &str, at_sentence_start: bool) -> String {
    if !at_sentence_start || to.chars().any(char::is_uppercase) {
        return to.to_string();
    }
    let mut chars = to.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The decider, with the time one paste may spend on it. A decider that gives
/// no answer (it failed or timed out) is not asked again in the same paste.
struct DeciderGate<'a> {
    decider: Option<&'a dyn Decider>,
    deadline: Instant,
}

impl<'a> DeciderGate<'a> {
    fn new(decider: Option<&'a dyn Decider>, budget: Duration) -> Self {
        Self {
            decider,
            deadline: Instant::now() + budget,
        }
    }

    fn choose(&mut self, blank: &str, options: &[String]) -> Option<Vec<f64>> {
        let decider = self.decider?;
        if Instant::now() >= self.deadline {
            self.decider = None;
            return None;
        }
        let answer = decider.choose(blank, options);
        if answer.is_none() {
            self.decider = None;
        }
        answer
    }
}

/// The sentence that holds `text[start..end]`, with the match replaced by
/// `___`. The sentence runs from the nearest `.`, `!`, `?` or line break before
/// the match to the nearest one after it (that mark ends the sentence and
/// stays). Only this much of the text goes to the decider.
fn sentence_with_blank(text: &str, start: usize, end: usize) -> String {
    // The stop marks are ASCII, so one byte each.
    let is_stop = |c: char| matches!(c, '.' | '!' | '?' | '\n' | '\r');
    let from = text[..start].rfind(is_stop).map_or(0, |i| i + 1);
    let to = text[end..]
        .find(is_stop)
        .map_or(text.len(), |i| end + i + 1);
    // A line break ends the sentence but is not part of it.
    let after = text[end..to].trim_end_matches(['\n', '\r']);
    format!("{}___{}", &text[from..start], after)
        .trim()
        .to_string()
}

/// Picks between `to` and `original` for an ambiguous rule. Returns the chosen
/// text and, when nothing settled it, the other option.
fn decide(
    p_to: f64,
    to: &str,
    original: &str,
    blank: &str,
    gate: &mut DeciderGate,
    jev_threshold: f64,
) -> (String, Option<String>) {
    let options = [to.to_string(), original.to_string()];
    if let Some(probs) = gate.choose(blank, &options) {
        for (option, p) in options.iter().zip(&probs) {
            if *p >= jev_threshold {
                return (option.clone(), None);
            }
        }
    }
    if p_to >= 0.5 {
        (to.to_string(), Some(original.to_string()))
    } else {
        (original.to_string(), Some(to.to_string()))
    }
}

/// Reads what the writer did to a paste and returns what to learn from it.
///
/// `applications` are the fixes Handy made in `pasted`. A hunk that puts an
/// application's original text back is a `Miss`; a fix left alone is a `Hit`.
pub fn observe(
    pasted: &str,
    edited: &str,
    applications: &[Application],
    rules: &[Rule],
) -> Vec<Observation> {
    let mut out = Vec::new();
    let mut touched = vec![false; applications.len()];

    for hunk in word_hunks(pasted, edited) {
        let new_norm = normalize_phrase(&hunk.new);
        let overlapping = applications
            .iter()
            .enumerate()
            .find(|(_, a)| a.start < hunk.old_end && hunk.old_start < a.end);
        if let Some((idx, app)) = overlapping {
            touched[idx] = true;
            if new_norm == normalize_phrase(&app.original) {
                out.push(Observation::Miss(app.rule_id));
                continue;
            }
            if new_norm == normalize_phrase(&app.chosen) {
                continue;
            }
        }

        let old_norm = normalize_phrase(&hunk.old);
        if let Some(rule) = rules
            .iter()
            .find(|r| normalize_phrase(&r.from) == old_norm && normalize_phrase(&r.to) == new_norm)
        {
            out.push(Observation::Hit(rule.id));
            continue;
        }

        let kind = match classify(&hunk.old, &hunk.new) {
            EditClass::ChangeOfMind => continue,
            EditClass::Spelling => RuleKind::Spelling,
            EditClass::SoundAlike if is_single_word(&hunk.old) && is_single_word(&hunk.new) => {
                RuleKind::Word
            }
            EditClass::SoundAlike => RuleKind::Phrase,
        };
        out.push(Observation::New(NewRule {
            from: hunk.old,
            to: hunk.new,
            kind,
            source_sentence: edited.to_string(),
        }));
    }

    for (app, was_touched) in applications.iter().zip(&touched) {
        if *was_touched {
            continue;
        }
        // A fix left alone confirms the rule. When an ambiguous rule pasted
        // the original text instead, leaving it alone counts against the rule.
        if app.alternative.is_some()
            && normalize_phrase(&app.chosen) == normalize_phrase(&app.original)
        {
            out.push(Observation::Miss(app.rule_id));
        } else {
            out.push(Observation::Hit(app.rule_id));
        }
    }
    out
}

fn is_single_word(s: &str) -> bool {
    tokenize(s).len() == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: i64, from: &str, to: &str, hits: u32, misses: u32) -> Rule {
        Rule {
            id,
            from: from.into(),
            to: to.into(),
            kind: RuleKind::Word,
            hits,
            misses,
            applied: 0,
            paused: false,
            source_sentence: String::new(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn learned_rule_fixes_next_transcript() {
        let obs = observe("send it to Calvin.", "send it to Kelvin.", &[], &[]);
        let Observation::New(n) = &obs[0] else {
            panic!()
        };
        assert_eq!((n.from.as_str(), n.to.as_str()), ("Calvin", "Kelvin"));
        let r = rule(1, &n.from, &n.to, 1, 0);
        assert_eq!(
            apply("tell Calvin the news", &[r], None, 0.8).text,
            "tell Kelvin the news"
        );
    }
    #[test]
    fn change_of_mind_is_not_learned() {
        assert!(observe("meet on Tuesday", "meet on Friday", &[], &[]).is_empty());
    }
    #[test]
    fn phrase_rule_applies() {
        assert_eq!(
            apply(
                "push to get hub please",
                &[rule(1, "get hub", "GitHub", 1, 0)],
                None,
                0.8
            )
            .text,
            "push to GitHub please"
        );
    }
    #[test]
    fn punctuation_is_kept() {
        assert_eq!(
            apply(
                "ask Calvin, then go.",
                &[rule(1, "Calvin", "Kelvin", 1, 0)],
                None,
                0.8
            )
            .text,
            "ask Kelvin, then go."
        );
    }
    #[test]
    fn rf3_proper_noun_at_sentence_start() {
        assert_eq!(
            apply(
                "heiress is ready.",
                &[rule(1, "heiress", "Eres", 1, 0)],
                None,
                0.8
            )
            .text,
            "Eres is ready."
        );
    }
    #[test]
    fn rf3_lowercase_to_at_sentence_start_is_capitalised() {
        assert_eq!(
            apply("ok so. ok then", &[rule(1, "ok", "okay", 1, 0)], None, 0.8).text,
            "Okay so. Okay then"
        );
    }
    #[test]
    fn no_partial_word_match() {
        assert_eq!(
            apply(
                "Calvinist views",
                &[rule(1, "Calvin", "Kelvin", 1, 0)],
                None,
                0.8
            )
            .text,
            "Calvinist views"
        );
    }
    #[test]
    fn paused_rule_does_not_apply() {
        let mut r = rule(1, "Calvin", "Kelvin", 1, 0);
        r.paused = true;
        assert_eq!(apply("hi Calvin", &[r], None, 0.8).text, "hi Calvin");
    }
    #[test]
    fn reverted_fix_is_a_miss() {
        let rules = [rule(1, "heiress", "Eres", 1, 0)];
        let a = apply("the heiress arrived", &rules, None, 0.8);
        assert_eq!(a.text, "the Eres arrived");
        let obs = observe(&a.text, "the heiress arrived", &a.applications, &rules);
        assert!(matches!(obs.as_slice(), [Observation::Miss(1)]));
    }
    #[test]
    fn kept_fix_is_a_hit() {
        let rules = [rule(1, "Calvin", "Kelvin", 1, 0)];
        let a = apply("hi Calvin", &rules, None, 0.8);
        assert!(matches!(
            observe(&a.text, &a.text, &a.applications, &rules).as_slice(),
            [Observation::Hit(1)]
        ));
    }
    #[test]
    fn kept_original_on_an_ambiguous_rule_is_a_miss() {
        // p = 1/3: the original was pasted and the fix offered. Leaving the
        // original alone is evidence against the rule, not for it.
        let rules = [rule(1, "heiress", "Eres", 1, 2)];
        let a = apply("the heiress inherited", &rules, None, 0.8);
        assert_eq!(a.text, "the heiress inherited");
        assert_eq!(a.applications[0].alternative.as_deref(), Some("Eres"));
        assert!(matches!(
            observe(&a.text, &a.text, &a.applications, &rules).as_slice(),
            [Observation::Miss(1)]
        ));
    }

    #[test]
    fn ambiguous_without_decider_offers_alternative() {
        let a = apply(
            "push the fix to heiress",
            &[rule(1, "heiress", "Eres", 1, 1)],
            None,
            0.8,
        );
        assert_eq!(a.applications[0].alternative.as_deref(), Some("heiress"));
    }
    struct Fixed(Vec<f64>);
    impl Decider for Fixed {
        fn choose(&self, _: &str, _: &[String]) -> Option<Vec<f64>> {
            Some(self.0.clone())
        }
    }
    #[test]
    fn sure_decider_settles_ambiguity() {
        let a = apply(
            "push the fix to heiress",
            &[rule(1, "heiress", "Eres", 1, 1)],
            Some(&Fixed(vec![0.9, 0.1])),
            0.8,
        );
        assert_eq!(a.text, "push the fix to Eres");
        assert!(a.applications[0].alternative.is_none());
    }
    #[test]
    fn unsure_decider_offers_alternative() {
        let a = apply(
            "the heiress inherited",
            &[rule(1, "heiress", "Eres", 2, 1)],
            Some(&Fixed(vec![0.6, 0.4])),
            0.8,
        );
        assert!(a.applications[0].alternative.is_some());
    }
    #[test]
    fn mostly_rejected_rule_keeps_original() {
        assert_eq!(
            apply(
                "the heiress",
                &[rule(1, "heiress", "Eres", 1, 4)],
                None,
                0.8
            )
            .text,
            "the heiress"
        );
    }

    #[test]
    fn application_ranges_point_into_the_output() {
        let rules = [
            rule(1, "Calvin", "Kelvin Smith", 1, 0),
            rule(2, "heiress", "Eres", 1, 0),
        ];
        let a = apply("Calvin, café heiress.", &rules, None, 0.8);
        assert_eq!(a.text, "Kelvin Smith, café Eres.");
        let got: Vec<&str> = a
            .applications
            .iter()
            .map(|x| &a.text[x.start..x.end])
            .collect();
        assert_eq!(got, ["Kelvin Smith", "Eres"]);
        assert_eq!(a.applications[1].original, "heiress");
    }

    #[test]
    fn longest_from_wins() {
        let rules = [
            rule(1, "get", "git", 1, 0),
            rule(2, "get hub", "GitHub", 1, 0),
        ];
        assert_eq!(
            apply("push to get hub", &rules, None, 0.8).text,
            "push to GitHub"
        );
    }

    #[test]
    fn rule_that_changes_nothing_records_nothing() {
        let a = apply("open Eres", &[rule(1, "eres", "Eres", 1, 0)], None, 0.8);
        assert_eq!(a.text, "open Eres");
        assert!(a.applications.is_empty());
    }

    #[test]
    fn case_only_edit_is_learned_as_spelling() {
        let obs = observe("open eres now", "open Eres now", &[], &[]);
        let [Observation::New(n)] = obs.as_slice() else {
            panic!("{obs:?}")
        };
        assert_eq!(n.kind, RuleKind::Spelling);
    }

    #[test]
    fn repeating_a_known_fix_is_a_hit() {
        let rules = [rule(7, "Calvin", "Kelvin", 1, 0)];
        let obs = observe("hi Calvin", "hi Kelvin", &[], &rules);
        assert_eq!(obs, vec![Observation::Hit(7)]);
    }

    #[test]
    fn phrase_edit_is_learned_as_phrase() {
        let obs = observe("push to get hub now", "push to GitHub now", &[], &[]);
        let [Observation::New(n)] = obs.as_slice() else {
            panic!("{obs:?}")
        };
        assert_eq!(
            (n.kind, n.from.as_str(), n.to.as_str()),
            (RuleKind::Phrase, "get hub", "GitHub")
        );
    }

    #[test]
    fn edits_that_change_the_meaning_are_not_learned() {
        for (pasted, edited) in [
            ("meet on Tuesday", "meet on Thursday"),
            ("it can work", "it can't work"),
            ("see you Monday", "see you Sunday"),
            ("me to", "me too"),
            ("sit in the car", "sit on the car"),
            ("the dog", "they dog"),
            ("in March", "in May"),
            ("take 15 of them", "take 50 of them"),
            ("it has colour", "it has no colour"),
        ] {
            assert!(
                observe(pasted, edited, &[], &[]).is_empty(),
                "{pasted:?} -> {edited:?}"
            );
        }
    }

    #[test]
    fn a_capital_after_a_full_stop_is_not_learned() {
        // The fix was a full stop. "so" must not become "So" everywhere.
        assert!(observe("we left so it rained", "we left. So it rained", &[], &[]).is_empty());
        assert!(observe(
            "it rained then we left",
            "it rained. Then we left",
            &[],
            &[]
        )
        .is_empty());
    }

    /// Records every sentence it is asked about and answers with `answer`.
    struct Recording {
        asked: std::cell::RefCell<Vec<String>>,
        answer: Option<Vec<f64>>,
        delay: std::time::Duration,
    }
    impl Recording {
        fn new(answer: Option<Vec<f64>>) -> Self {
            Self::slow(answer, std::time::Duration::ZERO)
        }
        fn slow(answer: Option<Vec<f64>>, delay: std::time::Duration) -> Self {
            Self {
                asked: Default::default(),
                answer,
                delay,
            }
        }
        fn asked(&self) -> Vec<String> {
            self.asked.borrow().clone()
        }
    }
    impl Decider for Recording {
        fn choose(&self, sentence: &str, _: &[String]) -> Option<Vec<f64>> {
            self.asked.borrow_mut().push(sentence.to_string());
            std::thread::sleep(self.delay);
            self.answer.clone()
        }
    }

    #[test]
    fn the_decider_gets_only_the_sentence_that_holds_the_match() {
        let rules = [rule(1, "heiress", "Eres", 1, 1)];
        let decider = Recording::new(Some(vec![0.5, 0.5]));
        apply(
            "We met at noon. Then we pushed the fix to heiress today! Nobody replied.",
            &rules,
            Some(&decider),
            0.8,
        );
        assert_eq!(decider.asked(), ["Then we pushed the fix to ___ today!"]);
    }

    #[test]
    fn a_line_break_ends_the_sentence_sent_to_the_decider() {
        let rules = [rule(1, "heiress", "Eres", 1, 1)];
        let decider = Recording::new(None);
        apply(
            "Dear team\nthe heiress is ready\nBest regards",
            &rules,
            Some(&decider),
            0.8,
        );
        assert_eq!(decider.asked(), ["the ___ is ready"]);
        let decider = Recording::new(None);
        apply("heiress", &rules, Some(&decider), 0.8);
        assert_eq!(decider.asked(), ["___"]);
    }

    #[test]
    fn the_decider_gets_a_total_time_budget_for_one_paste() {
        let rules = [rule(1, "heiress", "Eres", 1, 1)];
        let decider = Recording::slow(Some(vec![0.5, 0.5]), std::time::Duration::from_millis(60));
        let started = std::time::Instant::now();
        let applied = apply_within(
            "heiress one. heiress two. heiress three. heiress four. heiress five.",
            &rules,
            Some(&decider),
            0.8,
            std::time::Duration::from_millis(100),
        );
        // Two calls fit in the budget; the rest take the no-decider path.
        assert_eq!(decider.asked().len(), 2);
        assert!(started.elapsed() < std::time::Duration::from_millis(400));
        assert_eq!(applied.applications.len(), 5);
        assert!(applied.applications.iter().all(|a| a.alternative.is_some()));
    }

    #[test]
    fn a_decider_that_fails_once_is_not_asked_again() {
        let rules = [rule(1, "heiress", "Eres", 1, 1)];
        let decider = Recording::new(None);
        let applied = apply("heiress. heiress. heiress.", &rules, Some(&decider), 0.8);
        assert_eq!(decider.asked().len(), 1);
        assert_eq!(applied.applications.len(), 3);
    }
}

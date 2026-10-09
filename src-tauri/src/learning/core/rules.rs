//! The rule type and its ambiguity verdict.

use serde::{Deserialize, Serialize};
use specta::Type;

/// A rule with at least this share of confirmations is applied without doubt.
pub const APPLY_AT: f64 = 0.75;
/// A rule with at most this share of confirmations keeps the original text.
pub const KEEP_AT: f64 = 0.25;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum RuleKind {
    Word,
    Phrase,
    Spelling,
}

impl RuleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleKind::Word => "word",
            RuleKind::Phrase => "phrase",
            RuleKind::Spelling => "spelling",
        }
    }

    pub fn parse(s: &str) -> Option<RuleKind> {
        match s {
            "word" => Some(RuleKind::Word),
            "phrase" => Some(RuleKind::Phrase),
            "spelling" => Some(RuleKind::Spelling),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct Rule {
    pub id: i64,
    pub from: String,
    pub to: String,
    pub kind: RuleKind,
    pub hits: u32,
    pub misses: u32,
    pub applied: u32,
    pub paused: bool,
    pub source_sentence: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// What to do when a rule matches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verdict {
    /// Replace `from` with `to`.
    Apply,
    /// Leave the original text.
    Keep,
    /// The evidence runs both ways. `p_to` is the share of confirmations.
    Ambiguous { p_to: f64 },
}

impl Rule {
    /// With `p = hits / (hits + misses)`: no misses, or `p >= 0.75`, applies the
    /// rule; `p <= 0.25` keeps the original; anything between is ambiguous.
    /// A paused rule always keeps the original.
    pub fn verdict(&self) -> Verdict {
        if self.paused {
            return Verdict::Keep;
        }
        if self.misses == 0 {
            return Verdict::Apply;
        }
        let p = f64::from(self.hits) / f64::from(self.hits + self.misses);
        if p >= APPLY_AT {
            Verdict::Apply
        } else if p <= KEEP_AT {
            Verdict::Keep
        } else {
            Verdict::Ambiguous { p_to: p }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(hits: u32, misses: u32, paused: bool) -> Rule {
        Rule {
            id: 1,
            from: "a".into(),
            to: "b".into(),
            kind: RuleKind::Word,
            hits,
            misses,
            applied: 0,
            paused,
            source_sentence: String::new(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn no_misses_applies() {
        assert_eq!(rule(1, 0, false).verdict(), Verdict::Apply);
        assert_eq!(rule(0, 0, false).verdict(), Verdict::Apply);
    }

    #[test]
    fn band_edges() {
        assert_eq!(rule(3, 1, false).verdict(), Verdict::Apply); // p = 0.75
        assert_eq!(rule(1, 3, false).verdict(), Verdict::Keep); // p = 0.25
        assert!(matches!(rule(1, 1, false).verdict(), Verdict::Ambiguous { p_to } if p_to == 0.5));
        assert!(matches!(
            rule(2, 1, false).verdict(),
            Verdict::Ambiguous { .. }
        ));
        assert_eq!(rule(1, 4, false).verdict(), Verdict::Keep);
    }

    #[test]
    fn paused_keeps() {
        assert_eq!(rule(5, 0, true).verdict(), Verdict::Keep);
    }

    #[test]
    fn kind_round_trips_through_text() {
        for k in [RuleKind::Word, RuleKind::Phrase, RuleKind::Spelling] {
            assert_eq!(RuleKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(RuleKind::parse("nope"), None);
    }
}

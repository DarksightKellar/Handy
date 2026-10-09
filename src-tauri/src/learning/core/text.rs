//! Tokens and normalisation.

/// One whitespace-separated word of a source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    /// The word as written, with its punctuation.
    pub raw: String,
    /// Lower case, leading and trailing punctuation removed.
    pub norm: String,
    /// Byte offset of `raw` in the source (inclusive).
    pub start: usize,
    /// Byte offset of `raw` in the source (exclusive).
    pub end: usize,
}

/// Splits `s` on whitespace. A word with no letter or digit (a lone dash, for
/// example) carries no text and is dropped.
pub fn tokenize(s: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut word_start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        match (c.is_whitespace(), word_start) {
            (true, Some(start)) => {
                push_token(s, start, i, &mut tokens);
                word_start = None;
            }
            (false, None) => word_start = Some(i),
            _ => {}
        }
    }
    if let Some(start) = word_start {
        push_token(s, start, s.len(), &mut tokens);
    }
    tokens
}

fn push_token(s: &str, start: usize, end: usize, out: &mut Vec<Token>) {
    let raw = &s[start..end];
    let norm = strip_edges(raw).to_lowercase();
    if norm.is_empty() {
        return;
    }
    out.push(Token {
        raw: raw.to_string(),
        norm,
        start,
        end,
    });
}

/// `raw` without leading and trailing characters that are not letters or digits.
pub fn strip_edges(raw: &str) -> &str {
    raw.trim_matches(|c: char| !c.is_alphanumeric())
}

/// The tokens' `norm` values joined by single spaces.
pub fn normalize_phrase(s: &str) -> String {
    tokenize(s)
        .into_iter()
        .map(|t| t.norm)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Lower-case letters and digits only: "Type Safe" becomes "typesafe".
pub fn letters_only(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_keeps_offsets_and_strips_punctuation_in_norm() {
        let t = tokenize("  Hi, Calvin! ");
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].raw.as_str(), t[0].norm.as_str()), ("Hi,", "hi"));
        assert_eq!((t[0].start, t[0].end), (2, 5));
        assert_eq!(
            (t[1].raw.as_str(), t[1].norm.as_str()),
            ("Calvin!", "calvin")
        );
        assert_eq!((t[1].start, t[1].end), (6, 13));
    }

    #[test]
    fn tokenize_handles_multibyte_text() {
        let s = "café, naïve";
        let t = tokenize(s);
        assert_eq!(&s[t[1].start..t[1].end], "naïve");
        assert_eq!(t[0].norm, "café");
    }

    #[test]
    fn tokenize_drops_punctuation_only_words() {
        assert_eq!(tokenize("a — b").len(), 2);
    }

    #[test]
    fn normalize_phrase_collapses_space_and_case() {
        assert_eq!(normalize_phrase("  Get   HUB, "), "get hub");
    }

    #[test]
    fn letters_only_drops_spaces_and_hyphens() {
        assert_eq!(letters_only("Type Safe"), "typesafe");
        assert_eq!(letters_only("type-safe"), "typesafe");
    }
}

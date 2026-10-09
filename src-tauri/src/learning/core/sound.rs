//! The sound-alike and spelling check.

use super::text::{letters_only, tokenize};
use strsim::normalized_levenshtein;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditClass {
    Spelling,
    SoundAlike,
    ChangeOfMind,
}

/// The most words a phrase may have on either side of a rule.
pub const MAX_PHRASE_WORDS: usize = 4;

/// A single-token edit this similar (by letters) is a respelling.
const SPELLING_SIMILARITY: f64 = 0.75;
/// Two edits this similar by letters, or by phonetic key, sound alike.
const SOUND_SIMILARITY: f64 = 0.6;

/// Decides what kind of edit turned `old` into `new`.
///
/// `Spelling` and `SoundAlike` edits are worth learning: speech recognition
/// plausibly confused them. `ChangeOfMind` edits are the writer changing the
/// meaning, so they are not learned.
pub fn classify(old: &str, new: &str) -> EditClass {
    let old_tokens = tokenize(old);
    let new_tokens = tokenize(new);
    if old_tokens.is_empty()
        || new_tokens.is_empty()
        || old_tokens.len() > MAX_PHRASE_WORDS
        || new_tokens.len() > MAX_PHRASE_WORDS
    {
        return EditClass::ChangeOfMind;
    }

    let old_letters = letters_only(old);
    let new_letters = letters_only(new);
    if old_letters.is_empty() || new_letters.is_empty() {
        return EditClass::ChangeOfMind;
    }

    if old_letters == new_letters && old.trim() != new.trim() {
        return EditClass::Spelling;
    }
    let letter_similarity = normalized_levenshtein(&old_letters, &new_letters);
    if old_tokens.len() == 1 && new_tokens.len() == 1 && letter_similarity >= SPELLING_SIMILARITY {
        return EditClass::Spelling;
    }

    let old_key = phonetic_key(old);
    let new_key = phonetic_key(new);
    let key_similarity = if old_key.is_empty() || new_key.is_empty() {
        0.0
    } else {
        normalized_levenshtein(&old_key, &new_key)
    };
    if key_similarity >= SOUND_SIMILARITY || letter_similarity >= SOUND_SIMILARITY {
        EditClass::SoundAlike
    } else {
        EditClass::ChangeOfMind
    }
}

/// A small metaphone-style key: the consonant skeleton of each word, joined
/// without spaces. "Calvin" and "Kelvin" both give "klfn".
fn phonetic_key(s: &str) -> String {
    let joined: String = tokenize(s)
        .iter()
        .map(|t| word_key(&letters_only(&t.norm)))
        .collect();
    dedup_repeats(&joined)
}

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y')
}

fn word_key(word: &str) -> String {
    let word = word.replace("ph", "f").replace("ck", "k");
    let mut chars: Vec<char> = word.chars().collect();
    // A leading "h" is silent ("heiress", "hour"). The vowel after it then leads.
    if chars.len() > 1 && chars[0] == 'h' {
        chars.remove(0);
    }

    let mut key = String::new();
    let mut after_consonant = false;
    for (i, &c) in chars.iter().enumerate() {
        if is_vowel(c) {
            if i == 0 {
                key.push('a');
            }
            after_consonant = false;
            continue;
        }
        // "h" after a consonant only colours it ("th", "sh", "GitHub").
        if c == 'h' && after_consonant {
            continue;
        }
        match c {
            'c' | 'q' => key.push('k'),
            'v' => key.push('f'),
            'z' => key.push('s'),
            'j' => key.push('g'),
            'x' => key.push_str("ks"),
            other => key.push(other),
        }
        after_consonant = true;
    }
    dedup_repeats(&key)
}

fn dedup_repeats(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if !out.ends_with(c) {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calvin_kelvin_sounds_alike() {
        assert!(matches!(
            classify("Calvin", "Kelvin"),
            EditClass::SoundAlike
        ));
    }
    #[test]
    fn heiress_eres_sounds_alike() {
        assert!(matches!(classify("heiress", "Eres"), EditClass::SoundAlike));
    }
    #[test]
    fn jeff_jev_sounds_alike() {
        assert!(matches!(classify("Jeff", "Jev"), EditClass::SoundAlike));
    }
    #[test]
    fn get_hub_github_sounds_alike() {
        assert!(!matches!(
            classify("get hub", "GitHub"),
            EditClass::ChangeOfMind
        ));
    }
    #[test]
    fn type_safe_is_spelling() {
        assert!(matches!(
            classify("type safe", "TypeSafe"),
            EditClass::Spelling
        ));
    }
    #[test]
    fn eres_case_is_spelling() {
        assert!(matches!(classify("eres", "Eres"), EditClass::Spelling));
    }
    #[test]
    fn ok_okay_is_learnable() {
        assert!(!matches!(classify("ok", "okay"), EditClass::ChangeOfMind));
    }
    #[test]
    fn tuesday_friday_is_change_of_mind() {
        assert!(matches!(
            classify("Tuesday", "Friday"),
            EditClass::ChangeOfMind
        ));
    }
    #[test]
    fn send_forward_is_change_of_mind() {
        assert!(matches!(
            classify("send", "forward"),
            EditClass::ChangeOfMind
        ));
    }
    #[test]
    fn cat_dog_is_change_of_mind() {
        assert!(matches!(classify("cat", "dog"), EditClass::ChangeOfMind));
    }
    #[test]
    fn five_words_is_too_long() {
        assert!(matches!(
            classify("a b c d e", "a b c d f"),
            EditClass::ChangeOfMind
        ));
    }
}

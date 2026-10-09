//! The sound-alike and spelling check.

use super::text::{letters_only, normalize_phrase, tokenize};
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
/// A single word is judged by its letters alone only when both words are at
/// least this long and start with the same letter. Shorter words differ in
/// meaning with one letter ("bat", "cat"); words with another first letter are
/// other words ("sunny", "funny").
const MIN_RESPELLING_LETTERS: usize = 5;

/// Words that carry grammar or a fixed scale, not content. Speech recognition
/// does confuse them ("to", "too"), but a fix between two of them changes the
/// meaning of the sentence as often as it repairs a mishearing, and a learned
/// rule would apply from its first sighting. So an edit that has one on either
/// side is never learned.
const CLOSED_CLASS: &[&str] = &[
    // articles, determiners, quantifiers
    "a",
    "an",
    "the",
    "this",
    "that",
    "these",
    "those",
    "some",
    "any",
    "each",
    "every",
    "all",
    "both",
    "either",
    "neither",
    "many",
    "much",
    "more",
    "most",
    "few",
    "less",
    "other",
    "another",
    "such",
    "own",
    "same",
    // pronouns
    "i",
    "me",
    "my",
    "mine",
    "myself",
    "you",
    "your",
    "yours",
    "yourself",
    "he",
    "him",
    "his",
    "himself",
    "she",
    "her",
    "hers",
    "herself",
    "it",
    "its",
    "it's",
    "itself",
    "we",
    "us",
    "our",
    "ours",
    "ourselves",
    "they",
    "them",
    "their",
    "theirs",
    "themselves",
    "who",
    "whom",
    "whose",
    "which",
    "what",
    "whatever",
    "whoever",
    "one",
    "ones",
    "someone",
    "something",
    "anyone",
    "anything",
    "everyone",
    "everything",
    "nobody",
    "nothing",
    "none",
    // contractions
    "i'm",
    "i've",
    "i'll",
    "i'd",
    "you're",
    "you've",
    "you'll",
    "you'd",
    "he's",
    "he'll",
    "he'd",
    "she's",
    "she'll",
    "she'd",
    "we're",
    "we've",
    "we'll",
    "we'd",
    "they're",
    "they've",
    "they'll",
    "they'd",
    "that's",
    "there's",
    "here's",
    "what's",
    "who's",
    "let's",
    "where's",
    "how's",
    // prepositions
    "of",
    "in",
    "on",
    "at",
    "by",
    "for",
    "with",
    "about",
    "against",
    "between",
    "into",
    "through",
    "during",
    "before",
    "after",
    "above",
    "below",
    "to",
    "from",
    "up",
    "down",
    "out",
    "off",
    "over",
    "under",
    "again",
    "further",
    "onto",
    "upon",
    "within",
    "without",
    "across",
    "along",
    "around",
    "behind",
    "beside",
    "beyond",
    "near",
    "toward",
    "towards",
    "until",
    "till",
    "per",
    "via",
    "among",
    "inside",
    "outside",
    "past",
    "since",
    // conjunctions
    "and",
    "or",
    "but",
    "nor",
    "so",
    "yet",
    "if",
    "because",
    "as",
    "while",
    "although",
    "though",
    "unless",
    "whether",
    "than",
    "then",
    "when",
    "where",
    "why",
    "how",
    "whenever",
    "wherever",
    // auxiliaries and modals, with their n't forms
    "am",
    "is",
    "are",
    "was",
    "were",
    "be",
    "been",
    "being",
    "have",
    "has",
    "had",
    "having",
    "do",
    "does",
    "did",
    "doing",
    "done",
    "will",
    "would",
    "shall",
    "should",
    "can",
    "could",
    "may",
    "might",
    "must",
    "ought",
    "isn't",
    "aren't",
    "wasn't",
    "weren't",
    "haven't",
    "hasn't",
    "hadn't",
    "don't",
    "doesn't",
    "didn't",
    "won't",
    "wouldn't",
    "shan't",
    "shouldn't",
    "can't",
    "cannot",
    "couldn't",
    "mightn't",
    "mustn't",
    "needn't",
    "isnt",
    "arent",
    "wasnt",
    "werent",
    "havent",
    "hasnt",
    "hadnt",
    "dont",
    "doesnt",
    "didnt",
    "wont",
    "wouldnt",
    "shouldnt",
    "cant",
    "couldnt",
    // common adverbs
    "not",
    "no",
    "yes",
    "never",
    "always",
    "so",
    "too",
    "very",
    "just",
    "only",
    "also",
    "even",
    "still",
    "now",
    "here",
    "there",
    "already",
    "ever",
    "often",
    "soon",
    "quite",
    "rather",
    "really",
    "almost",
    "maybe",
    "perhaps",
    "once",
    "twice",
    "again",
    "else",
    "however",
    "therefore",
    "otherwise",
    "instead",
    "anyway",
    "together",
    "away",
    "back",
    "well",
    // weekdays and months
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    // number words
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "seventy",
    "eighty",
    "ninety",
    "hundred",
    "thousand",
    "million",
    "billion",
    "first",
    "second",
    "third",
    "fourth",
    "fifth",
    "sixth",
    "seventh",
    "eighth",
    "ninth",
    "tenth",
];

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
    // A fix between grammar words, days, months or numbers, or one that adds or
    // removes a "not", changes what the sentence says.
    if is_closed_class(old) || is_closed_class(new) || negation_count(old) != negation_count(new) {
        return EditClass::ChangeOfMind;
    }

    if old_letters == new_letters && old.trim() != new.trim() {
        return EditClass::Spelling;
    }
    let single_word = old_tokens.len() == 1 && new_tokens.len() == 1;
    // For one word on each side, similar letters count only for a plausible
    // respelling (see `MIN_RESPELLING_LETTERS`). The phonetic key still
    // catches the short words that really sound alike.
    let letters_count = !single_word || is_respelling_pair(&old_letters, &new_letters);
    let letter_similarity = if letters_count {
        normalized_levenshtein(&old_letters, &new_letters)
    } else {
        0.0
    };
    if single_word && letter_similarity >= SPELLING_SIMILARITY {
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

/// Both words are long enough and start alike.
fn is_respelling_pair(old_letters: &str, new_letters: &str) -> bool {
    old_letters.chars().count() >= MIN_RESPELLING_LETTERS
        && new_letters.chars().count() >= MIN_RESPELLING_LETTERS
        && old_letters.chars().next() == new_letters.chars().next()
}

/// The text with curly apostrophes made straight, so "can’t" matches "can't".
fn straight_apostrophes(s: &str) -> String {
    s.replace(['\u{2019}', '\u{2018}'], "'")
}

/// True when every word of `side` is a closed-class word or a number, so the
/// side as a whole is one.
fn is_closed_class(side: &str) -> bool {
    let normalized = straight_apostrophes(&normalize_phrase(side));
    if normalized.is_empty() {
        return false;
    }
    normalized.split(' ').all(|word| {
        CLOSED_CLASS.contains(&word)
            || word
                .chars()
                .filter(|c| c.is_alphanumeric())
                .all(|c| c.is_ascii_digit())
    })
}

/// How many negations `side` holds ("not", "no", "never", "cannot", "n't").
fn negation_count(side: &str) -> usize {
    tokenize(&straight_apostrophes(side))
        .iter()
        .filter(|t| {
            matches!(t.norm.as_str(), "not" | "no" | "never" | "cannot") || t.norm.ends_with("n't")
        })
        .count()
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

    // ---- the gate refuses changes of meaning ----------------------------

    fn refused(old: &str, new: &str) {
        assert_eq!(
            classify(old, new),
            EditClass::ChangeOfMind,
            "{old:?} -> {new:?}"
        );
    }

    #[test]
    fn function_words_are_never_a_mishearing_rule() {
        refused("to", "too");
        refused("in", "on");
        refused("the", "they");
        refused("can", "can't");
        refused("their", "there");
        refused("then", "than");
        refused("no", "now");
        refused("not", "now");
        refused("its", "it's");
        refused("in the", "on the");
    }

    #[test]
    fn weekdays_months_and_numbers_are_never_a_mishearing_rule() {
        refused("Tuesday", "Thursday");
        refused("Monday", "Sunday");
        refused("March", "May");
        refused("June", "July");
        refused("fifteen", "fifty");
        refused("third", "thirds");
        refused("10", "11");
        refused("1,000", "10,000");
    }

    #[test]
    fn adding_or_removing_a_negation_is_a_change_of_meaning() {
        refused("colour", "not colour");
        refused("works", "never works");
        refused("didn't work", "worked");
        refused("sound", "no sound");
    }

    #[test]
    fn a_similar_spelling_needs_the_same_first_letter_and_five_letters() {
        // Different first letter: a different word, not a respelling.
        refused("sunny", "funny");
        // Too short to tell a respelling from a different word.
        refused("bat", "cat");
        // Real respellings stay learnable.
        assert_eq!(classify("colour", "color"), EditClass::Spelling);
        assert_eq!(classify("definately", "definitely"), EditClass::Spelling);
    }
}

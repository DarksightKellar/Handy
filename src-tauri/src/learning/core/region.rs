//! The watch state machine: find the pasted region inside a text-box value that
//! keeps changing, and report the last good reading of it.

use std::collections::HashMap;

use super::text::tokenize;

/// Characters of context kept on each side of the pasted region.
const ANCHOR_CHARS: usize = 24;
/// An anchor may shrink toward the region (so an edit next to the region does
/// not hide it), but never below this many characters: a shorter anchor would
/// match unrelated text.
const MIN_ANCHOR_CHARS: usize = 4;

pub struct Watch {
    pasted: String,
    before: String,
    after: String,
    last_region: Option<String>,
    misses_in_row: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WatchStep {
    Continue,
    /// `Some(edited_region)`, or `None` when there is nothing to learn.
    Finish(Option<String>),
}

impl Watch {
    /// `value_after_paste` is the field value read just after the paste. Returns
    /// `None` when it does not contain `pasted`.
    pub fn start(value_after_paste: &str, pasted: &str) -> Option<Watch> {
        if pasted.is_empty() {
            return None;
        }
        let at = value_after_paste.rfind(pasted)?;
        let before_all = &value_after_paste[..at];
        let after_all = &value_after_paste[at + pasted.len()..];
        Some(Watch {
            pasted: pasted.to_string(),
            before: last_chars(before_all, ANCHOR_CHARS).to_string(),
            after: first_chars(after_all, ANCHOR_CHARS).to_string(),
            last_region: None,
            misses_in_row: 0,
        })
    }

    /// Feeds one later reading. `None` means the element is gone or focus moved.
    ///
    /// A reading in which the region cannot be found counts as a miss; two in a
    /// row end the watch (the box was cleared, as when a message is sent). The
    /// last good region is still reported.
    pub fn feed(&mut self, reading: Option<&str>) -> WatchStep {
        let Some(reading) = reading else {
            return WatchStep::Finish(self.result());
        };
        match self.locate(reading) {
            Some(region) => {
                self.last_region = Some(region.to_string());
                self.misses_in_row = 0;
                WatchStep::Continue
            }
            None => {
                self.misses_in_row += 1;
                if self.misses_in_row >= 2 {
                    WatchStep::Finish(self.result())
                } else {
                    WatchStep::Continue
                }
            }
        }
    }

    /// True when the last good reading shows the pasted text as pasted. The
    /// watch then has nothing to learn from an edit, but it did see the text
    /// survive, which confirms every fix Handy made in it.
    pub fn is_unchanged(&self) -> bool {
        self.last_region.as_deref() == Some(self.pasted.as_str())
    }

    /// Ends the watch at the timeout.
    pub fn finish(self) -> Option<String> {
        self.result()
    }

    fn result(&self) -> Option<String> {
        self.last_region
            .as_ref()
            .filter(|r| **r != self.pasted)
            .cloned()
    }

    /// The text between the before-anchor and the after-anchor, if both are
    /// present in order and the text between them is not blank.
    fn locate<'a>(&self, reading: &'a str) -> Option<&'a str> {
        let region = if self.before.is_empty() {
            self.find_end(reading, 0)
        } else {
            shrinking_suffixes(&self.before)
                .into_iter()
                .flat_map(|anchor| {
                    reading
                        .match_indices(anchor)
                        .map(move |(i, _)| i + anchor.len())
                })
                .find_map(|from| self.find_end(reading, from).map(|r| (from, r)))
                .map(|(from, end)| &reading[from..from + end.len()])
        }?;
        (!region.trim().is_empty() && self.resembles_paste(region)).then_some(region)
    }

    /// Whether `region` can be the pasted text, edited. With no text before and
    /// after the paste the anchors match anything, so a message typed after
    /// the box was cleared would pass as the region. A region that keeps fewer
    /// than half of the pasted words is not the paste. A one- or two-word paste
    /// has too few words to count; its edit may replace most of them, so it may
    /// grow by one word at most.
    fn resembles_paste(&self, region: &str) -> bool {
        let pasted = tokenize(&self.pasted);
        let mut available: HashMap<String, usize> = HashMap::new();
        for token in tokenize(region) {
            *available.entry(token.norm).or_default() += 1;
        }
        let region_words: usize = available.values().sum();
        let shared = pasted
            .iter()
            .filter(|t| match available.get_mut(&t.norm) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    true
                }
                _ => false,
            })
            .count();
        shared * 2 >= pasted.len() || (pasted.len() <= 2 && region_words <= pasted.len() + 1)
    }

    /// The part of `reading[from..]` that ends at the after-anchor (or at the
    /// end of the text when the anchor is empty).
    fn find_end<'a>(&self, reading: &'a str, from: usize) -> Option<&'a str> {
        let rest = &reading[from..];
        if self.after.is_empty() {
            return Some(rest);
        }
        shrinking_prefixes(&self.after)
            .into_iter()
            .find_map(|anchor| rest.find(anchor))
            .map(|end| &rest[..end])
    }
}

/// The anchor, then shorter ends of it, longest first, down to the minimum.
fn shrinking_suffixes(anchor: &str) -> Vec<&str> {
    let n = anchor.chars().count();
    let floor = MIN_ANCHOR_CHARS.min(n);
    (floor..=n).rev().map(|k| last_chars(anchor, k)).collect()
}

fn shrinking_prefixes(anchor: &str) -> Vec<&str> {
    let n = anchor.chars().count();
    let floor = MIN_ANCHOR_CHARS.min(n);
    (floor..=n).rev().map(|k| first_chars(anchor, k)).collect()
}

fn last_chars(s: &str, n: usize) -> &str {
    let total = s.chars().count();
    match s.char_indices().nth(total.saturating_sub(n)) {
        Some((i, _)) => &s[i..],
        None => "",
    }
}

fn first_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf1_send_clears_box_but_edit_is_kept() {
        let mut w = Watch::start("send it to Calvin", "send it to Calvin").unwrap();
        assert!(matches!(
            w.feed(Some("send it to Kelvin")),
            WatchStep::Continue
        ));
        assert!(matches!(w.feed(Some("")), WatchStep::Continue));
        assert!(
            matches!(w.feed(Some("")), WatchStep::Finish(Some(ref s)) if s == "send it to Kelvin")
        );
    }
    #[test]
    fn rf2_edit_before_region_is_tolerated() {
        let mut w = Watch::start("Hi team. send it to Calvin", "send it to Calvin").unwrap();
        w.feed(Some("Hi all of the team. send it to Kelvin"));
        assert_eq!(w.finish().as_deref(), Some("send it to Kelvin"));
    }
    #[test]
    fn rf2_region_not_found_learns_nothing() {
        let mut w = Watch::start("abc send it to Calvin xyz", "send it to Calvin").unwrap();
        w.feed(Some("completely different text"));
        assert_eq!(w.finish(), None);
    }
    #[test]
    fn paste_missing_from_field_does_not_start() {
        assert!(Watch::start("unrelated", "send it").is_none());
    }
    #[test]
    fn unchanged_text_is_reported_as_unchanged() {
        let mut w = Watch::start("x hello Calvin y", "hello Calvin").unwrap();
        assert!(!w.is_unchanged()); // nothing read yet
        w.feed(Some("x hello Calvin y"));
        assert!(w.is_unchanged());
        w.feed(Some("x hello Kelvin y"));
        assert!(!w.is_unchanged());
    }

    #[test]
    fn unchanged_text_learns_nothing() {
        let w = Watch::start("hello there", "hello there").unwrap();
        assert_eq!(w.finish(), None);
    }
    #[test]
    fn focus_lost_finishes_with_last_good() {
        let mut w = Watch::start("x hello Calvin y", "hello Calvin").unwrap();
        w.feed(Some("x hello Kelvin y"));
        assert!(matches!(w.feed(None), WatchStep::Finish(Some(ref s)) if s == "hello Kelvin"));
    }

    #[test]
    fn long_context_is_cut_to_the_anchor_size_on_multibyte_text() {
        let before = "ééééééééééééééééééééééééééééééé ";
        let value = format!("{before}send it to Calvin tail");
        let mut w = Watch::start(&value, "send it to Calvin").unwrap();
        w.feed(Some(&format!("{before}send it to Kelvin tail")));
        assert_eq!(w.finish().as_deref(), Some("send it to Kelvin"));
    }

    #[test]
    fn a_good_reading_resets_the_miss_count() {
        let mut w = Watch::start("send it to Calvin", "send it to Calvin").unwrap();
        assert_eq!(w.feed(Some("")), WatchStep::Continue);
        assert_eq!(w.feed(Some("send it to Kelvin")), WatchStep::Continue);
        assert_eq!(w.feed(Some("   ")), WatchStep::Continue);
        assert_eq!(w.feed(Some("send it to Kelvin")), WatchStep::Continue);
    }

    #[test]
    fn edit_reverted_to_the_paste_learns_nothing() {
        let mut w = Watch::start("hi Calvin", "hi Calvin").unwrap();
        w.feed(Some("hi Kelvin"));
        w.feed(Some("hi Calvin"));
        assert_eq!(w.finish(), None);
    }

    #[test]
    fn text_typed_after_the_region_is_not_part_of_it() {
        let mut w = Watch::start("hello Calvin | tail end", "hello Calvin").unwrap();
        w.feed(Some("hello Kelvin | tail end"));
        assert_eq!(w.finish().as_deref(), Some("hello Kelvin"));
    }

    #[test]
    fn a_message_typed_after_sending_is_not_the_pasted_region() {
        // Empty box before the paste: both anchors are empty.
        let mut w = Watch::start(
            "send the report to Kelvin today",
            "send the report to Kelvin today",
        )
        .unwrap();
        assert_eq!(w.feed(Some("")), WatchStep::Continue);
        assert_eq!(
            w.feed(Some("thanks, see you at the meeting tomorrow morning")),
            WatchStep::Finish(None)
        );
    }

    #[test]
    fn unrelated_text_does_not_replace_the_last_good_region() {
        let mut w = Watch::start(
            "send the report to Calvin today",
            "send the report to Calvin today",
        )
        .unwrap();
        w.feed(Some("send the report to Kelvin today"));
        assert_eq!(
            w.feed(Some("a brand new message about lunch plans")),
            WatchStep::Continue
        );
        assert_eq!(
            w.feed(Some("another new message about dinner")),
            WatchStep::Finish(Some("send the report to Kelvin today".to_string()))
        );
    }

    #[test]
    fn a_short_paste_can_still_be_edited() {
        let mut w = Watch::start("Calvin", "Calvin").unwrap();
        assert_eq!(w.feed(Some("Kelvin")), WatchStep::Continue);
        assert_eq!(w.finish().as_deref(), Some("Kelvin"));
    }
}

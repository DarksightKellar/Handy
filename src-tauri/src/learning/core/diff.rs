//! Word diff between pasted and edited text.

use super::text::{strip_edges, tokenize, Token};

/// One replaced span. Offsets are BYTE offsets of the hunk in `old`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub old: String,
    pub new: String,
    pub old_start: usize,
    pub old_end: usize,
}

/// Above this many cells the alignment table is not built. A diff that large is
/// not a correction of a few words, so the layer learns nothing from it.
const MAX_TABLE_CELLS: usize = 4_000_000;

enum Op {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

/// The replacement hunks that turn `old` into `new`, compared word by word.
///
/// Words are matched by their normalised form, so punctuation changes are not
/// hunks. A word whose letters differ only in case is a hunk (so capitals are
/// learnt). Adjacent changes merge into one hunk. Pure insertions and pure
/// deletions are dropped. Hunk text has its edge punctuation stripped.
pub fn word_hunks(old: &str, new: &str) -> Vec<Hunk> {
    let a = tokenize(old);
    let b = tokenize(new);
    let Some(ops) = align(&a, &b) else {
        return Vec::new();
    };

    let mut hunks = Vec::new();
    let mut old_run: Vec<usize> = Vec::new();
    let mut new_run: Vec<usize> = Vec::new();
    for op in ops {
        match op {
            Op::Equal(i, j) if strip_edges(&a[i].raw) == strip_edges(&b[j].raw) => {
                flush(old, new, &a, &b, &mut old_run, &mut new_run, &mut hunks);
            }
            // Same word, different case: a change.
            Op::Equal(i, j) => {
                old_run.push(i);
                new_run.push(j);
            }
            Op::Delete(i) => old_run.push(i),
            Op::Insert(j) => new_run.push(j),
        }
    }
    flush(old, new, &a, &b, &mut old_run, &mut new_run, &mut hunks);
    hunks
}

/// Turns the pending run of changed tokens into a hunk, if it replaces something.
fn flush(
    old: &str,
    new: &str,
    a: &[Token],
    b: &[Token],
    old_run: &mut Vec<usize>,
    new_run: &mut Vec<usize>,
    hunks: &mut Vec<Hunk>,
) {
    if let (Some(&of), Some(&ol), Some(&nf), Some(&nl)) = (
        old_run.first(),
        old_run.last(),
        new_run.first(),
        new_run.last(),
    ) {
        let (old_start, old_end) = tight(old, a[of].start, a[ol].end);
        let (new_start, new_end) = tight(new, b[nf].start, b[nl].end);
        hunks.push(Hunk {
            old: squash_space(&old[old_start..old_end]),
            new: squash_space(&new[new_start..new_end]),
            old_start,
            old_end,
        });
    }
    old_run.clear();
    new_run.clear();
}

/// Shrinks `start..end` so it begins and ends on a letter or digit.
fn tight(src: &str, start: usize, end: usize) -> (usize, usize) {
    let slice = &src[start..end];
    let is_edge = |c: char| !c.is_alphanumeric();
    let lead = slice.len() - slice.trim_start_matches(is_edge).len();
    let inner_len = slice.trim_matches(is_edge).len();
    (start + lead, start + lead + inner_len)
}

fn squash_space(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Longest-common-subsequence alignment of the two token lists by `norm`.
/// `None` when the texts are too different in size to align cheaply.
fn align(a: &[Token], b: &[Token]) -> Option<Vec<Op>> {
    let prefix = a
        .iter()
        .zip(b)
        .take_while(|(x, y)| x.norm == y.norm)
        .count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x.norm == y.norm)
        .count();
    let (n, m) = (a.len() - prefix - suffix, b.len() - prefix - suffix);
    if (n + 1).saturating_mul(m + 1) > MAX_TABLE_CELLS {
        return None;
    }

    // lcs[i][j] = LCS length of a_mid[i..] and b_mid[j..].
    let am = &a[prefix..prefix + n];
    let bm = &b[prefix..prefix + m];
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if am[i].norm == bm[j].norm {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut ops: Vec<Op> = (0..prefix).map(|i| Op::Equal(i, i)).collect();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && am[i].norm == bm[j].norm {
            ops.push(Op::Equal(prefix + i, prefix + j));
            i += 1;
            j += 1;
        } else if j == m || (i < n && lcs[i + 1][j] >= lcs[i][j + 1]) {
            ops.push(Op::Delete(prefix + i));
            i += 1;
        } else {
            ops.push(Op::Insert(prefix + j));
            j += 1;
        }
    }
    for k in 0..suffix {
        ops.push(Op::Equal(a.len() - suffix + k, b.len() - suffix + k));
    }
    Some(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_word_replacement() {
        let h = word_hunks("send it to Calvin today.", "send it to Kelvin today.");
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].old, "Calvin");
        assert_eq!(h[0].new, "Kelvin");
    }
    #[test]
    fn phrase_replacement_merges() {
        let h = word_hunks("push it to get hub now", "push it to GitHub now");
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].old, "get hub");
        assert_eq!(h[0].new, "GitHub");
    }
    #[test]
    fn case_only_change_is_a_hunk() {
        let h = word_hunks("open eres now", "open Eres now");
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].old, "eres");
        assert_eq!(h[0].new, "Eres");
    }
    #[test]
    fn punctuation_only_change_is_ignored() {
        assert!(word_hunks("hello world", "hello, world.").is_empty());
    }
    #[test]
    fn pure_insertion_is_ignored() {
        assert!(word_hunks("hello world", "hello big world").is_empty());
    }
    #[test]
    fn two_separate_hunks() {
        let h = word_hunks("Calvin pushed to heiress", "Kelvin pushed to Eres");
        assert_eq!(
            h.iter()
                .map(|x| (x.old.as_str(), x.new.as_str()))
                .collect::<Vec<_>>(),
            vec![("Calvin", "Kelvin"), ("heiress", "Eres")]
        );
    }

    #[test]
    fn hunk_offsets_are_byte_offsets_into_old() {
        let old = "café, Calvin! ok";
        let h = word_hunks(old, "café, Kelvin! ok");
        assert_eq!(h.len(), 1);
        assert_eq!(&old[h[0].old_start..h[0].old_end], "Calvin");
    }
}

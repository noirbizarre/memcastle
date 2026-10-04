//! Deterministic, explainable signals for "is this the same memory?".
//!
//! A drawer's `content` is canonical and never rewritten; everything here is a *derived* view of it, computed to
//! compare two texts. Nothing needs a model: an exact duplicate is equal bytes, a likely duplicate is equal after
//! normalisation or within a small edit distance. Whether two drawers are the same memory is MemCastle's decision
//! (docs/adr/025), and the database only proposes candidates.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::sha256_hex;

/// Texts up to this many characters (after normalisation) are compared by edit distance, which is exact but
/// quadratic. Longer ones are compared by shared character trigrams, which is linear and tolerant of a typo in
/// thousands of characters.
pub const EDIT_DISTANCE_LIMIT: usize = 2_000;

/// How two drawers resemble each other, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateKind {
    /// The same bytes. Same content hash.
    Exact,
    /// The same words once case, punctuation and whitespace are ignored.
    Normalized,
    /// Different enough to differ, close enough to be probably the same memory (a typo, a one-character edit).
    Near,
}

impl DuplicateKind {
    /// The stored label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Normalized => "normalized",
            Self::Near => "near",
        }
    }
}

/// `text` reduced to what a person would call "the same words": lowercased, every run of non-alphanumeric
/// characters (punctuation, whitespace, line breaks) collapsed to one space, trimmed.
///
/// Unicode-aware for case and for what counts as a letter, but it does not fold accents: `café` and `cafe` stay
/// different, because a wrong merge costs more than a missed one.
#[must_use]
pub fn normalize_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.extend(c.to_lowercase());
        } else {
            pending_space = true;
        }
    }
    out
}

/// A stable, indexable identity for `text`'s normalised form: two texts that differ only in case, punctuation or
/// whitespace share a fingerprint. Empty when `text` has no letters or digits at all, so a drawer of pure
/// punctuation is never "the same" as another.
#[must_use]
pub fn fingerprint(text: &str) -> String {
    let normal = normalize_text(text);
    if normal.is_empty() {
        String::new()
    } else {
        sha256_hex(normal.as_bytes())
    }
}

/// How alike two texts are, in `[0, 1]`: `1` for the same words, falling towards `0` as they diverge.
///
/// Both sides are normalised first. Short texts use edit similarity (`1 - distance / longer length`, counting an
/// adjacent swap as one edit); long texts use the Jaccard index of their character trigrams.
#[must_use]
pub fn similarity(a: &str, b: &str) -> f32 {
    similarity_of_normalized(&normalize_text(a), &normalize_text(b))
}

/// [`similarity`] for texts already passed through [`normalize_text`].
#[must_use]
pub fn similarity_of_normalized(a: &str, b: &str) -> f32 {
    if a == b {
        // Two empty texts have no words in common to call "the same".
        return if a.is_empty() { 0.0 } else { 1.0 };
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let longer = a.len().max(b.len());
    if longer == 0 {
        return 0.0;
    }
    if longer <= EDIT_DISTANCE_LIMIT {
        let distance = edit_distance(&a, &b);
        #[allow(clippy::cast_precision_loss)]
        return 1.0 - distance as f32 / longer as f32;
    }
    trigram_jaccard(&a, &b)
}

/// The optimal-string-alignment distance between `a` and `b`: the fewest insertions, deletions, substitutions and
/// adjacent transpositions turning one into the other.
///
/// Transpositions count as one edit because swapped neighbouring letters are the commonest typo. Three rolling
/// rows keep the memory linear.
#[must_use]
pub fn edit_distance(a: &[char], b: &[char]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let width = b.len() + 1;
    let mut two_back = vec![0usize; width];
    let mut previous: Vec<usize> = (0..width).collect();
    let mut current = vec![0usize; width];
    for i in 1..=a.len() {
        current[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(two_back[j - 2] + 1);
            }
            current[j] = best;
        }
        // Rotate without allocating: the oldest row becomes the next scratch row.
        std::mem::swap(&mut two_back, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// The Jaccard index of the character trigrams of `a` and `b`.
fn trigram_jaccard(a: &[char], b: &[char]) -> f32 {
    let grams = |text: &[char]| -> HashSet<[char; 3]> {
        text.windows(3).map(|w| [w[0], w[1], w[2]]).collect()
    };
    let (left, right) = (grams(a), grams(b));
    let union = left.union(&right).count();
    if union == 0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    {
        left.intersection(&right).count() as f32 / union as f32
    }
}

/// The evidence behind one duplicate verdict, stored on the `similar_to` edge so a later reader can see *why* two
/// drawers were linked and reverse the call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DuplicateSignals {
    /// Whether the content hashes were equal.
    pub same_hash: bool,
    /// Whether the normalised fingerprints were equal.
    pub same_fingerprint: bool,
    /// The similarity score in `[0, 1]`.
    pub similarity: f32,
    /// The threshold in force when the verdict was made.
    pub threshold: f32,
}

/// The verdict for one pair, from the hash, fingerprint and similarity of the new text against an existing one.
///
/// Returns `None` for a pair that is merely similar (`similarity` below `threshold`): those stay unrelated.
#[must_use]
pub fn classify(
    same_hash: bool,
    same_fingerprint: bool,
    similarity: f32,
    threshold: f32,
) -> Option<DuplicateKind> {
    if same_hash {
        Some(DuplicateKind::Exact)
    } else if same_fingerprint {
        Some(DuplicateKind::Normalized)
    } else if similarity >= threshold {
        Some(DuplicateKind::Near)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENTENCE: &str = "We decided to use SurrealDB for storage because one engine covers documents, \
                            graph and vectors.";

    #[test]
    fn identical_text_has_similarity_one() {
        assert!((similarity(SENTENCE, SENTENCE) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn case_punctuation_and_whitespace_do_not_change_the_fingerprint() {
        let variant = "we decided to use surrealdb for storage - because one engine covers documents,\n\
                       graph, and vectors";
        assert_eq!(fingerprint(SENTENCE), fingerprint(variant));
    }

    #[test]
    fn different_words_have_different_fingerprints() {
        assert_ne!(fingerprint("use port 8080"), fingerprint("use port 8081"));
    }

    #[test]
    fn a_text_without_letters_or_digits_has_no_fingerprint() {
        assert_eq!(fingerprint("... --- !!!"), "");
        assert!(similarity("...", "...").abs() < f32::EPSILON);
    }

    #[test]
    fn accents_are_not_folded_because_a_wrong_merge_costs_more_than_a_missed_one() {
        assert_ne!(fingerprint("cafe"), fingerprint("café"));
    }

    #[test]
    fn a_one_character_typo_in_a_sentence_is_close_to_the_original() {
        let typo = SENTENCE.replace("decided", "decidde");
        assert!(similarity(SENTENCE, &typo) >= 0.97);
        assert_ne!(fingerprint(SENTENCE), fingerprint(&typo));
    }

    #[test]
    fn swapped_neighbouring_letters_count_as_a_single_edit() {
        let a: Vec<char> = "storage".chars().collect();
        let b: Vec<char> = "sotrage".chars().collect();
        assert_eq!(edit_distance(&a, &b), 1);
    }

    #[test]
    fn an_unrelated_sentence_scores_far_below_the_threshold() {
        let other = "The nightly job compacts the write-ahead log and rotates the audit report.";
        assert!(similarity(SENTENCE, other) < 0.6);
    }

    #[test]
    fn similar_but_distinct_facts_fall_below_a_strict_threshold() {
        // Same shape, different decision: the part that matters is the part that differs.
        let a =
            "We decided to use Postgres for the billing service because the team knows it well.";
        let b =
            "We decided to use SurrealDB for the memory service because one engine covers more.";
        assert!(similarity(a, b) < 0.9);
    }

    #[test]
    fn long_texts_are_compared_by_trigrams_and_a_typo_barely_moves_the_score() {
        let long: String = (0..80)
            .map(|n| {
                format!(
                    "Item {n} says module m{n} owns task t{} in sprint {}. ",
                    n * 7919,
                    n % 7
                )
            })
            .collect();
        assert!(normalize_text(&long).chars().count() > EDIT_DISTANCE_LIMIT);
        let typo = long.replacen("module", "modlue", 1);
        assert!(similarity(&long, &typo) > 0.98);
        let unrelated: String = (0..80)
            .map(|n| {
                format!(
                    "Completely other words {} about nothing, number {n}. ",
                    n * 31
                )
            })
            .collect();
        assert!(similarity(&long, &unrelated) < 0.5);
    }

    #[test]
    fn classification_prefers_the_strongest_signal_and_ignores_mere_similarity() {
        assert_eq!(classify(true, true, 1.0, 0.9), Some(DuplicateKind::Exact));
        assert_eq!(
            classify(false, true, 1.0, 0.9),
            Some(DuplicateKind::Normalized)
        );
        assert_eq!(classify(false, false, 0.95, 0.9), Some(DuplicateKind::Near));
        assert_eq!(classify(false, false, 0.89, 0.9), None);
    }
}

//! The stand-in embedding model: deterministic vectors the dataset itself defines.
//!
//! Evaluating the retrieval engine (the index, the fusion, the filters) is a different question from evaluating an
//! embedding model, and mixing them makes a number mean nothing. So the harness computes vectors itself and hands them to
//! the daemon (`PUT /api/drawers/{id}/embedding`, `query_embedding`), which needs no provider and no model download.
//!
//! The model is a hashed bag of concepts. Each word is replaced by its concept when the dataset names one (so "sedan" and
//! "automobile" are one slot), stop words are dropped, and each remaining word lights one slot of a 768-dimensional unit
//! vector. Two texts are close when they share concepts, which is all a ranking test needs and is exactly as strong as
//! the dataset's `concepts` table says: nothing here claims to measure what a real model would.
//!
//! Each token also adds a faint dense component of its own. Without it, every text that shares no concept with the query
//! sits at exactly the same distance, and which of those the vector index returns at a cut-off is arbitrary: a metric
//! would then depend on luck. The faint component gives unrelated texts distinct, repeatable, tiny similarities instead.

use std::collections::BTreeMap;

use memcastle::domain::EMBEDDING_DIMENSION;

/// How strong the dense component is next to the one-hot slot (weight 1.0). Small enough never to outrank a shared
/// concept, large enough that no two unrelated texts tie.
const BACKGROUND: f32 = 0.01;

/// Words that carry no topic, left out so cosine similarity reflects what a text is about.
const STOP_WORDS: [&str; 22] = [
    "a", "an", "and", "at", "by", "for", "from", "in", "is", "it", "its", "of", "on", "s", "the",
    "to", "was", "were", "who", "with", "my", "i",
];

#[derive(Debug, Clone)]
pub struct Embedder {
    concepts: BTreeMap<String, String>,
}

impl Embedder {
    pub fn new(concepts: &BTreeMap<String, String>) -> Self {
        Self {
            concepts: concepts.clone(),
        }
    }

    /// A unit vector for `text`.
    pub fn embed(&self, text: &str) -> Vec<f32> {
        let mut vector = vec![0.0f32; EMBEDDING_DIMENSION];
        for word in text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .filter(|word| !STOP_WORDS.contains(&word.as_str()))
        {
            let token = self.concepts.get(&word).unwrap_or(&word);
            vector[slot(token)] += 1.0;
            background(token, &mut vector);
        }
        let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            vector.iter_mut().for_each(|x| *x /= norm);
        } else {
            // An all-zero vector has no direction, and cosine distance to it is undefined.
            vector[0] = 1.0;
        }
        vector
    }
}

/// Add `token`'s faint dense component to `vector`: the same pseudo-random values every time, from a xorshift seeded
/// with the token's hash.
fn background(token: &str, vector: &mut [f32]) {
    let mut state = u64::from(hash(token)) | 1;
    for value in vector {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        // The top 24 bits as a number in [-1, 1).
        let unit = (state >> 40) as f32 / (1u32 << 23) as f32 - 1.0;
        *value += BACKGROUND * unit;
    }
}

/// The slot a token lights: a 32-bit FNV-1a, which is stable across runs and platforms, unlike `DefaultHasher`, so
/// a baseline measured on one machine means the same thing on another.
fn slot(token: &str) -> usize {
    hash(token) as usize % EMBEDDING_DIMENSION
}

fn hash(token: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in token.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedder() -> Embedder {
        let concepts = BTreeMap::from([
            ("sedan".to_string(), "vehicle".to_string()),
            ("automobile".to_string(), "vehicle".to_string()),
        ]);
        Embedder::new(&concepts)
    }

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn a_vector_is_a_unit_vector_of_the_palaces_dimension() {
        let vector = embedder().embed("the sedan needs new brake pads");
        assert_eq!(vector.len(), EMBEDDING_DIMENSION);
        let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "{norm}");
    }

    #[test]
    fn the_same_text_always_gets_the_same_vector() {
        assert_eq!(
            embedder().embed("stable across runs"),
            embedder().embed("stable across runs")
        );
    }

    #[test]
    fn two_words_for_one_concept_are_as_close_as_one_word_repeated() {
        let embedder = embedder();
        let same = cosine(&embedder.embed("sedan"), &embedder.embed("automobile"));
        let different = cosine(&embedder.embed("sedan"), &embedder.embed("tomatoes"));
        assert!((same - 1.0).abs() < 1e-5, "{same}");
        assert!(different < 0.1, "{different}");
    }

    #[test]
    fn a_text_of_only_stop_words_still_has_a_direction() {
        let vector = embedder().embed("the of and");
        assert!((vector[0] - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn texts_that_share_nothing_are_not_exactly_equidistant_from_a_query() {
        let embedder = embedder();
        let query = embedder.embed("passport renewal");
        let a = cosine(&query, &embedder.embed("tomatoes need staking"));
        let b = cosine(&query, &embedder.embed("compost bin aerated"));
        assert!(
            a != b,
            "an exact tie would leave the order at a cut-off to chance"
        );
        assert!(
            a.abs() < 0.1 && b.abs() < 0.1,
            "but the background stays far below a shared concept: {a} {b}"
        );
    }
}

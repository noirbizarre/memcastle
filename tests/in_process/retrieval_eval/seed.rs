//! Loading a dataset into a daemon, over HTTP only.
//!
//! The harness is a client like any other: it writes drawers, supersedes them, links entities and attaches vectors
//! through the REST API, so what it measures is what a client of the daemon gets. The cost is that the daemon stamps
//! validity times itself, so time is handled in epochs: an instant is captured between two groups of writes, and a
//! temporal query is asked about those instants.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::dataset::Dataset;
use super::report::Ingest;
use super::vectors::Embedder;

/// Long enough that two instants taken either side of a write cannot tie, which is what existing temporal tests use too.
const TICK: Duration = Duration::from_millis(80);

/// What loading left behind, and what is needed to read results back in the dataset's terms.
pub struct Seeded {
    /// Drawer id (as the daemon assigned it) to the dataset's document id.
    pub doc_of: HashMap<String, String>,
    /// `epoch_end[e]` is an instant after every write of epoch `e` and before any write of epoch `e + 1`.
    pub epoch_end: Vec<DateTime<Utc>>,
    pub ingest: Ingest,
}

pub fn client() -> reqwest::Client {
    reqwest::Client::new()
}

/// Write `dataset` into the daemon at `base`, with vectors from `embedder`, plus `scale` filler documents.
pub async fn seed(base: &str, dataset: &Dataset, embedder: &Embedder, scale: usize) -> Seeded {
    let http = client();
    let started = Instant::now();
    let mut doc_of = HashMap::new();
    let mut drawer_of: HashMap<String, String> = HashMap::new();
    let mut written = 0usize;

    for document in &dataset.documents {
        let id = create_new(
            &http,
            base,
            &document.wing,
            &document.room,
            &document.content,
        )
        .await;
        attach(&http, base, &id, &embedder.embed(&document.content)).await;
        doc_of.insert(id.clone(), document.id.clone());
        drawer_of.insert(document.id.clone(), id);
        written += 1;
    }
    for mention in &dataset.mentions {
        let id = &drawer_of[&mention.doc];
        let response = http
            .post(format!("{base}/api/drawers/{id}/mentions"))
            .json(&json!({ "name": mention.entity, "kind": mention.kind }))
            .send()
            .await
            .expect("link entity");
        assert!(
            response.status().is_success(),
            "linking `{}`: {}",
            mention.doc,
            response.status()
        );
    }
    for (index, text) in filler(scale).into_iter().enumerate() {
        let id = create_new(&http, base, "filler", "bulk", &text).await;
        attach(&http, base, &id, &embedder.embed(&text)).await;
        doc_of.insert(id, format!("filler-{index}"));
        written += 1;
    }

    // Epoch 0 is everything above. The instant is taken after a pause so no write shares it.
    tokio::time::sleep(TICK).await;
    let mut epoch_end = vec![Utc::now()];
    for epoch in 1..=dataset.last_epoch() {
        tokio::time::sleep(TICK).await;
        for supersession in dataset.supersessions.iter().filter(|s| s.epoch == epoch) {
            let id = &drawer_of[&supersession.doc];
            let outcome: Value = http
                .post(format!("{base}/api/drawers/{id}/supersede"))
                .json(&json!({ "content": supersession.content }))
                .send()
                .await
                .expect("supersede")
                .json()
                .await
                .expect("supersede answer");
            let replacement = outcome["replacement"]["id"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "superseding `{}` made no replacement: {outcome}",
                        supersession.doc
                    )
                })
                .to_string();
            // A replacement is a new drawer, so it needs its own vector: the old one's belongs to the old text.
            attach(
                &http,
                base,
                &replacement,
                &embedder.embed(&supersession.content),
            )
            .await;
            doc_of.insert(replacement.clone(), supersession.new_id.clone());
            drawer_of.insert(supersession.new_id.clone(), replacement);
            written += 1;
        }
        tokio::time::sleep(TICK).await;
        epoch_end.push(Utc::now());
    }

    let seconds = started.elapsed().as_secs_f64();
    Seeded {
        doc_of,
        epoch_end,
        ingest: Ingest {
            documents: written,
            // The pauses between epochs are not ingest work, so they are taken out of the rate's denominator.
            documents_per_second: written as f64 / (seconds - pauses(dataset)).max(f64::EPSILON),
        },
    }
}

/// The time `seed` spends sleeping between epochs.
fn pauses(dataset: &Dataset) -> f64 {
    TICK.as_secs_f64() * (1.0 + 2.0 * dataset.last_epoch() as f64)
}

/// File `content` as a drawer and return its id and whether this call stored it (`false`: an exact copy was already there).
pub async fn create(
    http: &reqwest::Client,
    base: &str,
    wing: &str,
    room: &str,
    content: &str,
) -> (String, bool) {
    let response = http
        .post(format!("{base}/api/wings/{wing}/rooms/{room}/drawers"))
        .json(&json!({ "content": content }))
        .send()
        .await
        .expect("create drawer");
    assert!(
        response.status().is_success(),
        "creating a drawer: {}",
        response.status()
    );
    let drawer: Value = response.json().await.expect("drawer json");
    (
        drawer["id"].as_str().expect("drawer id").to_string(),
        drawer["created"] == true,
    )
}

/// [`create`] for a corpus the harness authored, where a copy is a mistake in the dataset.
async fn create_new(
    http: &reqwest::Client,
    base: &str,
    wing: &str,
    room: &str,
    content: &str,
) -> String {
    let (id, created) = create(http, base, wing, room, content).await;
    // An exact copy is not stored again, which would silently shrink the corpus a metric is computed over.
    assert!(created, "`{content}` duplicates an earlier document");
    id
}

/// Attach a caller-computed vector to a drawer.
pub async fn attach(http: &reqwest::Client, base: &str, id: &str, vector: &[f32]) {
    let response = http
        .put(format!("{base}/api/drawers/{id}/embedding"))
        .json(&json!({ "embedding": vector }))
        .send()
        .await
        .expect("attach embedding");
    assert!(
        response.status().is_success(),
        "attaching a vector: {}",
        response.status()
    );
}

/// `count` filler texts from a fixed generator, in words no query uses.
///
/// The words are invented syllable pairs, so a filler never matches a query's terms and the judgments stay true at any
/// scale. The generator is a xorshift with a fixed seed, so the same `count` always gives the same corpus.
pub fn filler(count: usize) -> Vec<String> {
    const SYLLABLES: [&str; 16] = [
        "zor", "vek", "mil", "tarq", "bun", "oxe", "lyp", "dru", "fen", "gaz", "hov", "jin", "kwo",
        "nox", "pez", "qua",
    ];
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..count)
        .map(|index| {
            let words: Vec<String> = (0..12)
                .map(|_| {
                    let a = SYLLABLES[(next() % 16) as usize];
                    let b = SYLLABLES[(next() % 16) as usize];
                    format!("{a}{b}")
                })
                .collect();
            // The index makes every text unique, so deduplication never drops one.
            format!("{} filler{index}", words.join(" "))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filler_is_the_same_corpus_every_time_and_never_repeats_a_text() {
        let first = filler(50);
        assert_eq!(first, filler(50));
        let unique: std::collections::HashSet<_> = first.iter().collect();
        assert_eq!(unique.len(), 50);
    }

    #[test]
    fn filler_shares_no_word_with_the_core_dataset_queries() {
        let (dataset, _) = Dataset::load(&super::super::dataset::core_path());
        let corpus = filler(200).join(" ");
        for query in &dataset.queries {
            for word in query.text.split_whitespace() {
                let word = word.to_lowercase();
                assert!(
                    !corpus.split(' ').any(|w| w == word),
                    "filler contains the query word `{word}`"
                );
            }
        }
    }
}

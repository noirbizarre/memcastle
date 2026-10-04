//! Memory deduplication: deciding whether a drawer about to be written is one the room already holds.
//!
//! The policy (docs/adr/025) is deliberately conservative, and everything here is deterministic and explainable:
//!
//! * **Exact** — the same bytes as a current drawer in the same room. A write that names no drawer is not stored
//!   twice: the writer is handed the existing one.
//! * **Normalized** and **near** — the same words ignoring case and punctuation, or within the configured
//!   similarity (a typo, a one-character edit). Both drawers are kept; a `similar_to` edge records the likeness and
//!   its evidence, and nothing is merged. Whether two memories are really one is for a person or a later pass.
//! * Anything below that is a different memory that merely resembles this one, and is not recorded at all.
//!
//! The store proposes candidates (an indexed hash and fingerprint lookup, then BM25 over shared words); the verdict
//! is `domain`'s [`classify`]. No model is involved, so a daemon with no embedding provider behaves the same, and a
//! drawer's embedding (which arrives after the write) plays no part. This module reads and writes drawers and graph
//! edges only; it names no mining source and reads no file, so the mining pipeline can call it without learning
//! where its content came from.

use crate::config::DedupConfig;
use crate::domain::{
    Drawer, DrawerId, DuplicateKind, DuplicateSignals, classify, fingerprint, normalize_text,
    similarity_of_normalized,
};
use crate::error::Result;
use crate::store::SurrealStore;

/// How many BM25 candidates are compared by similarity. Enough that a typo variant is among them, few enough that
/// the comparison stays cheap on every write.
const LEXICAL_CANDIDATES: u32 = 10;
/// The most likely duplicates recorded for one drawer. A room full of near-identical drawers links to the closest
/// few, not to all of them.
const MAX_MATCHES: usize = 5;
/// Texts shorter than this, once normalised, are only ever *exact* or *normalized* duplicates. One character in a
/// short phrase is a different word (`cat` / `car`), not a typo.
pub const MIN_NEAR_CHARS: usize = 12;

/// One existing drawer a new one resembles, with the evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    /// The existing drawer.
    pub drawer: DrawerId,
    /// How it resembles the new one.
    pub kind: DuplicateKind,
    /// The numbers behind the verdict.
    pub signals: DuplicateSignals,
}

/// What [`assess`] found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Assessment {
    /// The drawers the new one resembles, strongest first. Empty means it is a distinct memory.
    pub matches: Vec<Match>,
}

impl Assessment {
    /// The existing drawer holding exactly the same content, if any.
    #[must_use]
    pub fn exact(&self) -> Option<&Match> {
        self.matches.iter().find(|m| m.kind == DuplicateKind::Exact)
    }
}

/// How a writer wants its drawer treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    /// Hand the writer the existing drawer instead of storing an exact copy.
    pub skip_exact: bool,
    /// Only compare against drawers by the same agent. For memory that is private to an identity: one agent's diary
    /// entry is not another's, even when the words are the same.
    pub per_agent: bool,
}

impl Rules {
    /// An agent or person stating a memory: an exact copy in the room is not stored again.
    pub const MEMORY: Self = Self {
        skip_exact: true,
        per_agent: false,
    };
    /// A diary entry: like [`Self::MEMORY`], within one agent's own entries.
    pub const DIARY: Self = Self {
        skip_exact: true,
        per_agent: true,
    };
    /// A mined chunk: two identical files are two records (docs/adr/023), so nothing is skipped, but likeness is
    /// still recorded.
    pub const MINED: Self = Self {
        skip_exact: false,
        per_agent: false,
    };
}

/// What [`write`] did with a drawer.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The drawer was stored (or, on a replay, was already). `similar` are the likely duplicates it was linked to.
    Stored {
        /// The drawers it resembles.
        similar: Vec<Match>,
    },
    /// The drawer was not stored: the room already holds exactly this content.
    Duplicate {
        /// The drawer that does.
        existing: DrawerId,
    },
}

/// Find the drawers in `drawer`'s room that it duplicates or resembles.
///
/// Reads only: nothing is written, so a caller may assess first and decide. `drawer` itself is never a match, which
/// is what makes a replayed write (the drawer already stored) safe to assess again.
///
/// # Errors
///
/// Returns an error if a store lookup fails.
pub async fn assess(
    store: &SurrealStore,
    drawer: &Drawer,
    config: &DedupConfig,
    per_agent: bool,
) -> Result<Assessment> {
    if !config.enabled {
        return Ok(Assessment::default());
    }
    let normal = normalize_text(&drawer.content);
    let print = fingerprint(&drawer.content);
    let print = (!print.is_empty()).then_some(print);

    let mut candidates = store
        .find_duplicate_candidates(
            drawer.room,
            &drawer.content_hash,
            print.as_deref(),
            drawer.id,
        )
        .await?;
    candidates.retain(|c| !per_agent || c.agent == drawer.source.agent);
    // An exact copy settles the question; looking for typos beside it would only add noise to the evidence.
    let has_exact = candidates
        .iter()
        .any(|c| c.content_hash == drawer.content_hash);
    if !has_exact && normal.chars().count() >= MIN_NEAR_CHARS {
        for candidate in store
            .find_lexical_candidates(drawer.room, &drawer.content, LEXICAL_CANDIDATES, drawer.id)
            .await?
        {
            if (!per_agent || candidate.agent == drawer.source.agent)
                && !candidates.iter().any(|c| c.id == candidate.id)
            {
                candidates.push(candidate);
            }
        }
    }

    let near_allowed = normal.chars().count() >= MIN_NEAR_CHARS;
    let mut matches: Vec<Match> = candidates
        .into_iter()
        .filter_map(|candidate| {
            let other = normalize_text(&candidate.content);
            let same_hash = candidate.content_hash == drawer.content_hash;
            // Equal normal forms are the fingerprint test without a second hash: a drawer with no letters or digits
            // has an empty normal form and must not "match" another one.
            let same_fingerprint = !normal.is_empty() && other == normal;
            let similarity = if same_hash {
                1.0
            } else {
                similarity_of_normalized(&normal, &other)
            };
            // The verdict sees a zero where a near match is not allowed, but the evidence keeps the real number.
            let judged = if near_allowed { similarity } else { 0.0 };
            let kind = classify(same_hash, same_fingerprint, judged, config.near_threshold)?;
            Some(Match {
                drawer: candidate.id,
                kind,
                signals: DuplicateSignals {
                    same_hash,
                    same_fingerprint,
                    similarity,
                    threshold: config.near_threshold,
                },
            })
        })
        .collect();
    matches.sort_by(|a, b| {
        rank(a.kind)
            .cmp(&rank(b.kind))
            .then_with(|| b.signals.similarity.total_cmp(&a.signals.similarity))
            .then_with(|| a.drawer.to_string().cmp(&b.drawer.to_string()))
    });
    matches.truncate(MAX_MATCHES);
    Ok(Assessment { matches })
}

/// Strongest verdict first.
fn rank(kind: DuplicateKind) -> u8 {
    match kind {
        DuplicateKind::Exact => 0,
        DuplicateKind::Normalized => 1,
        DuplicateKind::Near => 2,
    }
}

/// Record every likeness in `assessment` as a `similar_to` edge from `drawer` (the newer) to what it resembles.
///
/// Idempotent, so a replayed job writes no second edge.
///
/// # Errors
///
/// Returns an error if a store write fails.
pub async fn record(store: &SurrealStore, drawer: DrawerId, assessment: &Assessment) -> Result<()> {
    for found in &assessment.matches {
        store
            .link_similar_drawers(drawer, found.drawer, found.kind, &found.signals)
            .await?;
    }
    Ok(())
}

/// Record what an already-stored `drawer` resembles, returning how many drawers that is.
///
/// For the writer that stores drawers itself and only wants the likeness noted (the mining pipeline, which
/// supersedes and creates in its own transactions and never skips a chunk). Idempotent, and a no-op with
/// deduplication disabled.
///
/// # Errors
///
/// Returns an error if a store lookup or write fails.
pub async fn link(store: &SurrealStore, drawer: &Drawer, config: &DedupConfig) -> Result<usize> {
    let assessment = assess(store, drawer, config, Rules::MINED.per_agent).await?;
    record(store, drawer.id, &assessment).await?;
    Ok(assessment.matches.len())
}

/// Write `drawer`, unless the room already holds its exact content, and record what it resembles.
///
/// `rules.skip_exact` is for the writers whose drawer has no identity beyond its words (an agent's unnamed write):
/// they get the existing drawer instead of a copy. Mining skips nothing (two identical files are two records,
/// docs/adr/023) and neither does a named write (the name is the identity, docs/adr/025): both are stored and linked.
///
/// Replay-safe, like [`SurrealStore::create_drawer_once`]: a drawer whose id is already stored is not written
/// again, and is neither skipped nor re-linked wrongly, because the assessment never matches a drawer to itself.
///
/// # Errors
///
/// Returns an error if a store lookup or write fails.
pub async fn write(
    store: &SurrealStore,
    drawer: &Drawer,
    config: &DedupConfig,
    rules: Rules,
) -> Result<Outcome> {
    let replayed = store.drawer_exists(drawer.id).await?;
    let assessment = assess(store, drawer, config, rules.per_agent).await?;
    if rules.skip_exact
        && !replayed
        && let Some(found) = assessment.exact()
    {
        return Ok(Outcome::Duplicate {
            existing: found.drawer,
        });
    }
    if !replayed {
        store.create_drawer(drawer).await?;
    }
    record(store, drawer.id, &assessment).await?;
    Ok(Outcome::Stored {
        similar: assessment.matches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Provenance, RoomId, Source, SourceKind};

    fn drawer(room: RoomId, content: &str) -> Drawer {
        Drawer::new(
            DrawerId::new(),
            room,
            content.to_string(),
            Source::new(SourceKind::Manual, None, None),
            vec![],
            Provenance {
                requested_by: "test".to_string(),
                job_id: None,
            },
        )
    }

    const DECISION: &str = "We decided to use SurrealDB for storage because one engine covers documents, graph and vectors.";

    async fn store() -> SurrealStore {
        SurrealStore::connect_memory_for_tests().await
    }

    #[tokio::test]
    async fn an_identical_write_in_the_same_room_is_not_stored_twice() {
        let store = store().await;
        let config = DedupConfig::default();
        let room = RoomId::new();
        let first = drawer(room, DECISION);
        assert!(matches!(
            write(&store, &first, &config, Rules::MEMORY).await.unwrap(),
            Outcome::Stored { similar } if similar.is_empty()
        ));

        let again = drawer(room, DECISION);
        let outcome = write(&store, &again, &config, Rules::MEMORY).await.unwrap();

        assert_eq!(outcome, Outcome::Duplicate { existing: first.id });
        assert_eq!(store.list_drawers(Some(room)).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_same_text_in_another_room_is_a_different_memory() {
        let store = store().await;
        let config = DedupConfig::default();
        write(
            &store,
            &drawer(RoomId::new(), DECISION),
            &config,
            Rules::MEMORY,
        )
        .await
        .unwrap();

        let outcome = write(
            &store,
            &drawer(RoomId::new(), DECISION),
            &config,
            Rules::MEMORY,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, Outcome::Stored { similar } if similar.is_empty()));
    }

    #[tokio::test]
    async fn a_one_character_typo_is_stored_and_linked_as_a_near_duplicate() {
        let store = store().await;
        let config = DedupConfig::default();
        let room = RoomId::new();
        let original = drawer(room, DECISION);
        write(&store, &original, &config, Rules::MEMORY)
            .await
            .unwrap();

        let typo = drawer(room, &DECISION.replace("storage", "storge"));
        let outcome = write(&store, &typo, &config, Rules::MEMORY).await.unwrap();

        let Outcome::Stored { similar } = outcome else {
            panic!("a typo is not an exact duplicate, so both drawers are kept");
        };
        assert_eq!(similar.len(), 1);
        assert_eq!(similar[0].drawer, original.id);
        assert_eq!(similar[0].kind, DuplicateKind::Near);
        assert!(similar[0].signals.similarity >= 0.97 && !similar[0].signals.same_hash);
        assert_eq!(store.list_drawers(Some(room)).await.unwrap().len(), 2);
        let links = store.list_similar_drawers(typo.id).await.unwrap();
        assert_eq!(
            links.len(),
            1,
            "the likeness is recorded for later resolution"
        );
        assert_eq!(links[0].drawer, original.id);
    }

    #[tokio::test]
    async fn a_case_and_punctuation_variant_is_linked_as_normalized_not_skipped() {
        let store = store().await;
        let config = DedupConfig::default();
        let room = RoomId::new();
        let original = drawer(room, DECISION);
        write(&store, &original, &config, Rules::MEMORY)
            .await
            .unwrap();

        let variant = drawer(room, &DECISION.to_uppercase().replace(',', ""));
        let Outcome::Stored { similar } = write(&store, &variant, &config, Rules::MEMORY)
            .await
            .unwrap()
        else {
            panic!("only identical bytes are skipped");
        };

        assert_eq!(similar[0].kind, DuplicateKind::Normalized);
        assert!(similar[0].signals.same_fingerprint && !similar[0].signals.same_hash);
    }

    #[tokio::test]
    async fn a_similar_but_distinct_memory_is_neither_skipped_nor_linked() {
        let store = store().await;
        let config = DedupConfig::default();
        let room = RoomId::new();
        write(
            &store,
            &drawer(
                room,
                "We decided to use Postgres for the billing service because the team knows it.",
            ),
            &config,
            Rules::MEMORY,
        )
        .await
        .unwrap();

        let distinct = drawer(
            room,
            "We decided to use SurrealDB for the memory service because one engine covers more.",
        );
        let outcome = write(&store, &distinct, &config, Rules::MEMORY)
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Stored { similar } if similar.is_empty()));
        assert!(
            store
                .list_similar_drawers(distinct.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn short_texts_one_letter_apart_are_different_memories() {
        let store = store().await;
        let config = DedupConfig::default();
        let room = RoomId::new();
        write(&store, &drawer(room, "Use cat."), &config, Rules::MEMORY)
            .await
            .unwrap();

        let outcome = write(&store, &drawer(room, "Use car."), &config, Rules::MEMORY)
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Stored { similar } if similar.is_empty()));
    }

    #[tokio::test]
    async fn mining_style_writes_keep_an_exact_copy_and_link_it() {
        let store = store().await;
        let config = DedupConfig::default();
        let room = RoomId::new();
        let first = drawer(room, DECISION);
        write(&store, &first, &config, Rules::MINED).await.unwrap();

        let second = drawer(room, DECISION);
        let Outcome::Stored { similar } =
            write(&store, &second, &config, Rules::MINED).await.unwrap()
        else {
            panic!("a writer that does not skip must store the copy");
        };

        assert_eq!(similar[0].kind, DuplicateKind::Exact);
        assert_eq!(store.list_drawers(Some(room)).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn diary_entries_are_only_duplicates_of_the_same_agents_entries() {
        let store = store().await;
        let config = DedupConfig::default();
        let room = RoomId::new();
        let by = |agent: &str| {
            let mut entry = drawer(room, DECISION);
            entry.source.agent = Some(agent.to_string());
            entry
        };
        let first = by("alice");
        write(&store, &first, &config, Rules::DIARY).await.unwrap();

        let other_agent = write(&store, &by("bob"), &config, Rules::DIARY)
            .await
            .unwrap();
        let same_agent = write(&store, &by("alice"), &config, Rules::DIARY)
            .await
            .unwrap();

        assert!(
            matches!(other_agent, Outcome::Stored { similar } if similar.is_empty()),
            "bob must keep his own entry; identities never see each other's"
        );
        assert_eq!(same_agent, Outcome::Duplicate { existing: first.id });
    }

    #[tokio::test]
    async fn a_named_write_is_stored_even_beside_an_identical_unnamed_one_and_linked_to_it() {
        let store = store().await;
        let config = DedupConfig::default();
        let room = RoomId::new();
        let unnamed = drawer(room, DECISION);
        write(&store, &unnamed, &config, Rules::MINED)
            .await
            .unwrap();

        let named = drawer(room, DECISION).with_name(Some("decision".to_string()));
        let rules = Rules {
            skip_exact: false,
            ..Rules::MEMORY
        };
        let Outcome::Stored { similar } = write(&store, &named, &config, rules).await.unwrap()
        else {
            panic!("a name is an identity of its own");
        };

        assert_eq!(similar[0].kind, DuplicateKind::Exact);
    }

    #[tokio::test]
    async fn a_replayed_write_neither_duplicates_nor_links_a_drawer_to_itself() {
        let store = store().await;
        let config = DedupConfig::default();
        let room = RoomId::new();
        let item = drawer(room, DECISION);
        write(&store, &item, &config, Rules::MEMORY).await.unwrap();

        let outcome = write(&store, &item, &config, Rules::MEMORY).await.unwrap();

        assert!(matches!(outcome, Outcome::Stored { similar } if similar.is_empty()));
        assert_eq!(store.list_drawers(Some(room)).await.unwrap().len(), 1);
        assert!(
            store
                .list_similar_drawers(item.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_superseded_drawer_does_not_shadow_a_re_assertion() {
        let store = store().await;
        let config = DedupConfig::default();
        let room = RoomId::new();
        let old = drawer(room, DECISION);
        write(&store, &old, &config, Rules::MEMORY).await.unwrap();
        store
            .supersede_drawer(old.id, None, chrono::Utc::now())
            .await
            .unwrap();

        let outcome = write(&store, &drawer(room, DECISION), &config, Rules::MEMORY)
            .await
            .unwrap();

        assert!(
            matches!(outcome, Outcome::Stored { similar } if similar.is_empty()),
            "what is no longer true must not block saying it again"
        );
    }

    #[tokio::test]
    async fn deleting_a_drawer_leaves_no_dangling_link_on_the_one_it_resembled() {
        let store = store().await;
        let config = DedupConfig::default();
        let room = RoomId::new();
        let original = drawer(room, DECISION);
        write(&store, &original, &config, Rules::MEMORY)
            .await
            .unwrap();
        let copy = drawer(room, &DECISION.replace("storage", "storge"));
        write(&store, &copy, &config, Rules::MEMORY).await.unwrap();
        assert_eq!(
            store.list_similar_drawers(original.id).await.unwrap().len(),
            1
        );

        store.delete_drawer(copy.id).await.unwrap();

        assert!(
            store
                .list_similar_drawers(original.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn disabled_deduplication_stores_everything_and_links_nothing() {
        let store = store().await;
        let config = DedupConfig {
            enabled: false,
            ..DedupConfig::default()
        };
        let room = RoomId::new();
        write(&store, &drawer(room, DECISION), &config, Rules::MEMORY)
            .await
            .unwrap();

        let outcome = write(&store, &drawer(room, DECISION), &config, Rules::MEMORY)
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Stored { similar } if similar.is_empty()));
        assert_eq!(store.list_drawers(Some(room)).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_stricter_threshold_stops_linking_typos() {
        let store = store().await;
        let config = DedupConfig {
            near_threshold: 1.0,
            ..DedupConfig::default()
        };
        let room = RoomId::new();
        write(&store, &drawer(room, DECISION), &config, Rules::MEMORY)
            .await
            .unwrap();

        let outcome = write(
            &store,
            &drawer(room, &DECISION.replace("storage", "storge")),
            &config,
            Rules::MEMORY,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, Outcome::Stored { similar } if similar.is_empty()));
    }
}

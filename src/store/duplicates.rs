//! Duplicate-drawer repository methods: candidate lookup and the `similar_to` links.
//!
//! The store only *proposes*: it returns drawers that share a hash or fingerprint with some text, or that share its
//! words, and it records a verdict somebody else reached. Deciding whether two drawers are the same memory is
//! `crate::dedup`'s job, on `domain`'s signals (docs/adr/025) — a unique index could refuse a write but could not
//! tell an exact copy from a typo, and would refuse the legitimate copies (a superseded drawer, a mined chunk).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{DrawerId, DuplicateKind, DuplicateSignals, RoomId};
use crate::error::Result;

use super::SurrealStore;

/// The words of a query beyond which a lexical candidate search stops reading: a long drawer would otherwise turn
/// into a thousand-term `OR` for no better recall. A typo changes one word; the rest still match.
const MAX_QUERY_WORDS: usize = 48;

/// An existing drawer proposed as a possible duplicate, with just what comparing it needs.
#[derive(Debug, Clone, Deserialize)]
pub struct DrawerCandidate {
    /// Its id.
    pub id: DrawerId,
    /// Its verbatim content.
    pub content: String,
    /// The SHA-256 of that content.
    pub content_hash: String,
    /// Who wrote it (`source.agent`), for the writers whose memory is private to an identity (the diary).
    #[serde(default)]
    pub agent: Option<String>,
}

/// Which side of a `similar_to` edge the other drawer is on, from the queried drawer's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimilarSide {
    /// The queried drawer was written later and resembles this older one.
    Older,
    /// This drawer was written later and resembles the queried one.
    Newer,
}

/// One drawer linked to another as a likely duplicate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarDrawer {
    /// The other drawer.
    pub drawer: DrawerId,
    /// Whether it is older or newer than the queried one.
    pub side: SimilarSide,
    /// How it resembles it.
    pub kind: DuplicateKind,
    /// The similarity score in `[0, 1]`.
    pub similarity: f32,
    /// The evidence for the verdict.
    pub signals: DuplicateSignals,
    /// When the link was recorded.
    pub created_at: DateTime<Utc>,
}

/// The row shape of one `similar_to` edge, read from either end.
#[derive(Deserialize)]
struct EdgeRow {
    other: DrawerId,
    kind: DuplicateKind,
    similarity: f32,
    signals: DuplicateSignals,
    created_at: DateTime<Utc>,
}

impl SurrealStore {
    /// The drawers currently valid in `room` whose content hash is `hash` or whose fingerprint is `fingerprint`
    /// (skipped when `None`), excluding `except`.
    ///
    /// Both are indexed equality lookups. Superseded drawers are left out (`!valid_to`): a correction that closed
    /// `A` must not make a later re-assertion of `A` look like a duplicate of something that is no longer true.
    pub async fn find_duplicate_candidates(
        &self,
        room: RoomId,
        hash: &str,
        fingerprint: Option<&str>,
        except: DrawerId,
    ) -> Result<Vec<DrawerCandidate>> {
        // Two statements rather than one `OR`, so each can use its own index.
        let mut response = self
            .db
            .query(
                "SELECT record::id(id) AS id, content, content_hash, source.agent AS agent FROM drawer \
                   WHERE room = $room AND content_hash = $hash AND !valid_to \
                     AND id != type::record('drawer', $except); \
                 SELECT record::id(id) AS id, content, content_hash, source.agent AS agent FROM drawer \
                   WHERE $fingerprint != NONE AND room = $room AND fingerprint = $fingerprint \
                     AND !valid_to AND id != type::record('drawer', $except);",
            )
            .bind(("room", room.to_string()))
            .bind(("hash", hash.to_string()))
            .bind(("fingerprint", fingerprint.map(str::to_string)))
            .bind(("except", except.to_string()))
            .await?;
        let mut found: Vec<DrawerCandidate> = super::take_rows(&mut response, 0)?;
        let normalized: Vec<DrawerCandidate> = super::take_rows(&mut response, 1)?;
        for candidate in normalized {
            if !found.iter().any(|c| c.id == candidate.id) {
                found.push(candidate);
            }
        }
        Ok(found)
    }

    /// The drawers currently valid in `room` that share words with `text`, best BM25 match first, at most
    /// `limit`, excluding `except`: the candidates for a typo-level duplicate.
    ///
    /// Any one shared word qualifies (a typo breaks one word, not all of them); ranking and the cut-off keep the
    /// list short and the caller's similarity check decides what is actually a duplicate. Drawers do not need an
    /// embedding for this, which is the point: embeddings arrive after the write.
    pub async fn find_lexical_candidates(
        &self,
        room: RoomId,
        text: &str,
        limit: u32,
        except: DrawerId,
    ) -> Result<Vec<DrawerCandidate>> {
        let words = distinct_words(text);
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let mut response = self
            .db
            .query(
                "SELECT record::id(id) AS id, content, content_hash, source.agent AS agent, search::score(1) AS rank FROM drawer \
                   WHERE content @1,OR@ $query AND room = $room AND !valid_to \
                     AND id != type::record('drawer', $except) \
                   ORDER BY rank DESC, id ASC LIMIT $limit",
            )
            .bind(("query", words.join(" ")))
            .bind(("room", room.to_string()))
            .bind(("except", except.to_string()))
            .bind(("limit", limit))
            .await?;
        super::take_rows(&mut response, 0)
    }

    /// Record that `newer` is a likely duplicate of `older`. Idempotent: the unique `(in, out)` index means linking
    /// twice leaves one edge, so a replayed job writes no second link. Returns whether this call created it.
    ///
    /// The edge is the whole record of the verdict: nothing about either drawer changes, and deleting the edge
    /// undoes it.
    pub async fn link_similar_drawers(
        &self,
        newer: DrawerId,
        older: DrawerId,
        kind: DuplicateKind,
        signals: &DuplicateSignals,
    ) -> Result<bool> {
        let mut response = self
            .db
            .query(
                "LET $existing = (SELECT VALUE id FROM similar_to \
                    WHERE in = type::record('drawer', $newer) AND out = type::record('drawer', $older)); \
                 IF array::len($existing) = 0 { \
                    RELATE (type::record('drawer', $newer))->similar_to->(type::record('drawer', $older)) \
                      SET kind = $kind, similarity = $similarity, signals = $signals, \
                          created_at = <datetime>$now; \
                    RETURN true; \
                 } ELSE { RETURN false; };",
            )
            .bind(("newer", newer.to_string()))
            .bind(("older", older.to_string()))
            .bind(("kind", kind.as_str()))
            .bind(("similarity", signals.similarity))
            .bind(("signals", super::bindable(signals)?))
            .bind(("now", super::stored(Utc::now())))
            .await?
            .check()?;
        let created: Option<bool> = response.take(response.num_statements() - 1)?;
        Ok(created.unwrap_or(false))
    }

    /// The drawers linked to `drawer` as likely duplicates, in both directions, most similar first.
    pub async fn list_similar_drawers(&self, drawer: DrawerId) -> Result<Vec<SimilarDrawer>> {
        let mut response = self
            .db
            .query(
                "SELECT record::id(out) AS other, kind, similarity, signals, <string>created_at AS created_at \
                   FROM similar_to WHERE in = type::record('drawer', $drawer); \
                 SELECT record::id(in) AS other, kind, similarity, signals, <string>created_at AS created_at \
                   FROM similar_to WHERE out = type::record('drawer', $drawer);",
            )
            .bind(("drawer", drawer.to_string()))
            .await?;
        let older: Vec<EdgeRow> = super::take_rows(&mut response, 0)?;
        let newer: Vec<EdgeRow> = super::take_rows(&mut response, 1)?;
        let mut similar: Vec<SimilarDrawer> = older
            .into_iter()
            .map(|row| (SimilarSide::Older, row))
            .chain(newer.into_iter().map(|row| (SimilarSide::Newer, row)))
            .map(|(side, row)| SimilarDrawer {
                drawer: row.other,
                side,
                kind: row.kind,
                similarity: row.similarity,
                signals: row.signals,
                created_at: row.created_at,
            })
            .collect();
        similar.sort_by(|a, b| {
            b.similarity
                .total_cmp(&a.similarity)
                .then_with(|| a.drawer.to_string().cmp(&b.drawer.to_string()))
        });
        Ok(similar)
    }

    /// Fill `fingerprint` on drawers stored before it existed. Returns how many were filled.
    ///
    /// Only for `crate::migrate`. Idempotent: a drawer that has one no longer matches. Drawers with no letters or
    /// digits have no fingerprint and are left `NONE`, so every run revisits them; that is cheap, and they are rare.
    pub(crate) async fn backfill_drawer_fingerprints(&self) -> Result<u64> {
        #[derive(Deserialize)]
        struct Row {
            id: String,
            content: String,
        }
        let mut response = self
            .db
            .query("SELECT record::id(id) AS id, content FROM drawer WHERE !fingerprint")
            .await?;
        let rows: Vec<Row> = super::take_rows(&mut response, 0)?;
        let mut filled = 0;
        for row in rows {
            let fingerprint = crate::domain::fingerprint(&row.content);
            if fingerprint.is_empty() {
                continue;
            }
            super::retrying_on_conflict(|| async {
                super::checked(
                    self.db
                        .query("UPDATE type::record('drawer', $id) SET fingerprint = $fingerprint")
                        .bind(("id", row.id.clone()))
                        .bind(("fingerprint", fingerprint.clone()))
                        .await?,
                )
                .map(|_| ())
            })
            .await?;
            filled += 1;
        }
        Ok(filled)
    }
}

/// The distinct words of `text` (as the normaliser sees them), at most [`MAX_QUERY_WORDS`], longest first.
///
/// Longest first because a long word is a rarer, more telling match than `the`; a one-character typo then costs
/// one word of many instead of the whole query.
fn distinct_words(text: &str) -> Vec<String> {
    let normal = crate::domain::normalize_text(text);
    let mut words: Vec<String> = Vec::new();
    for word in normal.split(' ').filter(|w| w.chars().count() > 2) {
        if !words.iter().any(|w| w == word) {
            words.push(word.to_string());
        }
    }
    words.sort_by(|a, b| {
        b.chars()
            .count()
            .cmp(&a.chars().count())
            .then_with(|| a.cmp(b))
    });
    words.truncate(MAX_QUERY_WORDS);
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Drawer, Provenance, Source, SourceKind};

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

    fn signals(similarity: f32) -> DuplicateSignals {
        DuplicateSignals {
            same_hash: false,
            same_fingerprint: false,
            similarity,
            threshold: 0.9,
        }
    }

    #[test]
    fn query_words_are_distinct_longest_first_and_capped() {
        let words = distinct_words("The cat and the Cat sat on a mat, magnificently");
        assert_eq!(words[0], "magnificently");
        assert_eq!(words.iter().filter(|w| *w == "cat").count(), 1);
        assert!(
            !words.iter().any(|w| w == "on"),
            "short words carry no signal"
        );
        let many = (0..200)
            .map(|n| format!("word{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(distinct_words(&many).len(), MAX_QUERY_WORDS);
    }

    #[tokio::test]
    async fn a_hash_or_fingerprint_match_in_the_same_room_is_a_candidate() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let room = RoomId::new();
        let original = drawer(room, "We chose SurrealDB for storage.");
        store.create_drawer(&original).await.unwrap();

        let probe = drawer(room, "we chose surrealdb for storage");
        let found = store
            .find_duplicate_candidates(
                room,
                &probe.content_hash,
                Some(&crate::domain::fingerprint(&probe.content)),
                probe.id,
            )
            .await
            .unwrap();
        assert_eq!(
            found.len(),
            1,
            "case and punctuation differ, the words do not"
        );
        assert_eq!(found[0].id, original.id);
    }

    #[tokio::test]
    async fn another_room_and_superseded_drawers_are_never_candidates() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let room = RoomId::new();
        let elsewhere = RoomId::new();
        let text = "We chose SurrealDB for storage.";
        let other_room = drawer(elsewhere, text);
        let closed = drawer(room, text);
        store.create_drawer(&other_room).await.unwrap();
        store.create_drawer(&closed).await.unwrap();
        store
            .supersede_drawer(closed.id, None, Utc::now())
            .await
            .unwrap();

        let probe = drawer(room, text);
        let found = store
            .find_duplicate_candidates(room, &probe.content_hash, None, probe.id)
            .await
            .unwrap();
        assert!(found.is_empty(), "found {found:?}");
    }

    #[tokio::test]
    async fn a_drawer_is_never_its_own_candidate() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let room = RoomId::new();
        let only = drawer(room, "We chose SurrealDB for storage.");
        store.create_drawer(&only).await.unwrap();
        let found = store
            .find_duplicate_candidates(room, &only.content_hash, None, only.id)
            .await
            .unwrap();
        assert!(found.is_empty());
        let lexical = store
            .find_lexical_candidates(room, &only.content, 5, only.id)
            .await
            .unwrap();
        assert!(lexical.is_empty());
    }

    #[tokio::test]
    async fn a_typo_variant_is_found_by_its_shared_words() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let room = RoomId::new();
        let original = drawer(
            room,
            "We decided to use SurrealDB for storage in this project.",
        );
        let unrelated = drawer(room, "Nightly backups rotate at midnight.");
        store.create_drawer(&original).await.unwrap();
        store.create_drawer(&unrelated).await.unwrap();

        let found = store
            .find_lexical_candidates(
                room,
                "We decidde to use SurrealDB for storage in this project.",
                5,
                DrawerId::new(),
            )
            .await
            .unwrap();
        assert_eq!(found.first().map(|c| c.id), Some(original.id));
    }

    #[tokio::test]
    async fn linking_twice_leaves_one_edge_readable_from_both_ends() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let room = RoomId::new();
        let older = drawer(room, "alpha beta gamma");
        let newer = drawer(room, "alpha beta gamma!");
        store.create_drawer(&older).await.unwrap();
        store.create_drawer(&newer).await.unwrap();

        let evidence = signals(0.95);
        assert!(
            store
                .link_similar_drawers(newer.id, older.id, DuplicateKind::Near, &evidence)
                .await
                .unwrap()
        );
        assert!(
            !store
                .link_similar_drawers(newer.id, older.id, DuplicateKind::Near, &evidence)
                .await
                .unwrap(),
            "a replay must not add a second link"
        );

        let from_newer = store.list_similar_drawers(newer.id).await.unwrap();
        assert_eq!(from_newer.len(), 1);
        assert_eq!(from_newer[0].drawer, older.id);
        assert_eq!(from_newer[0].side, SimilarSide::Older);
        assert_eq!(from_newer[0].kind, DuplicateKind::Near);
        let from_older = store.list_similar_drawers(older.id).await.unwrap();
        assert_eq!(from_older[0].drawer, newer.id);
        assert_eq!(from_older[0].side, SimilarSide::Newer);
        assert!((from_older[0].signals.threshold - 0.9).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn the_backfill_fills_missing_fingerprints_once() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let room = RoomId::new();
        let legacy = drawer(room, "Legacy memory, written before fingerprints.");
        store.create_drawer(&legacy).await.unwrap();
        store
            .execute_for_tests(&format!(
                "UPDATE drawer SET fingerprint = NONE WHERE id = type::record('drawer', '{}')",
                legacy.id
            ))
            .await;

        assert_eq!(store.backfill_drawer_fingerprints().await.unwrap(), 1);
        assert_eq!(store.backfill_drawer_fingerprints().await.unwrap(), 0);
        let probe = drawer(room, "legacy memory written before fingerprints");
        let found = store
            .find_duplicate_candidates(
                room,
                &probe.content_hash,
                Some(&crate::domain::fingerprint(&probe.content)),
                probe.id,
            )
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
    }
}

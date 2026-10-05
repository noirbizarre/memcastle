//! The temporal query model, end to end through the store (docs/adr/032).
//!
//! One timeline, asked about every way the model allows, and each question put to all three retrieval legs
//! (lexical, semantic and hybrid) so they cannot disagree about what was true. Instants are fixed rather than
//! relative to now, so a boundary test lands on the boundary to the nanosecond.

use chrono::{DateTime, Duration, Utc};

use crate::domain::{
    Drawer, DrawerId, EMBEDDING_DIMENSION, Provenance, RoomId, SearchFilter, SearchHit, Source,
    SourceKind, Temporal,
};

use super::{MatchMode, SurrealStore};

fn at(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .expect("a fixture instant")
        .with_timezone(&Utc)
}

/// Every drawer shares one vector, so the semantic leg ranks nothing apart: what it returns is decided by the
/// temporal scope alone.
fn vector() -> Vec<f32> {
    let mut v = vec![0.0; EMBEDDING_DIMENSION];
    v[0] = 1.0;
    v
}

async fn room(store: &SurrealStore) -> RoomId {
    let wing = store.get_or_create_wing("w", None).await.expect("wing");
    store
        .get_or_create_room(wing.id, "r", None)
        .await
        .expect("room")
        .id
}

/// A drawer valid from `valid_from` until `valid_to` (open-ended when `None`), not yet written.
///
/// Kept apart from [`add`] because a drawer that is a supersession's replacement must be written by
/// `supersede_drawer` once, not created and then re-created: deleting and re-creating one id loses a write
/// conflict against index maintenance on some platforms (seen on Windows CI).
fn draft(
    room: RoomId,
    content: &str,
    valid_from: DateTime<Utc>,
    valid_to: Option<DateTime<Utc>>,
) -> Drawer {
    let mut drawer = Drawer::new(
        DrawerId::new(),
        room,
        content.to_string(),
        Source {
            kind: SourceKind::Manual,
            uri: None,
            agent: None,
            origin: None,
        },
        vec![],
        Provenance {
            requested_by: "test".into(),
            job_id: None,
        },
    );
    drawer.valid_from = valid_from;
    drawer.valid_to = valid_to;
    drawer.embedding = Some(vector());
    drawer
}

async fn add(
    store: &SurrealStore,
    room: RoomId,
    content: &str,
    valid_from: DateTime<Utc>,
    valid_to: Option<DateTime<Utc>>,
) -> Drawer {
    let drawer = draft(room, content, valid_from, valid_to);
    store.create_drawer(&drawer).await.expect("create drawer");
    drawer
}

/// Five facts about "the database", laid out so each kind of validity appears once:
///
/// ```text
///            2024-01  2024-02      2025-01  2025-03-01  2025-03-10  2025-06       2099
/// sqlite      [========)                                                  expired
/// postgres                          [===========================================)  superseded
/// bounded                                       [=======)                          short-lived
/// surrealdb                                                               [======================  open-ended
/// cockroach                                                                              [======  future
/// ```
///
/// Every drawer is *recorded* now, years after the period it describes: record time and validity time are
/// different things, and only validity is searched.
async fn timeline(store: &SurrealStore) {
    let r = room(store).await;
    add(
        store,
        r,
        "database sqlite",
        at("2024-01-01T00:00:00Z"),
        Some(at("2024-02-01T00:00:00Z")),
    )
    .await;
    add(
        store,
        r,
        "database postgres",
        at("2025-01-01T00:00:00Z"),
        Some(at("2025-06-01T00:00:00Z")),
    )
    .await;
    add(
        store,
        r,
        "database bounded",
        at("2025-03-01T00:00:00Z"),
        Some(at("2025-03-10T00:00:00Z")),
    )
    .await;
    add(
        store,
        r,
        "database surrealdb",
        at("2025-06-01T00:00:00Z"),
        None,
    )
    .await;
    add(
        store,
        r,
        "database cockroach",
        at("2099-01-01T00:00:00Z"),
        None,
    )
    .await;
}

fn names(hits: &[SearchHit]) -> Vec<String> {
    let mut names: Vec<String> = hits
        .iter()
        .map(|hit| {
            hit.drawer
                .content
                .trim_start_matches("database ")
                .to_string()
        })
        .collect();
    names.sort();
    names
}

fn filter(temporal: Temporal) -> SearchFilter {
    SearchFilter {
        temporal,
        ..SearchFilter::default()
    }
}

/// Ask all three legs the same question and require the same answer from each, which is the consistency the
/// model promises; returns that answer.
async fn ask(store: &SurrealStore, temporal: Temporal) -> Vec<String> {
    let filter = filter(temporal);
    let lexical = names(
        &store
            .search_lexical("database", 10, &filter, MatchMode::All)
            .await
            .expect("lexical"),
    );
    let semantic = names(
        &store
            .search_vector(&vector(), 10, &filter)
            .await
            .expect("semantic"),
    );
    let hybrid = names(
        &store
            .search_hybrid("database", &vector(), 10, &filter)
            .await
            .expect("hybrid"),
    );
    assert_eq!(
        lexical, semantic,
        "lexical and semantic disagree for {temporal:?}"
    );
    assert_eq!(
        lexical, hybrid,
        "lexical and hybrid disagree for {temporal:?}"
    );
    lexical
}

async fn seeded() -> SurrealStore {
    let store = SurrealStore::connect_memory_for_tests().await;
    timeline(&store).await;
    store
}

fn between(from: &str, until: &str) -> Temporal {
    Temporal::Between {
        from: at(from),
        until: at(until),
    }
}

#[tokio::test]
async fn the_current_view_holds_what_is_open_now_and_neither_the_expired_nor_the_future() {
    let store = seeded().await;
    assert_eq!(ask(&store, Temporal::Current).await, ["surrealdb"]);
}

#[tokio::test]
async fn all_time_returns_every_version_whatever_its_validity() {
    let store = seeded().await;
    assert_eq!(
        ask(&store, Temporal::All).await,
        ["bounded", "cockroach", "postgres", "sqlite", "surrealdb"]
    );
}

#[tokio::test]
async fn an_as_of_query_sees_what_was_true_on_that_date_not_what_was_stored_last() {
    let store = seeded().await;
    assert_eq!(
        ask(&store, Temporal::AsOf(at("2024-01-15T00:00:00Z"))).await,
        ["sqlite"]
    );
    assert_eq!(
        ask(&store, Temporal::AsOf(at("2025-02-01T00:00:00Z"))).await,
        ["postgres"]
    );
    assert_eq!(
        ask(&store, Temporal::AsOf(at("2025-03-05T00:00:00Z"))).await,
        ["bounded", "postgres"],
        "two facts can be true together"
    );
    assert_eq!(
        ask(&store, Temporal::AsOf(at("2026-01-01T00:00:00Z"))).await,
        ["surrealdb"]
    );
}

#[tokio::test]
async fn a_gap_in_the_timeline_has_no_answer() {
    let store = seeded().await;
    assert!(
        ask(&store, Temporal::AsOf(at("2024-06-01T00:00:00Z")))
            .await
            .is_empty()
    );
    assert!(
        ask(&store, Temporal::AsOf(at("2023-01-01T00:00:00Z")))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn validity_starts_inclusive_at_valid_from() {
    let store = seeded().await;
    assert_eq!(
        ask(&store, Temporal::AsOf(at("2025-01-01T00:00:00Z"))).await,
        ["postgres"]
    );
    let just_before = at("2025-01-01T00:00:00Z") - Duration::nanoseconds(1);
    assert!(ask(&store, Temporal::AsOf(just_before)).await.is_empty());
}

#[tokio::test]
async fn validity_ends_exclusive_at_valid_to_so_a_supersession_has_no_overlap_and_no_gap() {
    let store = seeded().await;
    let handover = at("2025-06-01T00:00:00Z");
    // The nanosecond before: the old fact is still true, the new one has not begun.
    assert_eq!(
        ask(&store, Temporal::AsOf(handover - Duration::nanoseconds(1))).await,
        ["postgres"]
    );
    // At the handover: exactly the new one.
    assert_eq!(ask(&store, Temporal::AsOf(handover)).await, ["surrealdb"]);
}

#[tokio::test]
async fn a_future_drawer_becomes_visible_exactly_when_it_starts() {
    let store = seeded().await;
    let start = at("2099-01-01T00:00:00Z");
    assert_eq!(
        ask(&store, Temporal::AsOf(start - Duration::nanoseconds(1))).await,
        ["surrealdb"]
    );
    assert_eq!(
        ask(&store, Temporal::AsOf(start)).await,
        ["cockroach", "surrealdb"]
    );
}

#[tokio::test]
async fn an_open_ended_drawer_stays_valid_indefinitely() {
    let store = seeded().await;
    assert_eq!(
        ask(&store, Temporal::AsOf(at("2500-01-01T00:00:00Z"))).await,
        ["cockroach", "surrealdb"]
    );
}

#[tokio::test]
async fn an_interval_returns_every_version_whose_validity_overlaps_it() {
    let store = seeded().await;
    // Across the handover: the old and the new fact were both true at some moment of the window.
    assert_eq!(
        ask(
            &store,
            between("2025-05-01T00:00:00Z", "2025-07-01T00:00:00Z")
        )
        .await,
        ["postgres", "surrealdb"]
    );
    // Wholly inside one fact's validity, which contains it.
    assert_eq!(
        ask(
            &store,
            between("2025-03-02T00:00:00Z", "2025-03-05T00:00:00Z")
        )
        .await,
        ["bounded", "postgres"]
    );
    // Wholly containing a fact's validity.
    assert_eq!(
        ask(
            &store,
            between("2023-01-01T00:00:00Z", "2024-12-01T00:00:00Z")
        )
        .await,
        ["sqlite"]
    );
    // Wide enough for everything that was ever true, but not the future.
    assert_eq!(
        ask(
            &store,
            between("2000-01-01T00:00:00Z", "2030-01-01T00:00:00Z")
        )
        .await,
        ["bounded", "postgres", "sqlite", "surrealdb"]
    );
}

#[tokio::test]
async fn an_interval_that_only_touches_a_validity_boundary_does_not_overlap() {
    let store = seeded().await;
    // Starts exactly where sqlite ended (`valid_to` is exclusive), and ends exactly where postgres began
    // (the window's end is exclusive too).
    assert!(
        ask(
            &store,
            between("2024-02-01T00:00:00Z", "2025-01-01T00:00:00Z")
        )
        .await
        .is_empty()
    );
    // One nanosecond more on either side and they do overlap.
    let from = at("2024-02-01T00:00:00Z") - Duration::nanoseconds(1);
    assert_eq!(
        ask(
            &store,
            Temporal::Between {
                from,
                until: at("2025-01-01T00:00:00Z")
            }
        )
        .await,
        ["sqlite"]
    );
    let until = at("2025-01-01T00:00:00Z") + Duration::nanoseconds(1);
    assert_eq!(
        ask(
            &store,
            Temporal::Between {
                from: at("2024-02-01T00:00:00Z"),
                until
            }
        )
        .await,
        ["postgres"]
    );
    // Starting at the handover leaves the superseded fact out and keeps its replacement.
    assert_eq!(
        ask(
            &store,
            between("2025-06-01T00:00:00Z", "2025-07-01T00:00:00Z")
        )
        .await,
        ["surrealdb"]
    );
}

#[tokio::test]
async fn an_interval_a_nanosecond_wide_is_the_same_question_as_an_as_of() {
    let store = seeded().await;
    for instant in [
        "2024-01-15T00:00:00Z",
        "2025-03-05T00:00:00Z",
        "2025-06-01T00:00:00Z",
        "2099-01-01T00:00:00Z",
    ] {
        let point = at(instant);
        let window = Temporal::Between {
            from: point,
            until: point + Duration::nanoseconds(1),
        };
        assert_eq!(
            ask(&store, window).await,
            ask(&store, Temporal::AsOf(point)).await,
            "at {instant}"
        );
    }
}

#[tokio::test]
async fn a_memory_recorded_long_after_its_validity_is_found_by_when_it_was_true_not_when_it_was_stored()
 {
    let store = seeded().await;
    let stored = store
        .search_lexical("sqlite", 10, &filter(Temporal::All), MatchMode::All)
        .await
        .expect("search");
    let sqlite = &stored[0].drawer;
    assert!(
        sqlite.created_at > sqlite.valid_to.expect("it was closed"),
        "the fixture records the memory after the period it describes"
    );

    // Found by the period it describes, even though it was stored years later...
    assert_eq!(
        ask(&store, Temporal::AsOf(at("2024-01-15T00:00:00Z"))).await,
        ["sqlite"]
    );
    // ...and not by the time it was stored: asked about now, it is not true.
    assert!(
        !ask(&store, Temporal::Current)
            .await
            .contains(&"sqlite".to_string())
    );
}

#[tokio::test]
async fn superseding_a_drawer_closes_the_old_version_and_opens_the_new_one_at_the_same_instant() {
    let store = SurrealStore::connect_memory_for_tests().await;
    let r = room(&store).await;
    let old = add(
        &store,
        r,
        "database postgres",
        at("2025-01-01T00:00:00Z"),
        None,
    )
    .await;
    let handover = at("2025-06-01T00:00:00Z");
    let new = draft(r, "database surrealdb", handover, None);

    assert!(
        store
            .supersede_drawer(old.id, Some(&new), handover)
            .await
            .expect("supersede")
    );

    assert_eq!(
        ask(&store, Temporal::AsOf(handover - Duration::nanoseconds(1))).await,
        ["postgres"]
    );
    assert_eq!(ask(&store, Temporal::AsOf(handover)).await, ["surrealdb"]);
    assert_eq!(ask(&store, Temporal::All).await, ["postgres", "surrealdb"]);
}

#[tokio::test]
async fn superseding_links_the_two_drawers_in_both_directions() {
    let store = SurrealStore::connect_memory_for_tests().await;
    let r = room(&store).await;
    let old = add(&store, r, "first", at("2025-01-01T00:00:00Z"), None).await;
    let new = draft(r, "second", at("2025-06-01T00:00:00Z"), None);
    store
        .supersede_drawer(old.id, Some(&new), at("2025-06-01T00:00:00Z"))
        .await
        .expect("supersede");

    let old = store.get_drawer(old.id).await.unwrap().unwrap();
    let new = store.get_drawer(new.id).await.unwrap().unwrap();
    assert_eq!(old.superseded_by, Some(new.id));
    assert_eq!(new.supersedes, Some(old.id));
    assert_eq!(old.supersedes, None);
    assert_eq!(new.superseded_by, None);
}

/// A chain of three versions of one fact, plus an unrelated drawer, written through `supersede_drawer`.
async fn chain(store: &SurrealStore) -> (Vec<Drawer>, Drawer) {
    let r = room(store).await;
    let first = add(
        store,
        r,
        "database sqlite",
        at("2024-01-01T00:00:00Z"),
        None,
    )
    .await;
    let second = draft(r, "database postgres", at("2025-01-01T00:00:00Z"), None);
    let third = draft(r, "database surrealdb", at("2025-06-01T00:00:00Z"), None);
    let unrelated = add(
        store,
        r,
        "database elsewhere",
        at("2025-01-01T00:00:00Z"),
        None,
    )
    .await;
    store
        .supersede_drawer(first.id, Some(&second), at("2025-01-01T00:00:00Z"))
        .await
        .expect("first handover");
    store
        .supersede_drawer(second.id, Some(&third), at("2025-06-01T00:00:00Z"))
        .await
        .expect("second handover");
    (vec![first, second, third], unrelated)
}

#[tokio::test]
async fn history_returns_every_version_oldest_first_from_any_member_of_the_chain() {
    let store = SurrealStore::connect_memory_for_tests().await;
    let (versions, _) = chain(&store).await;
    let expected: Vec<DrawerId> = versions.iter().map(|d| d.id).collect();

    for start in &versions {
        let found = store
            .drawer_lineage(start.id)
            .await
            .expect("lineage")
            .expect("the drawer exists");
        let ids: Vec<DrawerId> = found.iter().map(|d| d.id).collect();
        assert_eq!(ids, expected, "starting from {}", start.content);
    }
}

#[tokio::test]
async fn history_preserves_each_versions_validity_provenance_and_content() {
    let store = SurrealStore::connect_memory_for_tests().await;
    let (versions, _) = chain(&store).await;

    let found = store.drawer_lineage(versions[2].id).await.unwrap().unwrap();

    let contents: Vec<&str> = found.iter().map(|d| d.content.as_str()).collect();
    assert_eq!(
        contents,
        ["database sqlite", "database postgres", "database surrealdb"]
    );
    assert_eq!(found[0].valid_to, Some(at("2025-01-01T00:00:00Z")));
    assert_eq!(found[1].valid_from, at("2025-01-01T00:00:00Z"));
    assert_eq!(found[1].valid_to, Some(at("2025-06-01T00:00:00Z")));
    assert_eq!(found[2].valid_to, None, "the last version is still true");
    assert!(found.iter().all(|d| d.provenance.requested_by == "test"));
    assert!(
        found.iter().all(|d| d.embedding.is_none()),
        "history never carries vectors"
    );
}

#[tokio::test]
async fn history_leaves_out_drawers_that_are_not_part_of_the_chain() {
    let store = SurrealStore::connect_memory_for_tests().await;
    let (versions, unrelated) = chain(&store).await;

    let found = store.drawer_lineage(versions[0].id).await.unwrap().unwrap();
    assert!(found.iter().all(|d| d.id != unrelated.id));

    let alone = store.drawer_lineage(unrelated.id).await.unwrap().unwrap();
    assert_eq!(
        alone.len(),
        1,
        "a drawer never superseded is its own one-version history"
    );
}

#[tokio::test]
async fn history_of_a_drawer_closed_without_a_replacement_ends_at_that_drawer() {
    let store = SurrealStore::connect_memory_for_tests().await;
    let r = room(&store).await;
    let old = add(
        &store,
        r,
        "database sqlite",
        at("2024-01-01T00:00:00Z"),
        None,
    )
    .await;
    store
        .supersede_drawer(old.id, None, at("2024-02-01T00:00:00Z"))
        .await
        .unwrap();

    let found = store.drawer_lineage(old.id).await.unwrap().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].valid_to, Some(at("2024-02-01T00:00:00Z")));
}

#[tokio::test]
async fn history_of_a_drawer_that_does_not_exist_is_none() {
    let store = SurrealStore::connect_memory_for_tests().await;
    assert!(
        store
            .drawer_lineage(DrawerId::new())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_corrupt_cycle_in_the_links_ends_the_walk_instead_of_looping() {
    let store = SurrealStore::connect_memory_for_tests().await;
    let (versions, _) = chain(&store).await;
    // The last version claiming to have been replaced by the first.
    store
        .execute_for_tests(&format!(
            "UPDATE type::record('drawer', '{}') SET superseded_by = '{}'",
            versions[2].id, versions[0].id
        ))
        .await;

    let found = store.drawer_lineage(versions[0].id).await.unwrap().unwrap();

    assert_eq!(found.len(), 3, "each version appears once");
}

#[tokio::test]
async fn history_survives_a_missing_link_target_by_stopping_there() {
    let store = SurrealStore::connect_memory_for_tests().await;
    let (versions, _) = chain(&store).await;
    store.delete_drawer(versions[2].id).await.unwrap();

    let found = store.drawer_lineage(versions[0].id).await.unwrap().unwrap();

    let ids: Vec<DrawerId> = found.iter().map(|d| d.id).collect();
    assert_eq!(ids, [versions[0].id, versions[1].id]);
}

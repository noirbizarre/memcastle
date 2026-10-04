//! Entity resolution: deciding whether a name seen in some source is an entity the graph already knows.
//!
//! A pure decision over candidates the store proposes (docs/adr/025). It is deliberately conservative: it converges
//! two spellings only on evidence a person would accept without thinking (same name up to case and punctuation, a
//! recorded alias, a single unambiguous typo in a name long enough that a typo is the likelier explanation), and
//! otherwise says "distinct", naming the entities it resembles so they can be reviewed later. It never merges two
//! entities that already exist.

use serde::{Deserialize, Serialize};

use super::EntityId;
use super::fingerprint::edit_distance;

/// The shortest key, in characters, a typo may be forgiven in. Below it a one-character difference is as likely to
/// be a different name (`Maria` / `Mario`, `Ana` / `Ann`) as a slip.
pub const MIN_TYPO_KEY_CHARS: usize = 6;
/// The shortest key worth proposing as a *possible* match. Shorter names resemble too many others.
const MIN_POSSIBLE_KEY_CHARS: usize = 4;
/// The most resemblances kept for review per observed name.
const MAX_POSSIBLE: usize = 5;
/// The lowest similarity at which a resemblance is worth reviewing.
const POSSIBLE_SIMILARITY: f32 = 0.75;

/// A name reduced to what identifies a thing regardless of how it was typed: lowercased, with letters and digits
/// kept, `+` and `#` kept (`C++` and `C#` are not `C`), apostrophes and dots dropped (`Node.js` is `nodejs`) and
/// every other run of characters (spaces, hyphens, underscores, slashes) collapsed to one space.
///
/// Falls back to the trimmed, lowercased name when nothing survives, so a name made of symbols still has a key.
#[must_use]
pub fn entity_key(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_space = false;
    for c in name.chars() {
        if c.is_alphanumeric() || c == '+' || c == '#' {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.extend(c.to_lowercase());
        } else if matches!(c, '.' | '\'' | '\u{2019}') {
            // Joins the neighbours instead of splitting them.
        } else {
            pending_space = true;
        }
    }
    if out.is_empty() {
        name.trim().to_lowercase()
    } else {
        out
    }
}

/// An entity the graph already holds, as far as resolution needs to know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityCandidate {
    /// Its identity.
    pub id: EntityId,
    /// Its canonical display name.
    pub name: String,
    /// Its kind.
    pub kind: String,
    /// Other spellings recorded for it.
    pub aliases: Vec<String>,
}

/// Why two names were taken to be one entity, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionRule {
    /// The same name and kind.
    Exact,
    /// The same key: the names differ only in case, punctuation or spacing.
    Normalized,
    /// The name is a recorded alias of the entity.
    Alias,
    /// One edit away from exactly one entity, in a name long enough for that to be a typo.
    Typo,
}

impl ResolutionRule {
    /// The stored label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Normalized => "normalized",
            Self::Alias => "alias",
            Self::Typo => "typo",
        }
    }
}

/// An entity a name resembles without being able to say it is the same.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PossibleMatch {
    /// The entity it resembles.
    pub entity: EntityId,
    /// How alike the keys are, in `(0, 1)`.
    pub similarity: f32,
}

/// What resolution decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// The name is this entity.
    Same {
        /// The entity.
        entity: EntityId,
        /// Why.
        rule: ResolutionRule,
        /// How sure, in `(0, 1]`.
        confidence: f32,
    },
    /// The name is new. `possible` lists entities it resembles but could not be equated with: empty when nothing
    /// resembles it, non-empty when the match is ambiguous and a person (or a later pass) should decide.
    Distinct {
        /// Resemblances to review, closest first.
        possible: Vec<PossibleMatch>,
    },
}

/// Whether an observation of kind `observed` may be the same thing as an entity of kind `existing`.
///
/// A specific kind only matches itself; a vague `other` observation (the extractor did not know) may match any.
fn compatible(observed: &str, existing: &str) -> bool {
    observed == existing || observed == "other"
}

/// Decide what `name` of `kind` is, given `candidates` (every entity the store thinks it might be).
///
/// `fuzzy` allows the typo rule and the recording of possible matches; with it off only spelling variants and
/// recorded aliases converge.
#[must_use]
pub fn resolve(name: &str, kind: &str, candidates: &[EntityCandidate], fuzzy: bool) -> Resolution {
    let key = entity_key(name);
    let compatible_candidates: Vec<&EntityCandidate> = candidates
        .iter()
        .filter(|c| compatible(kind, &c.kind))
        .collect();

    // 1. The exact name. Preferring the same kind, then the lowest id, keeps the answer stable when older data
    // already holds duplicates.
    if let Some(best) = best_of(
        compatible_candidates
            .iter()
            .copied()
            .filter(|c| c.name == name),
        kind,
    ) {
        return same(best, ResolutionRule::Exact, 1.0);
    }
    // 2. The same key.
    if let Some(best) = best_of(
        compatible_candidates
            .iter()
            .copied()
            .filter(|c| entity_key(&c.name) == key),
        kind,
    ) {
        return same(best, ResolutionRule::Normalized, 0.98);
    }
    // 3. A recorded alias.
    if let Some(best) = best_of(
        compatible_candidates
            .iter()
            .copied()
            .filter(|c| c.aliases.iter().any(|a| entity_key(a) == key)),
        kind,
    ) {
        return same(best, ResolutionRule::Alias, 0.95);
    }
    if !fuzzy {
        return Resolution::Distinct {
            possible: Vec::new(),
        };
    }

    // 4. Resemblance: the closest key of each candidate, canonical or alias.
    let key_chars: Vec<char> = key.chars().collect();
    let mut near: Vec<(&EntityCandidate, usize, f32)> = Vec::new();
    for candidate in &compatible_candidates {
        let closest = std::iter::once(candidate.name.as_str())
            .chain(candidate.aliases.iter().map(String::as_str))
            .map(|spelling| {
                let other: Vec<char> = entity_key(spelling).chars().collect();
                let distance = edit_distance(&key_chars, &other);
                let longer = key_chars.len().max(other.len());
                (distance, longer, other.len())
            })
            .min_by_key(|(distance, ..)| *distance);
        if let Some((distance, longer, other_len)) = closest {
            if longer == 0 {
                continue;
            }
            #[allow(clippy::cast_precision_loss)]
            let similarity = 1.0 - distance as f32 / longer as f32;
            let shortest = key_chars.len().min(other_len);
            if distance == 1 && shortest >= MIN_TYPO_KEY_CHARS {
                near.push((candidate, distance, similarity));
            } else if distance <= 2
                && shortest >= MIN_POSSIBLE_KEY_CHARS
                && similarity >= POSSIBLE_SIMILARITY
            {
                // Resembles, but not strongly enough to be called a typo.
                near.push((candidate, distance.max(2), similarity));
            }
        }
    }

    // A typo needs exactly one candidate to be the explanation: with two, the name is ambiguous.
    let typos: Vec<&(&EntityCandidate, usize, f32)> = near
        .iter()
        .filter(|(c, distance, _)| *distance == 1 && is_typo_eligible(&key_chars, c))
        .collect();
    if let [(candidate, _, similarity)] = typos.as_slice() {
        return same(candidate, ResolutionRule::Typo, *similarity);
    }

    near.sort_by(|a, b| {
        b.2.total_cmp(&a.2)
            .then_with(|| a.0.id.to_string().cmp(&b.0.id.to_string()))
    });
    Resolution::Distinct {
        possible: near
            .into_iter()
            .take(MAX_POSSIBLE)
            .map(|(candidate, _, similarity)| PossibleMatch {
                entity: candidate.id,
                similarity,
            })
            .collect(),
    }
}

/// Whether `candidate` has a spelling at distance one from `key` that is long enough for a typo to be the
/// explanation (the distance filter above is per candidate, not per spelling).
fn is_typo_eligible(key: &[char], candidate: &EntityCandidate) -> bool {
    std::iter::once(candidate.name.as_str())
        .chain(candidate.aliases.iter().map(String::as_str))
        .any(|spelling| {
            let other: Vec<char> = entity_key(spelling).chars().collect();
            other.len().min(key.len()) >= MIN_TYPO_KEY_CHARS && edit_distance(key, &other) == 1
        })
}

fn same(candidate: &EntityCandidate, rule: ResolutionRule, confidence: f32) -> Resolution {
    Resolution::Same {
        entity: candidate.id,
        rule,
        confidence,
    }
}

/// The preferred of several candidates that all fit, then the lowest id so the answer never depends on the order
/// the store listed them.
///
/// A specific observation prefers the candidate of exactly its kind. A vague (`other`) one prefers a candidate that
/// has a specific kind, so an extractor that could not classify a name joins the better-typed entity instead of
/// another vague one.
fn best_of<'a>(
    candidates: impl Iterator<Item = &'a EntityCandidate>,
    kind: &str,
) -> Option<&'a EntityCandidate> {
    let preference = |c: &EntityCandidate| {
        if kind == "other" {
            c.kind == "other"
        } else {
            c.kind != kind
        }
    };
    candidates.min_by(|a, b| {
        preference(a)
            .cmp(&preference(b))
            .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(name: &str, kind: &str, aliases: &[&str]) -> EntityCandidate {
        EntityCandidate {
            id: EntityId::new(),
            name: name.to_string(),
            kind: kind.to_string(),
            aliases: aliases.iter().map(ToString::to_string).collect(),
        }
    }

    fn same_as(resolution: &Resolution, expected: &EntityCandidate) -> ResolutionRule {
        match resolution {
            Resolution::Same { entity, rule, .. } => {
                assert_eq!(*entity, expected.id, "resolved to the wrong entity");
                *rule
            }
            other => panic!("expected a match, got {other:?}"),
        }
    }

    #[test]
    fn casing_punctuation_and_spacing_variants_share_a_key() {
        let key = entity_key("Jean-Luc  Picard");
        assert_eq!(key, "jean luc picard");
        assert_eq!(entity_key("jean_luc picard"), key);
        assert_eq!(entity_key("Node.js"), entity_key("NodeJS"));
        assert_eq!(entity_key("O'Brien"), entity_key("OBrien"));
    }

    #[test]
    fn symbols_that_carry_meaning_are_kept_in_the_key() {
        assert_ne!(entity_key("C++"), entity_key("C"));
        assert_ne!(entity_key("C#"), entity_key("C"));
        assert_eq!(entity_key("???"), "???", "a name of symbols keeps a key");
    }

    #[test]
    fn a_casing_variant_converges_on_the_known_entity() {
        let ada = entity("Ada Lovelace", "person", &[]);
        let resolution = resolve("ada lovelace", "person", std::slice::from_ref(&ada), true);
        assert_eq!(same_as(&resolution, &ada), ResolutionRule::Normalized);
    }

    #[test]
    fn the_exact_name_is_preferred_over_a_variant() {
        let upper = entity("ADA", "person", &[]);
        let exact = entity("Ada", "person", &[]);
        let resolution = resolve("Ada", "person", &[upper, exact.clone()], true);
        assert_eq!(same_as(&resolution, &exact), ResolutionRule::Exact);
    }

    #[test]
    fn a_recorded_alias_converges_on_its_entity() {
        let project = entity("MemCastle", "project", &["the castle"]);
        let resolution = resolve(
            "The Castle",
            "project",
            std::slice::from_ref(&project),
            true,
        );
        assert_eq!(same_as(&resolution, &project), ResolutionRule::Alias);
    }

    #[test]
    fn aliases_converge_even_when_fuzzy_matching_is_off() {
        let project = entity("MemCastle", "project", &["the castle"]);
        let resolution = resolve(
            "the castle",
            "project",
            std::slice::from_ref(&project),
            false,
        );
        assert_eq!(same_as(&resolution, &project), ResolutionRule::Alias);
    }

    #[test]
    fn a_single_unambiguous_typo_in_a_long_name_converges() {
        let surreal = entity("SurrealDB", "tool", &[]);
        let resolution = resolve("SurealDB", "tool", std::slice::from_ref(&surreal), true);
        assert_eq!(same_as(&resolution, &surreal), ResolutionRule::Typo);
    }

    #[test]
    fn a_typo_in_a_short_name_is_not_forgiven() {
        let maria = entity("Maria", "person", &[]);
        let resolution = resolve("Mario", "person", std::slice::from_ref(&maria), true);
        match resolution {
            Resolution::Distinct { possible } => {
                assert_eq!(possible.len(), 1, "it is still worth reviewing");
                assert_eq!(possible[0].entity, maria.id);
            }
            other => panic!("expected distinct, got {other:?}"),
        }
    }

    #[test]
    fn a_name_one_edit_from_two_entities_stays_distinct_and_names_both() {
        let johnson = entity("Johnson", "person", &[]);
        let johnsen = entity("Johnsen", "person", &[]);
        let resolution = resolve(
            "Johnsin",
            "person",
            &[johnson.clone(), johnsen.clone()],
            true,
        );
        match resolution {
            Resolution::Distinct { possible } => {
                let ids: Vec<EntityId> = possible.iter().map(|p| p.entity).collect();
                assert!(ids.contains(&johnson.id) && ids.contains(&johnsen.id));
            }
            other => panic!("expected an ambiguous result, got {other:?}"),
        }
    }

    #[test]
    fn a_typo_never_converges_when_fuzzy_matching_is_off() {
        let surreal = entity("SurrealDB", "tool", &[]);
        let resolution = resolve("SurealDB", "tool", &[surreal], false);
        assert_eq!(
            resolution,
            Resolution::Distinct {
                possible: Vec::new()
            }
        );
    }

    #[test]
    fn a_specific_kind_never_matches_an_entity_of_another_kind() {
        let apple_company = entity("Apple Inc", "organization", &[]);
        let resolution = resolve("apple inc", "product", &[apple_company], true);
        assert_eq!(
            resolution,
            Resolution::Distinct {
                possible: Vec::new()
            }
        );
    }

    #[test]
    fn a_vague_observation_attaches_to_the_entity_of_any_kind() {
        let ada = entity("Ada", "person", &[]);
        let resolution = resolve("ADA", "other", std::slice::from_ref(&ada), true);
        assert_eq!(same_as(&resolution, &ada), ResolutionRule::Normalized);
    }

    #[test]
    fn unrelated_names_resemble_nothing() {
        let ada = entity("Ada Lovelace", "person", &[]);
        let resolution = resolve("Grace Hopper", "person", &[ada], true);
        assert_eq!(
            resolution,
            Resolution::Distinct {
                possible: Vec::new()
            }
        );
    }

    #[test]
    fn an_exact_match_on_older_duplicates_is_stable() {
        let a = entity("Ada", "person", &[]);
        let b = entity("ada", "person", &[]);
        let first = resolve("ADA", "person", &[a.clone(), b.clone()], true);
        let second = resolve("ADA", "person", &[b, a], true);
        assert_eq!(first, second, "candidate order must not change the answer");
    }
}

//! The built-in extractor: deterministic, dependency-free, and deliberately modest.
//!
//! It finds three kinds of entity — runs of capitalised words (`Ada Lovelace`, `MemCastle`), `@handles` and
//! `` `code spans` `` — and one kind of relationship: two entities in one sentence joined by a phrase from a fixed
//! table (`works on`, `depends on`, `is part of`, ...). It does not guess: two entities that merely share a
//! sentence are not related, and a sentence it does not recognise yields entities and no edge. That is the point of
//! a default that sends nothing anywhere; an LLM provider is the way to more.
//!
//! The answer depends only on the text, so two runs over the same drawer store the same graph.

use super::{ExtractFuture, Extractor};
use crate::domain::{EntityKind, ExtractedEntity, ExtractedGraph, ExtractedRelation, Predicate};

/// The confidence of a relationship read from a known phrase: better than a guess, short of a model's judgement.
const PHRASE_CONFIDENCE: f32 = 0.6;

/// Phrases (lowercase, space-separated) that join a subject to an object, and what they say. Longer phrases are
/// listed first only for readability: a match must cover everything between the two entities.
const PHRASES: &[(&str, Predicate)] = &[
    ("works on", Predicate::WorksOn),
    ("is working on", Predicate::WorksOn),
    ("worked on", Predicate::WorksOn),
    ("is a member of", Predicate::MemberOf),
    ("is member of", Predicate::MemberOf),
    ("member of", Predicate::MemberOf),
    ("belongs to", Predicate::MemberOf),
    ("joined", Predicate::MemberOf),
    ("depends on", Predicate::DependsOn),
    ("relies on", Predicate::DependsOn),
    ("uses", Predicate::Uses),
    ("is using", Predicate::Uses),
    ("owns", Predicate::Owns),
    ("maintains", Predicate::Owns),
    ("is part of", Predicate::PartOf),
    ("part of", Predicate::PartOf),
    ("is located in", Predicate::LocatedIn),
    ("lives in", Predicate::LocatedIn),
    ("is based in", Predicate::LocatedIn),
];

/// Capitalised words that start sentences without naming anything.
const SENTENCE_STARTERS: &[&str] = &[
    "a",
    "an",
    "the",
    "i",
    "we",
    "you",
    "he",
    "she",
    "it",
    "they",
    "this",
    "that",
    "these",
    "those",
    "our",
    "their",
    "his",
    "her",
    "its",
    "my",
    "your",
    "in",
    "on",
    "at",
    "of",
    "for",
    "to",
    "and",
    "but",
    "or",
    "so",
    "then",
    "also",
    "if",
    "when",
    "while",
    "after",
    "before",
    "because",
    "however",
    "there",
    "here",
    "see",
    "note",
    "todo",
    "yes",
    "no",
    "ok",
    "now",
    "next",
    "first",
    "finally",
    "today",
    "yesterday",
    "tomorrow",
];

/// See the module documentation.
pub struct HeuristicExtractor;

impl Extractor for HeuristicExtractor {
    fn name(&self) -> &'static str {
        "heuristic"
    }

    fn extract<'a>(&'a self, texts: &'a [String]) -> ExtractFuture<'a> {
        Box::pin(async move { Ok(texts.iter().map(|text| extract_one(text)).collect()) })
    }
}

/// What kind of thing a token is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// A capitalised word that can join a run.
    Capitalised,
    /// A `` `code span` ``.
    Code,
    /// An `@handle`.
    Handle,
    /// Anything else.
    Plain,
}

#[derive(Debug, Clone)]
struct Token {
    /// The word with surrounding punctuation removed.
    text: String,
    shape: Shape,
    /// The token ended with a clause break (`,`, `;`, `:`, `)`), so a run cannot continue across it.
    breaks_after: bool,
}

/// One entity found in a sentence, as a token range.
struct Span {
    start: usize,
    end: usize,
    name: String,
    kind: EntityKind,
}

fn extract_one(text: &str) -> ExtractedGraph {
    let mut entities: Vec<ExtractedEntity> = Vec::new();
    let mut relations: Vec<ExtractedRelation> = Vec::new();

    for sentence in sentences(text) {
        let tokens = tokenize(sentence);
        let mut spans = spans(&tokens);
        let found = relations_in(&tokens, &mut spans);
        for span in &spans {
            entities.push(ExtractedEntity {
                name: span.name.clone(),
                kind: span.kind,
            });
        }
        relations.extend(found);
    }
    // Sentence by sentence an entity may be seen first as `other` and later as something specific; keep the most
    // specific kind it was ever given, so order in the text does not change what the entity is.
    entities.sort_by_key(|entity| entity.kind == EntityKind::Other);
    ExtractedGraph {
        entities,
        relations,
    }
}

/// Split `text` into sentences: at a line break, or at `.`, `!` or `?` followed by whitespace or the end (so
/// `a.md` and `v1.2` stay whole).
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let boundary = match c {
            '\n' => true,
            '.' | '!' | '?' => chars.peek().is_none_or(|(_, next)| next.is_whitespace()),
            _ => false,
        };
        if boundary {
            out.push(&text[start..index]);
            start = index + c.len_utf8();
        }
    }
    out.push(&text[start..]);
    out.into_iter()
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
        .collect()
}

fn tokenize(sentence: &str) -> Vec<Token> {
    // Markdown furniture at the start of a line (`## `, `- `, `> `) is not part of any name.
    let sentence = sentence.trim_start_matches(|c: char| {
        matches!(c, '#' | '-' | '*' | '>' | '•') || c.is_whitespace()
    });
    let mut tokens = Vec::new();
    for raw in sentence.split_whitespace() {
        let breaks_after = raw.ends_with([',', ';', ':', ')', '"']);
        let stripped =
            raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '@' && c != '`' && c != '_');
        if stripped.is_empty() {
            continue;
        }
        if stripped.len() > 2 && stripped.starts_with('`') && stripped.ends_with('`') {
            let inner = stripped.trim_matches('`');
            if !inner.is_empty() {
                tokens.push(Token {
                    text: inner.to_string(),
                    shape: Shape::Code,
                    breaks_after,
                });
            }
            continue;
        }
        if let Some(handle) = stripped.strip_prefix('@') {
            let handle = handle.trim_matches(|c: char| !c.is_alphanumeric());
            if handle.chars().count() > 1 {
                tokens.push(Token {
                    text: handle.to_string(),
                    shape: Shape::Handle,
                    breaks_after,
                });
            }
            continue;
        }
        // A possessive names the same thing: "Ada's" is "Ada".
        let word = stripped.strip_suffix("'s").unwrap_or(stripped);
        let word = word.trim_matches(|c: char| !c.is_alphanumeric());
        if word.is_empty() {
            continue;
        }
        let capitalised = word.chars().next().is_some_and(char::is_uppercase)
            && word.chars().any(char::is_alphabetic)
            && word.chars().count() > 1;
        tokens.push(Token {
            text: word.to_string(),
            shape: if capitalised {
                Shape::Capitalised
            } else {
                Shape::Plain
            },
            breaks_after,
        });
    }
    // A capitalised word opening the sentence is capitalised because it opens the sentence, unless it is one the
    // reader would not mistake for a name.
    if let Some(first) = tokens.first_mut()
        && first.shape == Shape::Capitalised
        && SENTENCE_STARTERS.contains(&first.text.to_lowercase().as_str())
    {
        first.shape = Shape::Plain;
    }
    tokens
}

fn spans(tokens: &[Token]) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index].shape {
            Shape::Capitalised => {
                let start = index;
                // A run continues over capitalised words, until a clause break.
                while !tokens[index].breaks_after
                    && tokens
                        .get(index + 1)
                        .is_some_and(|t| t.shape == Shape::Capitalised)
                {
                    index += 1;
                }
                let name = tokens[start..=index]
                    .iter()
                    .map(|t| t.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                spans.push(Span {
                    start,
                    end: index + 1,
                    name,
                    kind: EntityKind::Other,
                });
            }
            Shape::Code => spans.push(Span {
                start: index,
                end: index + 1,
                name: tokens[index].text.clone(),
                kind: EntityKind::Tool,
            }),
            Shape::Handle => spans.push(Span {
                start: index,
                end: index + 1,
                name: tokens[index].text.clone(),
                kind: EntityKind::Person,
            }),
            Shape::Plain => {}
        }
        index += 1;
    }
    spans
}

/// The relationships in one sentence: for each pair of neighbouring entities, whether the words between them are
/// exactly one of the known phrases. Entities an edge pins down (the object of `located in` is a place) are typed
/// accordingly if nothing better is known.
fn relations_in(tokens: &[Token], spans: &mut [Span]) -> Vec<ExtractedRelation> {
    let mut found = Vec::new();
    for pair in 0..spans.len().saturating_sub(1) {
        let (left, right) = (&spans[pair], &spans[pair + 1]);
        let between: Vec<String> = tokens[left.end..right.start]
            .iter()
            .map(|t| t.text.to_lowercase())
            .collect();
        // A clause break between them means the phrase does not join them.
        if tokens[left.end - 1].breaks_after && left.end != right.start {
            continue;
        }
        let phrase = between.join(" ");
        let Some(&(_, predicate)) = PHRASES.iter().find(|(known, _)| *known == phrase) else {
            continue;
        };
        found.push(ExtractedRelation {
            subject: left.name.clone(),
            predicate,
            object: right.name.clone(),
            confidence: PHRASE_CONFIDENCE,
        });
        let (subject_kind, object_kind) = match predicate {
            Predicate::WorksOn => (Some(EntityKind::Person), Some(EntityKind::Project)),
            Predicate::MemberOf => (Some(EntityKind::Person), Some(EntityKind::Organization)),
            Predicate::LocatedIn => (None, Some(EntityKind::Place)),
            _ => (None, None),
        };
        if let Some(kind) = subject_kind
            && spans[pair].kind == EntityKind::Other
        {
            spans[pair].kind = kind;
        }
        if let Some(kind) = object_kind
            && spans[pair + 1].kind == EntityKind::Other
        {
            spans[pair + 1].kind = kind;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(graph: &ExtractedGraph) -> Vec<(&str, EntityKind)> {
        let mut out: Vec<_> = graph
            .entities
            .iter()
            .map(|e| (e.name.as_str(), e.kind))
            .collect();
        out.sort_by_key(|(name, _)| *name);
        out
    }

    #[test]
    fn a_known_phrase_between_two_names_becomes_a_typed_relationship() {
        let graph = extract_one("Ada Lovelace works on MemCastle.");
        assert_eq!(
            names(&graph),
            vec![
                ("Ada Lovelace", EntityKind::Person),
                ("MemCastle", EntityKind::Project)
            ]
        );
        assert_eq!(graph.relations.len(), 1);
        let relation = &graph.relations[0];
        assert_eq!(relation.subject, "Ada Lovelace");
        assert_eq!(relation.predicate, Predicate::WorksOn);
        assert_eq!(relation.object, "MemCastle");
    }

    #[test]
    fn two_names_sharing_a_sentence_without_a_known_phrase_are_not_related() {
        let graph = extract_one("Ada met Bob yesterday.");
        assert_eq!(graph.entities.len(), 2);
        assert!(graph.relations.is_empty());
    }

    #[test]
    fn a_sentence_starter_is_not_mistaken_for_a_name() {
        let graph = extract_one("The team ships on Friday. We like it.");
        // `Friday` is capitalised mid-sentence, and that is all this extractor can know of it.
        assert_eq!(names(&graph), vec![("Friday", EntityKind::Other)]);
    }

    #[test]
    fn code_spans_and_handles_are_entities_of_their_own_kind() {
        let graph = extract_one("@ada depends on `surrealdb` here.");
        assert_eq!(
            names(&graph),
            vec![("ada", EntityKind::Person), ("surrealdb", EntityKind::Tool)]
        );
        assert_eq!(graph.relations[0].predicate, Predicate::DependsOn);
    }

    #[test]
    fn a_comma_ends_a_run_of_capitalised_words() {
        let graph = extract_one("We thank Ada, Bob and Carol.");
        let found: Vec<_> = names(&graph).iter().map(|(n, _)| *n).collect();
        assert_eq!(found, vec!["Ada", "Bob", "Carol"]);
    }

    #[test]
    fn a_location_phrase_types_its_object_as_a_place() {
        let graph = extract_one("Grace is based in Lisbon.");
        assert!(names(&graph).contains(&("Lisbon", EntityKind::Place)));
        assert_eq!(graph.relations[0].predicate, Predicate::LocatedIn);
    }

    #[test]
    fn markdown_furniture_and_possessives_do_not_leak_into_names() {
        let graph = extract_one("## Ada's notes\n- Bob maintains Parser.");
        assert!(names(&graph).contains(&("Ada", EntityKind::Other)));
        assert!(
            graph.relations.iter().any(|r| r.subject == "Bob"
                && r.predicate == Predicate::Owns
                && r.object == "Parser")
        );
    }

    #[test]
    fn a_dot_inside_a_filename_does_not_split_the_sentence() {
        assert_eq!(
            sentences("See Notes.md for v1.2 details. Done"),
            vec!["See Notes.md for v1.2 details", "Done"]
        );
    }

    #[test]
    fn text_with_nothing_to_name_yields_an_empty_graph() {
        assert_eq!(
            extract_one("nothing here, all lowercase."),
            ExtractedGraph::default()
        );
        assert_eq!(extract_one(""), ExtractedGraph::default());
    }

    #[test]
    fn the_same_text_always_yields_the_same_graph() {
        let text = "Ada works on MemCastle. Bob uses `rust`.";
        assert_eq!(extract_one(text), extract_one(text));
    }

    #[test]
    fn the_most_specific_kind_wins_whatever_the_order() {
        let graph = extract_one("Ada met Bob. Ada works on Castle.");
        let kinds: Vec<_> = graph
            .entities
            .iter()
            .filter(|e| e.name == "Ada")
            .map(|e| e.kind)
            .collect();
        // Dedup keeps the first; the specific one must come first.
        assert_eq!(kinds.first(), Some(&EntityKind::Person));
    }
}

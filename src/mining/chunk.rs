//! The chunker: cuts a canonical document into the texts that become drawers.
//!
//! Pure and source-agnostic: it sees [`Segment`]s and a size, nothing about where they came from. Three rules keep
//! mining honest:
//!
//! - **A document that fits is one chunk, verbatim.** Segments are concatenated as they are, so what is filed is
//!   what the source said.
//! - **Whole segments stay together.** A message or paragraph the adapter delimited is never cut unless it alone is
//!   larger than a chunk.
//! - **Deterministic.** The same segments and size always produce the same chunks, because a chunk's hash is how a
//!   re-mine recognises it: a chunker that wobbled would turn every re-mine into a rewrite.
//!
//! Sizes are in characters, not bytes, so a cut never lands inside a multi-byte character and a chunk's size means
//! the same thing for every script.

use crate::domain::Segment;

/// Cut `segments` into chunks of at most `max_chars` characters (a segment that cannot be placed intact is split;
/// see [`split_oversized`]). Chunks that are only whitespace are dropped: they would be drawers nothing can match.
///
/// Concatenating the returned chunks gives the concatenation of the segments, minus those whitespace-only chunks.
#[must_use]
pub fn chunk(segments: &[Segment], max_chars: usize) -> Vec<String> {
    // Defensive floor: a size of zero would never make progress.
    let max_chars = max_chars.max(1);
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0usize;

    // Closes the open chunk: kept if it says anything, dropped if it is only whitespace.
    fn flush(current: &mut String, chunks: &mut Vec<String>) {
        if current.trim().is_empty() {
            current.clear();
        } else {
            chunks.push(std::mem::take(current));
        }
    }

    for segment in segments {
        let size = segment.text.chars().count();
        if size > max_chars {
            // Larger than any chunk: close what is open, split this one, and leave its last piece open so the
            // segments after it can still join it.
            flush(&mut current, &mut chunks);
            let mut pieces = split_oversized(&segment.text, max_chars);
            let last = pieces.pop().unwrap_or_default();
            chunks.extend(pieces.into_iter().filter(|piece| !piece.trim().is_empty()));
            current_chars = last.chars().count();
            current = last;
        } else if current_chars + size > max_chars {
            flush(&mut current, &mut chunks);
            current.clone_from(&segment.text);
            current_chars = size;
        } else {
            current.push_str(&segment.text);
            current_chars += size;
        }
    }
    flush(&mut current, &mut chunks);
    chunks
}

/// Split `text` into pieces of at most `max_chars` characters, cutting at the latest paragraph break (a blank
/// line) in the second half of the window, else the latest line break there, else mid-line.
///
/// Preferring structure keeps a piece readable on its own; requiring the break to be in the second half keeps one
/// early blank line from producing a tiny piece followed by a huge one.
fn split_oversized(text: &str, max_chars: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut rest = text;
    loop {
        // The byte offset just past the `max_chars`th character; `None` means the rest fits.
        let Some(end) = rest.char_indices().nth(max_chars).map(|(index, _)| index) else {
            pieces.push(rest.to_string());
            return pieces;
        };
        let window = &rest[..end];
        let half = end / 2;
        let cut = window
            .rfind("\n\n")
            .filter(|at| *at >= half)
            .map(|at| at + 2)
            .or_else(|| window.rfind('\n').filter(|at| *at >= half).map(|at| at + 1))
            .unwrap_or(end);
        pieces.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segments(texts: &[&str]) -> Vec<Segment> {
        texts
            .iter()
            .map(|text| Segment {
                text: (*text).to_string(),
            })
            .collect()
    }

    #[test]
    fn a_document_that_fits_is_one_chunk_identical_to_its_text() {
        let text = "line one\nline two\n\nline four\n";
        assert_eq!(chunk(&segments(&[text]), 1000), vec![text.to_string()]);
    }

    #[test]
    fn concatenating_the_chunks_gives_back_the_document() {
        let text = "alpha beta\n".repeat(500);
        let chunks = chunk(&segments(&[&text]), 700);
        assert!(chunks.len() > 1);
        assert_eq!(
            chunks.concat(),
            text,
            "a cut must never drop or reorder text"
        );
    }

    #[test]
    fn no_chunk_exceeds_the_size() {
        let text = "word ".repeat(1000);
        for chunk in chunk(&segments(&[&text]), 333) {
            assert!(chunk.chars().count() <= 333);
        }
    }

    #[test]
    fn whole_segments_are_packed_together_up_to_the_size() {
        let chunks = chunk(&segments(&["aaaa\n", "bbbb\n", "cccc\n"]), 10);
        assert_eq!(
            chunks,
            vec!["aaaa\nbbbb\n".to_string(), "cccc\n".to_string()]
        );
    }

    #[test]
    fn a_segment_is_never_split_across_chunks_when_it_fits_in_one() {
        let chunks = chunk(&segments(&["aaaaaa", "bbbbbb"]), 8);
        assert_eq!(chunks, vec!["aaaaaa".to_string(), "bbbbbb".to_string()]);
    }

    #[test]
    fn an_oversized_segment_is_cut_at_a_paragraph_break_when_there_is_one() {
        let text = format!("{}\n\n{}", "a".repeat(60), "b".repeat(60));
        let chunks = chunk(&segments(&[&text]), 100);
        assert_eq!(chunks[0], format!("{}\n\n", "a".repeat(60)));
        assert_eq!(chunks[1], "b".repeat(60));
    }

    #[test]
    fn an_oversized_segment_without_paragraphs_is_cut_at_a_line_break() {
        let text = format!("{}\n{}", "a".repeat(60), "b".repeat(60));
        let chunks = chunk(&segments(&[&text]), 100);
        assert_eq!(chunks[0], format!("{}\n", "a".repeat(60)));
    }

    #[test]
    fn a_line_longer_than_a_chunk_is_cut_mid_line_rather_than_left_oversized() {
        let chunks = chunk(&segments(&[&"x".repeat(250)]), 100);
        assert_eq!(
            chunks.iter().map(|c| c.chars().count()).collect::<Vec<_>>(),
            vec![100, 100, 50]
        );
    }

    #[test]
    fn a_cut_never_lands_inside_a_multi_byte_character() {
        // 3-byte characters: a byte-based cut at 100 would split one.
        let text = "€".repeat(250);
        let chunks = chunk(&segments(&[&text]), 100);
        assert_eq!(chunks.concat(), text);
        assert!(chunks.iter().all(|c| c.chars().count() <= 100));
    }

    #[test]
    fn segments_after_an_oversized_one_join_its_last_piece() {
        let chunks = chunk(&segments(&[&"x".repeat(150), "tail"]), 100);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[1].ends_with("tail"));
    }

    #[test]
    fn whitespace_only_chunks_are_dropped() {
        assert!(chunk(&segments(&["  \n\n ", "\n"]), 100).is_empty());
        assert!(chunk(&[], 100).is_empty());
    }

    #[test]
    fn chunking_is_deterministic() {
        let text = "some text\n\nmore text\n".repeat(200);
        assert_eq!(
            chunk(&segments(&[&text]), 250),
            chunk(&segments(&[&text]), 250),
            "a re-mine recognises chunks by hash, so the same input must cut the same way"
        );
    }

    #[test]
    fn appending_to_a_document_leaves_every_earlier_chunk_unchanged() {
        // The property that makes re-mining a growing transcript cheap: only the tail chunk(s) change.
        let base: Vec<String> = (0..40).map(|i| format!("message number {i}\n\n")).collect();
        let grown: Vec<String> = base
            .iter()
            .cloned()
            .chain(["one more message\n\n".to_string()])
            .collect();
        let as_segments = |texts: &[String]| {
            texts
                .iter()
                .map(|text| Segment { text: text.clone() })
                .collect::<Vec<_>>()
        };
        let before = chunk(&as_segments(&base), 200);
        let after = chunk(&as_segments(&grown), 200);
        assert!(after.len() >= before.len());
        assert_eq!(
            &before[..before.len() - 1],
            &after[..before.len() - 1],
            "everything but the last chunk must be untouched by an append"
        );
    }
}

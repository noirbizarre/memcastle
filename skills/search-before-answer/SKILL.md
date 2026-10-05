---
name: search-before-answer
description: Search MemCastle before answering any question that may depend on earlier sessions, past decisions, people, preferences or project history, and quote what it returns verbatim. Use at the start of such questions, not only at the start of a session.
license: MIT
compatibility: Needs the MemCastle MCP server connected, and a memory mode that allows reading.
metadata:
  memcastle-version: ">=0.2.0"
---

# Search before answering

MemCastle stores what earlier sessions decided to keep, and it never volunteers it.
An answer given without looking is a guess about the past.
Look first, then answer from what was found.

## When to search

Search before answering a question that could depend on something said or decided before:

- why something was built or chosen a certain way
- what the user prefers, and how they want things done
- what happened in earlier work, who was involved, what is still open
- anything phrased as "last time", "we decided", "remind me", "as usual"

Skip the search for questions that are self-contained, such as a syntax question, a request to edit the open file,
or a calculation.
Be generous but relevant: the aim is to never miss a memory that matters, not to call MemCastle on every message.

## How to search

1. Reduce the question to two to four keywords: names, nouns and distinctive terms.
   Matching is by words, stemmed and without synonyms, unless the daemon has an embedding provider
   (then it also matches by meaning), so do not count on "formatter" finding "prettier".
   Short keyword queries work better than sentences.
2. Call `memcastle_search` with the query, to see which drawers match.
   Narrow with `wing` or `room` when the topic clearly belongs to one project.
3. Call `memcastle_recall` with the same query when the wording matters, because it returns the stored content verbatim.
4. If nothing matches, retry once with fewer or different words before concluding that nothing is stored.
5. When the question is about the past, add `as_of` (an instant or a date such as `2026-01-01`) to see what was believed
   then, or `from` with `until` for a period.
   By default only what is true now is returned.
   To explain how a decision changed, search with `include_historical`, then call `memcastle_history` with a hit's `id`.

Every query word must match first, and only when no drawer has them all does it fall back to drawers with any of them,
so a long query is more likely to return loose matches.

## How to answer

- Quote retrieved content verbatim, in quotation marks, and say it came from memory.
  Never paraphrase a decision, a number, a name or a command: a paraphrase is a second, unchecked copy.
- Separate what was retrieved from what you infer, and say when the memory is old enough that it may have changed.
- If memory and the current repository or the user disagree, surface the conflict instead of picking silently.
- If nothing relevant was found, say so, and answer from what is actually known without implying a memory existed.

## Memory modes

The mode of the session limits what is possible, and this skill never overrides it.
In `disabled`, reads are refused with `memcastle::mode::forbidden`: stop searching, do not retry,
and answer as if MemCastle did not exist.
`read_only` allows searching and recalling.
Do not call `memcastle_set_mode` to get around a refusal.

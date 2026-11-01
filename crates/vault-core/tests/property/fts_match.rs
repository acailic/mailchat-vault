//! FTS MATCH semantics: `Storage::search` results must equal an independent
//! oracle of FTS5 `unicode61` semantics over the generated corpus —
//! unicode folding (Latin diacritics, Cyrillic case, CJK runs), prefixes,
//! phrases (never spanning body→author), and implicit AND for sanitized
//! queries.

use std::collections::BTreeSet;

use proptest::prelude::*;
use proptest::sample::select;

use vault_core::message::Message;
use vault_core::search::build_match_query;
use vault_core::storage::Storage;

use crate::fixtures::{self, ACCENT_WORDS, ASCII_WORDS};

/// How a query term may be spelled; all spellings of one word must be
/// interchangeable under `unicode61`.
#[derive(Clone, Copy, Debug)]
enum Spelling {
    Literal,
    Upper,
    Folded,
}

fn render(word: &str, spelling: Spelling) -> String {
    match spelling {
        Spelling::Literal => word.to_string(),
        Spelling::Upper => word
            .chars()
            .map(|c| c.to_uppercase().next().unwrap_or(c))
            .collect(),
        Spelling::Folded => fixtures::fold_str(word),
    }
}

fn spelling() -> impl Strategy<Value = Spelling> {
    prop_oneof![
        2 => Just(Spelling::Literal),
        1 => Just(Spelling::Upper),
        1 => Just(Spelling::Folded),
    ]
}

fn hit_ids(storage: &Storage, query: &str, limit: u32) -> BTreeSet<(String, String)> {
    storage
        .search(query, limit)
        .unwrap()
        .into_iter()
        .map(|m: Message| (m.source, m.external_id))
        .collect()
}

fn limit_for(model_len: usize) -> u32 {
    model_len as u32 + 8
}

proptest! {
    #![proptest_config(crate::prop_config(128))]

    // A quoted term matches exactly the messages whose body or author
    // tokenizes to that term — with unicode folding (diacritics, Cyrillic
    // case, CJK) and any query spelling of the word.
    #[test]
    fn term_matches_expected_set(
        batch in fixtures::batch(30),
        word in any_word_strategy(),
        spelling in spelling(),
    ) {
        let storage = Storage::open_in_memory().unwrap();
        for msg in &batch {
            storage.insert_message(msg).unwrap();
        }
        let model = fixtures::first_write_model(&batch);

        let query = fixtures::quoted_term(&render(&word, spelling));
        let expected: BTreeSet<_> = model
            .values()
            .filter(|m| fixtures::term_matches(m, &render(&word, spelling)))
            .map(|m| (m.source.clone(), m.external_id.clone()))
            .collect();

        let hits = hit_ids(&storage, &query, limit_for(model.len()));
        prop_assert_eq!(hits, expected, "query: {}", query);
    }

    // A prefix query `p*` matches exactly the messages with a token
    // starting with `p` (folded), including fused multi-script tokens.
    #[test]
    fn prefix_matches_expected_set(
        batch in fixtures::batch(30),
        word in any_word_strategy(),
        cut in 1usize..=4,
        spelling in spelling(),
    ) {
        let storage = Storage::open_in_memory().unwrap();
        for msg in &batch {
            storage.insert_message(msg).unwrap();
        }
        let model = fixtures::first_write_model(&batch);

        let spelled = render(&word, spelling);
        let chars: Vec<char> = spelled.chars().collect();
        let prefix: String = chars.iter().take(cut.min(chars.len())).collect();
        prop_assume!(!prefix.is_empty());

        let query = fixtures::quoted_prefix(&prefix);
        let expected: BTreeSet<_> = model
            .values()
            .filter(|m| fixtures::prefix_matches(m, &prefix))
            .map(|m| (m.source.clone(), m.external_id.clone()))
            .collect();

        let hits = hit_ids(&storage, &query, limit_for(model.len()));
        prop_assert_eq!(hits, expected, "query: {}", query);
    }

    // A phrase query `"a b"` matches exactly the messages where the folded
    // tokens `a`,`b` are adjacent inside ONE column (body or author) —
    // phrases never span the body→author boundary and ignore separator
    // punctuation.
    #[test]
    fn phrase_matches_expected_set(
        batch in fixtures::batch(30),
        a in any_word_strategy(),
        b in any_word_strategy(),
    ) {
        let storage = Storage::open_in_memory().unwrap();
        for msg in &batch {
            storage.insert_message(msg).unwrap();
        }
        let model = fixtures::first_write_model(&batch);

        let query = fixtures::quoted_phrase(&a, &b);
        let expected: BTreeSet<_> = model
            .values()
            .filter(|m| fixtures::phrase_matches(m, &a, &b))
            .map(|m| (m.source.clone(), m.external_id.clone()))
            .collect();

        let hits = hit_ids(&storage, &query, limit_for(model.len()));
        prop_assert_eq!(hits, expected, "query: {}", query);
    }

    // Phrases seeded from the corpus itself guarantee non-empty matches:
    // an adjacent token pair taken from a stored body must be found again.
    #[test]
    fn phrase_seeded_from_corpus_matches(
        batch in fixtures::batch(30),
        which in any::<u64>(),
    ) {
        prop_assume!(!batch.is_empty());
        let storage = Storage::open_in_memory().unwrap();
        for msg in &batch {
            storage.insert_message(msg).unwrap();
        }
        let model = fixtures::first_write_model(&batch);

        // Pick any stored message with ≥2 body tokens; use its adjacent pair.
        let candidates: Vec<_> = model
            .values()
            .filter(|m| fixtures::tokenize(&m.body).len() >= 2)
            .collect();
        prop_assume!(!candidates.is_empty());
        let seed_msg = candidates[(which as usize) % candidates.len()];
        let tokens = fixtures::tokenize(&seed_msg.body);
        let (a, b) = (&tokens[0], &tokens[1]);

        let query = fixtures::quoted_phrase(a, b);
        let hits = hit_ids(&storage, &query, limit_for(model.len()));
        prop_assert!(
            hits.contains(&(seed_msg.source.clone(), seed_msg.external_id.clone())),
            "seeded phrase must find its own message: {}", query
        );

        let expected: BTreeSet<_> = model
            .values()
            .filter(|m| fixtures::phrase_matches(m, a, b))
            .map(|m| (m.source.clone(), m.external_id.clone()))
            .collect();
        prop_assert_eq!(hits, expected);
    }

    // All spellings of one word (literal, UPPER, folded/base) return the
    // identical result set — the unicode-insensitivity contract.
    #[test]
    fn unicode_spellings_are_interchangeable(
        batch in fixtures::batch(30),
        word in any_word_strategy(),
    ) {
        let storage = Storage::open_in_memory().unwrap();
        for msg in &batch {
            storage.insert_message(msg).unwrap();
        }
        let model = fixtures::first_write_model(&batch);

        let sets: Vec<BTreeSet<_>> = [Spelling::Literal, Spelling::Upper, Spelling::Folded]
            .iter()
            .map(|&s| {
                let q = fixtures::quoted_term(&render(&word, s));
                hit_ids(&storage, &q, limit_for(model.len()))
            })
            .collect();
        prop_assert_eq!(&sets[0], &sets[1]);
        prop_assert_eq!(&sets[0], &sets[2]);
    }

    // Sanitized free text (build_match_query) is implicit AND: two terms
    // match the intersection of their single-term sets, from arbitrary
    // user input.
    #[test]
    fn sanitized_query_is_intersection_of_terms(
        batch in fixtures::batch(30),
        a in any_word_strategy(),
        b in any_word_strategy(),
    ) {
        let storage = Storage::open_in_memory().unwrap();
        for msg in &batch {
            storage.insert_message(msg).unwrap();
        }
        let model = fixtures::first_write_model(&batch);

        let set_a = hit_ids(&storage, &fixtures::quoted_term(&a), limit_for(model.len()));
        let set_b = hit_ids(&storage, &fixtures::quoted_term(&b), limit_for(model.len()));
        let expected: BTreeSet<_> = set_a.intersection(&set_b).cloned().collect();

        let query = build_match_query(&format!("{a} {b}"));
        let hits = hit_ids(&storage, &query, limit_for(model.len()));
        prop_assert_eq!(hits, expected, "query: {}", query);
    }

    // build_match_query output never produces an FTS5 syntax error, for
    // arbitrary (hostile) user input — the sanitization contract.
    #[test]
    fn sanitized_queries_never_error(input in any::<String>()) {
        let storage = Storage::open_in_memory().unwrap();
        storage
            .insert_message(&fixtures::golden_batch(1, 8).remove(0))
            .unwrap();
        let query = build_match_query(&input);
        prop_assume!(!query.is_empty()); // whitespace-only: caller skips
        let result = storage.search(&query, 10);
        prop_assert!(
            result.is_ok(),
            "sanitized query must not error; input={:?} query={:?}",
            input, query
        );
    }
}

/// Any vocabulary word in its literal spelling (used as query terms).
fn any_word_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => select(ASCII_WORDS).prop_map(String::from),
        3 => select(ACCENT_WORDS).prop_map(|(w, _)| w.to_string()),
        2 => select(fixtures::CYRILLIC_WORDS).prop_map(String::from),
        1 => select(fixtures::CJK_WORDS).prop_map(String::from),
    ]
}

/// Golden pinned seed: for every vocabulary word, term and prefix queries
/// agree exactly with the oracle on a fixed deterministic corpus.
#[test]
fn golden_seed_fts_oracle_agreement() {
    let batch = fixtures::golden_batch(0xF75_5EED, 64);
    let storage = Storage::open_in_memory().unwrap();
    for msg in &batch {
        storage.insert_message(msg).unwrap();
    }
    let model = fixtures::first_write_model(&batch);
    let limit = limit_for(model.len());

    let mut words: Vec<String> = ASCII_WORDS.iter().map(|s| s.to_string()).collect();
    words.extend(ACCENT_WORDS.iter().map(|(w, _)| w.to_string()));
    words.extend(fixtures::CYRILLIC_WORDS.iter().map(|s| s.to_string()));
    words.extend(fixtures::CJK_WORDS.iter().map(|s| s.to_string()));

    for word in &words {
        let expected: BTreeSet<_> = model
            .values()
            .filter(|m| fixtures::term_matches(m, word))
            .map(|m| (m.source.clone(), m.external_id.clone()))
            .collect();
        let hits = hit_ids(&storage, &fixtures::quoted_term(word), limit);
        assert_eq!(hits, expected, "term: {word}");

        let prefix: String = word.chars().take(2).collect();
        let expected: BTreeSet<_> = model
            .values()
            .filter(|m| fixtures::prefix_matches(m, &prefix))
            .map(|m| (m.source.clone(), m.external_id.clone()))
            .collect();
        let hits = hit_ids(&storage, &fixtures::quoted_prefix(&prefix), limit);
        assert_eq!(hits, expected, "prefix: {prefix}*");
    }
}

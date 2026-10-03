// Copyright 2026 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Integration tests for the in-memory multi-pattern `glob::PatternFilter`.

use glob::{MatchOptions, PatternFilter};
use std::convert::Infallible;

fn pattern_strings<'a>(filter: &'a PatternFilter, hits: &[glob::MatchedPath]) -> Vec<Vec<&'a str>> {
    hits.iter()
        .map(|hit| hit.source_patterns(filter))
        .collect()
}

// Acceptance: `src/a.rs` hits both `src/*` and `**/*.rs`, yields one result
// with two sources, and repeated inputs never produce duplicate outputs.
#[test]
fn acceptance_multi_pattern_hit_and_dedup() {
    let patterns = ["src/*", "**/*.rs"];
    let filter = PatternFilter::new(patterns).unwrap();

    let inputs = [
        "src/a.rs",
        "src/a.rs",
        "src/b.rs",
        "docs/README.md",
        "src/a.rs",
    ];
    let hits = filter.filter(inputs);

    assert_eq!(hits.len(), 2);

    assert_eq!(hits[0].path(), "src/a.rs");
    assert_eq!(hits[0].source_indices(), &[0, 1]);
    assert_eq!(
        hits[0].source_patterns(&filter),
        vec!["src/*", "**/*.rs"]
    );

    assert_eq!(hits[1].path(), "src/b.rs");
    assert_eq!(hits[1].source_indices(), &[0, 1]);
}

// Acceptance: matching is case sensitive by default, so `A.rs` and `a.rs`
// stay separate.
#[test]
fn acceptance_case_sensitive_by_default() {
    let filter = PatternFilter::new(["*.rs"]).unwrap();
    let hits = filter.filter(["A.rs", "a.rs"]);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].path(), "A.rs");
    assert_eq!(hits[1].path(), "a.rs");

    // An explicit option can opt out of case sensitivity.
    let ci = PatternFilter::with_options(
        ["*.rs"],
        MatchOptions {
            case_sensitive: false,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        },
    )
    .unwrap();
    let hits = ci.filter(["A.rs", "a.rs"]);
    assert_eq!(hits.len(), 2); // identity still uses the original strings
    assert!(hits.iter().all(|hit| hit.source_indices() == [0]));
}

// Separators are significant by default: `*` does not cross `/`.
#[test]
fn separator_significant_by_default() {
    let filter = PatternFilter::new(["src/*"]).unwrap();
    assert!(filter.matches("src/a.rs"));
    assert!(!filter.matches("src/nested/a.rs"));
    assert!(!filter.matches("other/a.rs"));
}

// Acceptance: when normalization equates distinct original strings, the
// merged result unions the sources of all merged originals rather than
// keeping only the first one's, and displays the first original string.
#[test]
fn acceptance_normalized_merge_unions_all_sources() {
    // "FOObar" matches only pattern 0; "foobar" matches only pattern 1.
    let filter = PatternFilter::new(["FOO*", "*bar"]).unwrap();
    let normalize = |s: &str| Ok::<_, Infallible>(s.to_ascii_lowercase());

    // Order where the later original contributes the pattern the first one
    // does not.
    let hits = filter
        .filter_normalized(["FOObar", "foobar"], normalize)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path(), "FOObar");
    assert_eq!(hits[0].source_indices(), &[0, 1]);
    assert_eq!(
        pattern_strings(&filter, &hits),
        vec![vec!["FOO*", "*bar"]]
    );

    // The reversed input order merges the same two sources and keeps the
    // other original for display.
    let hits = filter
        .filter_normalized(["foobar", "FOObar"], normalize)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path(), "foobar");
    assert_eq!(hits[0].source_indices(), &[0, 1]);
}

// Identity separators in inputs never collapse without a normalizer, but
// with one they may merge; sources follow declaration order even when
// contributed by different originals.
#[test]
fn normalizer_only_changes_dedup_identity() {
    // Without normalization, case folds differ.
    let filter = PatternFilter::new(["*.rs", "*.RS"]).unwrap();
    assert_eq!(filter.filter(["a.rs", "a.RS"]).len(), 2);

    // With case-folding normalization, one merged group whose sources are
    // listed in declaration order: [0, 1].
    let hits = filter
        .filter_normalized(["a.rs", "a.RS"], |s| {
            Ok::<_, Infallible>(s.to_ascii_lowercase())
        })
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path(), "a.rs");
    assert_eq!(hits[0].source_indices(), &[0, 1]);
}

// Stable ordering: results follow the first-seen order of each identity.
#[test]
fn stable_first_seen_ordering() {
    let filter = PatternFilter::new(["*"]).unwrap();
    let inputs = ["z", "y", "x", "y", "z", "w"];
    let hits = filter.filter(inputs);
    let paths: Vec<&str> = hits.iter().map(|h| h.path()).collect();
    assert_eq!(paths, vec!["z", "y", "x", "w"]);
}

// Error tests: pattern compilation errors identify the offending pattern.
#[test]
fn invalid_pattern_error_reports_index_and_pattern() {
    let error = PatternFilter::new(["good", "a[", "worse"]).unwrap_err();
    assert_eq!(error.index, 1);
    assert_eq!(error.pattern, "a[");
    assert_eq!(
        error.to_string(),
        "failed to compile pattern \"a[\" at index 1: Pattern syntax error near position 1: \
         invalid range pattern"
    );
}

// Error tests: a normalizer failure rejects the entire batch, with no
// partial results, and identifies the failing input.
#[test]
fn normalizer_failure_rejects_whole_batch_without_partial_results() {
    let filter = PatternFilter::new(["*"]).unwrap();

    let result = filter.filter_normalized(["first", "second", "third"], |s| {
        if s == "second" {
            Err("boom")
        } else {
            Ok(s.to_owned())
        }
    });

    let error = result.unwrap_err();
    assert_eq!(error.index, 1);
    assert_eq!(error.path, "second");
    assert_eq!(error.source, "boom");
    assert_eq!(
        error.to_string(),
        "normalization failed for path \"second\" at index 1: boom"
    );
}

// A custom error type with source chaining works through the normalizer.
#[test]
fn normalizer_error_supports_std_error() {
    #[derive(Debug)]
    struct Boom;

    impl std::fmt::Display for Boom {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "boom")
        }
    }

    impl std::error::Error for Boom {}

    let filter = PatternFilter::new(["*"]).unwrap();
    let error = filter
        .filter_normalized(["ok", "nope"], |s| {
            if s == "nope" {
                Err(Boom)
            } else {
                Ok(s.to_owned())
            }
        })
        .unwrap_err();

    use std::error::Error;
    assert!(error.source().is_some());
}

// Empty-group tests: no patterns, no inputs, or no matches.
#[test]
fn empty_groups() {
    let no_patterns = PatternFilter::new(std::iter::empty::<&str>()).unwrap();
    assert!(no_patterns.filter(["a", "b"]).is_empty());
    assert_eq!(no_patterns.patterns().len(), 0);

    let filter = PatternFilter::new(["*.rs"]).unwrap();
    assert!(filter.filter(std::iter::empty::<&str>()).is_empty());
    assert!(filter.filter(["a.txt", "b.md"]).is_empty());

    // Normalization of a no-match batch still runs but yields nothing.
    let hits = filter
        .filter_normalized(["A.TXT", "b.MD"], |s| Ok::<_, Infallible>(s.to_lowercase()))
        .unwrap();
    assert!(hits.is_empty());
}

// Duplicate patterns are harmless: sources stay unique, merged once per path.
#[test]
fn duplicate_patterns_report_unique_sources() {
    let filter = PatternFilter::new(["*.rs", "*.rs"]).unwrap();
    let hits = filter.filter(["a.rs"]);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source_indices(), &[0, 1]);
}

// Matching a single input directly, outside of a batch.
#[test]
fn direct_source_indices_and_matches() {
    let filter = PatternFilter::new(["src/*", "**/*.rs", "*.md"]).unwrap();
    assert_eq!(filter.source_indices("src/a.rs"), vec![0, 1]);
    assert_eq!(filter.source_indices("notes.md"), vec![2]);
    assert!(filter.source_indices("bin/tool").is_empty());
    assert!(!filter.matches("bin/tool"));
    assert!(filter.matches("notes.md"));
}


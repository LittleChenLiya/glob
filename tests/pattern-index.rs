// Copyright 2026 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Local, deterministic tests for the explicit `PatternIndex` structural
//! index and the baseline-checked literal-segment editor.

use glob::{CharClassItem, Pattern, PatternSegmentKind, SegmentEditError};

#[test]
fn accepts_pattern_a_star_class_b_question_classification() {
    let source = r"a\*/[x*]/b?";
    let pattern = Pattern::new(source).unwrap();
    let index = pattern.index();

    assert_eq!(index.baseline(), source);

    let kinds: Vec<_> = index.segments().iter().map(|s| s.kind().clone()).collect();
    assert_eq!(
        kinds,
        vec![
            PatternSegmentKind::Literal("a".to_string()),
            PatternSegmentKind::Escaped('*'),
            PatternSegmentKind::Literal("/".to_string()),
            PatternSegmentKind::CharClass {
                negated: false,
                items: vec![CharClassItem::Char('x'), CharClassItem::Char('*')],
            },
            PatternSegmentKind::Literal("/b".to_string()),
            PatternSegmentKind::Question,
        ]
    );

    // ranges point back at the exact source slices, in UTF-8 bytes
    let slices: Vec<&str> = index
        .segments()
        .iter()
        .map(|s| &source[s.range()])
        .collect();
    assert_eq!(slices, vec!["a", r"\*", "/", "[x*]", "/b", "?"]);
}

#[test]
fn editing_leading_literal_keeps_escaped_star_literal() {
    let source = r"a\*/[x*]/b?";
    let pattern = Pattern::new(source).unwrap();
    let index = pattern.index();

    let edited = pattern.replace_literal_segment(&index, 0, "alpha").unwrap();

    assert_eq!(edited.as_str(), r"alpha\*/[x*]/b?");

    // escaped star matches only the literal character '*'
    assert!(edited.matches(r"alpha*/x/bZ"));
    assert!(!edited.matches(r"alphax/x/bZ"));

    // star inside the class is not a wildcard either
    assert!(edited.matches(r"alpha*/*/bZ"));
    assert!(!edited.matches(r"alpha*/xx/bZ"));

    // the non-target segments kept their exact kinds
    let new_kinds: Vec<_> = edited.segments().iter().map(|s| s.kind()).collect();
    assert_eq!(*new_kinds[1], PatternSegmentKind::Escaped('*'));
    assert!(matches!(
        *new_kinds[3],
        PatternSegmentKind::CharClass { .. }
    ));
    assert_eq!(*new_kinds[5], PatternSegmentKind::Question);
}

#[test]
fn stale_index_is_rejected_after_baseline_changes() {
    let source = r"a\*/[x*]/b?";
    let index = Pattern::new(source).unwrap().index();

    // the baseline source text changed
    let changed = Pattern::new("a").unwrap();
    assert!(matches!(
        changed.replace_literal_segment(&index, 0, "z"),
        Err(SegmentEditError::BaselineMismatch)
    ));

    // same source text, parsed structure cannot diverge for identical
    // strings, but the check must still compare structure: build a pattern
    // with the same length-looking but different text and ensure refusal
    let other = Pattern::new("a/b/[x*]?").unwrap();
    assert!(matches!(
        other.replace_literal_segment(&index, 0, "z"),
        Err(SegmentEditError::BaselineMismatch)
    ));
}

#[test]
fn index_segments_tile_source_for_varied_patterns() {
    for source in [
        "",
        "abc",
        "a*b",
        "a/**/b",
        "**",
        "a/**",
        "**/b",
        "[!a-z][0-9_]",
        r"\?\*\[\]\\",
        "q?r*s",
    ] {
        let pattern = Pattern::new(source).unwrap();
        let segments = pattern.segments();
        if source.is_empty() {
            assert!(segments.is_empty());
            continue;
        }
        assert_eq!(segments.first().unwrap().range().start, 0, "{source}");
        assert_eq!(
            segments.last().unwrap().range().end,
            source.len(),
            "{source}"
        );
        for pair in segments.windows(2) {
            assert_eq!(pair[0].range().end, pair[1].range().start, "{source}");
        }
    }
}

#[test]
fn double_star_segments_keep_separator_literal() {
    let pattern = Pattern::new("a/**/b").unwrap();
    let kinds: Vec<_> = pattern.segments().iter().map(|s| s.kind()).collect();
    assert_eq!(*kinds[0], PatternSegmentKind::Literal("a/".to_string()));
    assert_eq!(*kinds[1], PatternSegmentKind::DoubleStar);
    assert_eq!(*kinds[2], PatternSegmentKind::Literal("/b".to_string()));
}

#[test]
fn unicode_byte_offsets() {
    // one 3-byte char, one ASCII star, one 4-byte emoji
    let source = "\u{4e2d}*\u{1f600}";
    let pattern = Pattern::new(source).unwrap();
    let segments = pattern.segments();

    assert_eq!(segments[0].range(), 0..3);
    assert_eq!(&source[segments[0].range()], "\u{4e2d}");
    assert_eq!(*segments[1].kind(), PatternSegmentKind::Star);
    assert_eq!(segments[1].range(), 3..4);
    assert_eq!(segments[2].range(), 4..8);
    assert_eq!(&source[segments[2].range()], "\u{1f600}");
}

#[test]
fn edit_example_shift_later_ranges_only() {
    let pattern = Pattern::new("ab/*/cd").unwrap();
    let index = pattern.index();
    assert_eq!(index.len(), 3); // "ab/", Star, "/cd"

    let edited = pattern
        .replace_literal_segment(&index, 0, "xxxxx/")
        .unwrap();
    assert_eq!(edited.as_str(), "xxxxx/*/cd");
    // the star and following literal only shifted byte ranges
    assert_eq!(edited.segments()[1].range(), 6..7);
    assert_eq!(edited.segments()[2].range(), 7..10);
    assert_eq!(
        *edited.segments()[2].kind(),
        PatternSegmentKind::Literal("/cd".to_string())
    );
}

#[test]
fn wildcard_in_replacement_is_rejected() {
    let pattern = Pattern::new("ab").unwrap();
    let index = pattern.index();
    assert!(matches!(
        pattern.replace_literal_segment(&index, 0, "x*y"),
        Err(SegmentEditError::ReplacementNotLiteral { .. })
    ));
    assert!(matches!(
        pattern.replace_literal_segment(&index, 0, "[x]"),
        Err(SegmentEditError::ReplacementNotLiteral { .. })
    ));
}

#[test]
fn editing_non_literal_segment_is_rejected() {
    let pattern = Pattern::new(r"a\*b").unwrap();
    let index = pattern.index();
    assert!(matches!(
        pattern.replace_literal_segment(&index, 1, "x"),
        Err(SegmentEditError::NotALiteralSegment { index: 1 })
    ));
}

#[test]
fn consecutive_double_stars_are_separate_segments_but_collapsed_token() {
    // Tokens collapse consecutive `**` for matching, but the structural
    // index faithfully describes the source: both `**` runs are present.
    let pattern = Pattern::new("some/**/**/needle.txt").unwrap();
    let kinds: Vec<_> = pattern.segments().iter().map(|s| s.kind()).collect();
    assert!(
        kinds
            .iter()
            .filter(|k| ***k == PatternSegmentKind::DoubleStar)
            .count()
            == 2
    );
    // matching behaviour is unchanged
    assert!(pattern.matches("some/needle.txt"));
    assert!(pattern.matches("some/one/two/needle.txt"));
}

#[test]
fn editing_keeps_original_pattern_unchanged() {
    let pattern = Pattern::new("ab/cd").unwrap();
    let index = pattern.index();
    let _ = pattern.replace_literal_segment(&index, 0, "x").unwrap();
    assert_eq!(pattern.as_str(), "ab/cd");
    assert_eq!(pattern.index(), index);
}

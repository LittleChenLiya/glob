// Copyright 2024 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Integration tests for the path remapping planner.

use glob::remap::{RemapError, RemapPlanner, RemapRule, UnsafeReason};
use glob::MatchOptions;
use std::path::PathBuf;

fn rule(source: &str, target: &str) -> RemapRule {
    RemapRule {
        source: source.to_string(),
        target: target.to_string(),
    }
}

fn src(paths: &[&str]) -> Vec<PathBuf> {
    paths.iter().map(PathBuf::from).collect()
}

// The headline acceptance scenario: map `src/{dir:**}/{name:*}.rs` to
// `lib/{dir}/{name}.txt`, including nested directories and an empty `dir`
// capture.
#[test]
fn maps_nested_directories() {
    let planner = RemapPlanner::new(vec![rule(
        "src/{dir:**}/{name:*}.rs",
        "lib/{dir}/{name}.txt",
    )])
    .unwrap();

    let mappings = planner
        .plan(&src(&[
            "src/top.rs",
            "src/a/b/c.rs",
            "src/x/y/z/mod.rs",
            "src/a/b/deep/nested/file.rs",
        ]))
        .unwrap();

    assert_eq!(mappings.len(), 4);

    assert_eq!(mappings[0].source, PathBuf::from("src/top.rs"));
    assert_eq!(mappings[0].target, PathBuf::from("lib/top.txt"));
    assert_eq!(mappings[0].rule, 0);

    assert_eq!(mappings[1].source, PathBuf::from("src/a/b/c.rs"));
    assert_eq!(mappings[1].target, PathBuf::from("lib/a/b/c.txt"));

    assert_eq!(mappings[2].source, PathBuf::from("src/x/y/z/mod.rs"));
    assert_eq!(mappings[2].target, PathBuf::from("lib/x/y/z/mod.txt"));

    assert_eq!(
        mappings[3].source,
        PathBuf::from("src/a/b/deep/nested/file.rs")
    );
    assert_eq!(
        mappings[3].target,
        PathBuf::from("lib/a/b/deep/nested/file.txt")
    );
}

// Two distinct sources that would map to the same target are rejected.
#[test]
fn rejects_duplicate_targets() {
    let planner = RemapPlanner::new(vec![
        rule("src/{dir:**}/{name:*}.rs", "lib/{dir}/{name}.txt"),
        rule("alt/{name:*}.rs", "lib/{name}.txt"),
    ])
    .unwrap();

    let err = planner
        .plan(&src(&["src/foo.rs", "alt/foo.rs"]))
        .unwrap_err();

    match err {
        RemapError::DuplicateTarget {
            target,
            first,
            second,
        } => {
            assert_eq!(target, PathBuf::from("lib/foo.txt"));
            assert_eq!(first, PathBuf::from("src/foo.rs"));
            assert_eq!(second, PathBuf::from("alt/foo.rs"));
        }
        other => panic!("expected DuplicateTarget, got {:?}", other),
    }
}

// A target that escapes the root through `..` is rejected.
#[test]
fn rejects_parent_dir_escape() {
    let planner = RemapPlanner::new(vec![rule("src/{name:*}.rs", "lib/../{name}.txt")]).unwrap();

    let err = planner.plan(&src(&["src/foo.rs"])).unwrap_err();
    match err {
        RemapError::UnsafeTarget { reason, .. } => {
            assert_eq!(reason, UnsafeReason::ParentDir);
        }
        other => panic!("expected UnsafeTarget, got {:?}", other),
    }
}

// An absolute target is rejected.
#[test]
fn rejects_absolute_target() {
    let planner = RemapPlanner::new(vec![rule("src/{name:*}.rs", "/{name}.txt")]).unwrap();

    let err = planner.plan(&src(&["src/foo.rs"])).unwrap_err();
    match err {
        RemapError::UnsafeTarget { reason, .. } => {
            assert_eq!(reason, UnsafeReason::Absolute);
        }
        other => panic!("expected UnsafeTarget, got {:?}", other),
    }
}

// A capture value that carries a reserved character is rejected.
#[test]
fn rejects_reserved_char_in_capture() {
    let planner = RemapPlanner::new(vec![rule("src/{name:*}.rs", "lib/{name}.txt")]).unwrap();

    let err = planner.plan(&src(&["src/a:b.rs"])).unwrap_err();
    match err {
        RemapError::UnsafeTarget { reason, .. } => {
            assert_eq!(reason, UnsafeReason::ReservedChar(':'));
        }
        other => panic!("expected UnsafeTarget, got {:?}", other),
    }

    // `?` and `*` are also reserved.
    let err = planner.plan(&src(&["src/a?b.rs"])).unwrap_err();
    match err {
        RemapError::UnsafeTarget { reason, .. } => {
            assert_eq!(reason, UnsafeReason::ReservedChar('?'));
        }
        other => panic!("expected UnsafeTarget, got {:?}", other),
    }
}

// An empty target (from an empty capture) is rejected.
#[test]
fn rejects_empty_target() {
    let planner = RemapPlanner::new(vec![rule("src/{dir:**}/{name:*}.rs", "{dir}")]).unwrap();

    let err = planner.plan(&src(&["src/top.rs"])).unwrap_err();
    match err {
        RemapError::UnsafeTarget { reason, .. } => {
            assert_eq!(reason, UnsafeReason::Empty);
        }
        other => panic!("expected UnsafeTarget, got {:?}", other),
    }
}

// A target that ends in a dot or space is rejected (Windows quirk).
#[test]
fn rejects_trailing_dot_or_space() {
    let planner = RemapPlanner::new(vec![rule("src/{name:*}.rs", "lib/{name}.")]).unwrap();

    let err = planner.plan(&src(&["src/foo.rs"])).unwrap_err();
    match err {
        RemapError::UnsafeTarget { reason, .. } => {
            assert_eq!(reason, UnsafeReason::TrailingDotOrSpace);
        }
        other => panic!("expected UnsafeTarget, got {:?}", other),
    }
}

// Empty captures are allowed (a `**` capture can match zero segments).
#[test]
fn empty_capture_is_allowed() {
    let planner = RemapPlanner::new(vec![rule(
        "src/{dir:**}/{name:*}.rs",
        "lib/{dir}/{name}.txt",
    )])
    .unwrap();

    let mappings = planner.plan(&src(&["src/top.rs"])).unwrap();
    assert_eq!(mappings.len(), 1);
    assert_eq!(mappings[0].target, PathBuf::from("lib/top.txt"));
}

// Windows case mode: matching is case-insensitive.
#[test]
fn windows_case_mode() {
    let case_insensitive = MatchOptions {
        case_sensitive: false,
        ..MatchOptions::new()
    };
    let planner = RemapPlanner::with_options(
        vec![rule("src/{dir:**}/{name:*}.rs", "lib/{dir}/{name}.txt")],
        case_insensitive,
    )
    .unwrap();

    let mappings = planner.plan(&src(&["SRC/FOO.RS"])).unwrap();
    assert_eq!(mappings.len(), 1);
    assert_eq!(mappings[0].target, PathBuf::from("lib/FOO.txt"));

    // Case-sensitive mode rejects the mixed-case source.
    let case_sensitive = MatchOptions {
        case_sensitive: true,
        ..MatchOptions::new()
    };
    let planner = RemapPlanner::with_options(
        vec![rule("src/{dir:**}/{name:*}.rs", "lib/{dir}/{name}.txt")],
        case_sensitive,
    )
    .unwrap();

    let mappings = planner.plan(&src(&["SRC/FOO.RS"])).unwrap();
    assert!(mappings.is_empty());
}

// A target template that references an undefined capture is an error at
// construction time.
#[test]
fn undefined_capture_is_error() {
    let err = RemapPlanner::new(vec![rule(
        "src/{dir:**}/{name:*}.rs",
        "lib/{dir}/{other}.txt",
    )])
    .unwrap_err();

    match err {
        RemapError::UndefinedCapture { rule, name } => {
            assert_eq!(rule, 0);
            assert_eq!(name, "other");
        }
        other => panic!("expected UndefinedCapture, got {:?}", other),
    }
}

// A malformed target template is an error at construction time.
#[test]
fn malformed_template_is_error() {
    let err = RemapPlanner::new(vec![rule(
        "src/{name:*}.rs",
        "lib/{name.txt",
    )])
    .unwrap_err();

    match err {
        RemapError::InvalidTemplate { rule, .. } => assert_eq!(rule, 0),
        other => panic!("expected InvalidTemplate, got {:?}", other),
    }
}

// A malformed source pattern is an error at construction time.
#[test]
fn malformed_source_is_error() {
    let err = RemapPlanner::new(vec![rule("src/{name:**}.rs", "lib/{name}.txt")]).unwrap_err();

    match err {
        RemapError::InvalidPattern(_) => {}
        other => panic!("expected InvalidPattern, got {:?}", other),
    }
}

// Single-segment captures within a segment.
#[test]
fn single_segment_captures() {
    let planner = RemapPlanner::new(vec![rule(
        "src/{name:*}.rs",
        "lib/{name}.txt",
    )])
    .unwrap();

    let mappings = planner
        .plan(&src(&["src/foo.rs", "src/bar.rs", "src/a/baz.rs"]))
        .unwrap();

    // `src/a/baz.rs` does not match because `*` is single-segment.
    assert_eq!(mappings.len(), 2);
    assert_eq!(mappings[0].target, PathBuf::from("lib/foo.txt"));
    assert_eq!(mappings[1].target, PathBuf::from("lib/bar.txt"));
}

// Multiple single-segment captures in one segment.
#[test]
fn multiple_single_captures() {
    let planner = RemapPlanner::new(vec![rule(
        "src/{a:*}-{b:*}.rs",
        "lib/{b}/{a}.txt",
    )])
    .unwrap();

    let mappings = planner.plan(&src(&["src/x-y.rs"])).unwrap();
    assert_eq!(mappings.len(), 1);
    assert_eq!(mappings[0].target, PathBuf::from("lib/y/x.txt"));
}

// Rules are tried in declaration order; the first match wins.
#[test]
fn first_match_wins() {
    let planner = RemapPlanner::new(vec![
        rule("src/{name:*}.rs", "first/{name}.txt"),
        rule("src/{dir:**}/{name:*}.rs", "second/{dir}/{name}.txt"),
    ])
    .unwrap();

    let mappings = planner
        .plan(&src(&["src/foo.rs", "src/a/bar.rs"]))
        .unwrap();

    assert_eq!(mappings.len(), 2);
    // `src/foo.rs` matches the first rule.
    assert_eq!(mappings[0].target, PathBuf::from("first/foo.txt"));
    assert_eq!(mappings[0].rule, 0);
    // `src/a/bar.rs` only matches the second rule.
    assert_eq!(mappings[1].target, PathBuf::from("second/a/bar.txt"));
    assert_eq!(mappings[1].rule, 1);
}

// `**` captures are greedy (longest cross-segment span wins).
#[test]
fn recursive_captures_are_greedy() {
    let planner = RemapPlanner::new(vec![rule(
        "src/{a:**}/{b:**}",
        "out/{a}__{b}",
    )])
    .unwrap();

    let mappings = planner.plan(&src(&["src/x/y/z"])).unwrap();
    assert_eq!(mappings.len(), 1);
    // The leftmost `**` takes the longest possible span: a=`x/y/z`, b=``.
    assert_eq!(mappings[0].target, PathBuf::from("out/x/y/z__"));
}

// A `**` capture backtracks when a later literal cannot match.
#[test]
fn recursive_capture_backtracks() {
    let planner = RemapPlanner::new(vec![rule(
        "src/{a:**}/mid/{b:**}",
        "out/{a}__{b}",
    )])
    .unwrap();

    let mappings = planner.plan(&src(&["src/x/y/mid/z"])).unwrap();
    assert_eq!(mappings.len(), 1);
    // a=`x/y`, b=`z` (the greedy a=`x/y/mid` fails because `mid` is consumed).
    assert_eq!(mappings[0].target, PathBuf::from("out/x/y__z"));
}

// Unmatched sources are skipped.
#[test]
fn unmatched_sources_are_skipped() {
    let planner = RemapPlanner::new(vec![rule(
        "src/{name:*}.rs",
        "lib/{name}.txt",
    )])
    .unwrap();

    let mappings = planner
        .plan(&src(&["src/foo.rs", "other/bar.rs", "src/baz.txt"]))
        .unwrap();

    assert_eq!(mappings.len(), 1);
    assert_eq!(mappings[0].target, PathBuf::from("lib/foo.txt"));
}

// An absolute source is rejected.
#[test]
fn rejects_absolute_source() {
    let planner = RemapPlanner::new(vec![rule(
        "src/{name:*}.rs",
        "lib/{name}.txt",
    )])
    .unwrap();

    let err = planner
        .plan(&src(&["src/foo.rs", "/abs/bar.rs"]))
        .unwrap_err();

    match err {
        RemapError::AbsoluteSource(p) => assert_eq!(p, PathBuf::from("/abs/bar.rs")),
        other => panic!("expected AbsoluteSource, got {:?}", other),
    }
}

// Literal glob metacharacters in the source pattern still work.
#[test]
fn literal_glob_metacharacters() {
    let planner = RemapPlanner::new(vec![rule(
        "src/[ab]{name:*}.rs",
        "lib/{name}.txt",
    )])
    .unwrap();

    let mappings = planner
        .plan(&src(&["src/aone.rs", "src/btwo.rs", "src/cthree.rs"]))
        .unwrap();

    assert_eq!(mappings.len(), 2);
    assert_eq!(mappings[0].target, PathBuf::from("lib/one.txt"));
    assert_eq!(mappings[1].target, PathBuf::from("lib/two.txt"));
}

//! Acceptance tests for the read-only finite-candidate diff checker.
//!
//! Run with: cargo test --test diff

use glob::{diff_match_options, diff_patterns, DiffReason, MatchOptions};

fn opts(case_sensitive: bool, literal_sep: bool, literal_dot: bool) -> MatchOptions {
    MatchOptions {
        case_sensitive,
        require_literal_separator: literal_sep,
        require_literal_leading_dot: literal_dot,
    }
}

#[test]
fn case_option_distinguishes_upper_and_lower_candidates() {
    // Candidates `A.txt` and `a.txt` must show an accurate difference when
    // only the case_sensitive option changes.
    let strict = opts(true, false, false);
    let relaxed = opts(false, false, false);
    let candidates = ["A.txt", "a.txt"];

    let diff = diff_match_options("a.txt", strict, relaxed, &candidates).unwrap();

    assert_eq!(diff.candidate_count, 2);
    assert_eq!(diff.witnesses.len(), 1);
    let w = &diff.witnesses[0];
    assert_eq!(w.index, 0);
    assert_eq!(w.path, "A.txt");
    assert!(!w.matched_by_a); // strict case: no match
    assert!(w.matched_by_b); // relaxed case: match
    assert_eq!(w.reasons, vec![DiffReason::CaseSensitivity]);
    // `a.txt` matches under both, so it is not a witness.
    assert!(!diff.is_equivalent_on_candidates());
}

#[test]
fn dot_file_rule_is_explained_separately() {
    // Toggling require_literal_leading_dot must be reported as its own
    // reason, not conflated with case or separator handling.
    let dot_allowed = opts(true, false, false);
    let dot_literal = opts(true, false, true);
    let candidates = [".hidden", "visible"];

    let diff = diff_match_options("*", dot_allowed, dot_literal, &candidates).unwrap();

    assert_eq!(diff.witnesses.len(), 1);
    let w = &diff.witnesses[0];
    assert_eq!(w.path, ".hidden");
    assert!(w.matched_by_a);
    assert!(!w.matched_by_b);
    assert_eq!(w.reasons, vec![DiffReason::LiteralLeadingDot]);
    assert_ne!(w.reasons, vec![DiffReason::CaseSensitivity]);
}

#[test]
fn separator_rule_is_explained_separately() {
    let cross_ok = opts(true, false, false);
    let cross_no = opts(true, true, false);
    let candidates = ["a/b", "ab"];

    let diff = diff_match_options("a*b", cross_ok, cross_no, &candidates).unwrap();

    assert_eq!(diff.witnesses.len(), 1);
    let w = &diff.witnesses[0];
    assert_eq!(w.path, "a/b");
    assert_eq!(w.reasons, vec![DiffReason::LiteralSeparator]);
}

#[test]
fn duplicate_paths_keep_their_own_identity() {
    // The same path appearing twice must yield two witnesses, each keeping
    // its original index and verbatim path.
    let strict = opts(true, false, false);
    let relaxed = opts(false, false, false);
    let candidates = ["A.txt", "a.txt", "A.txt"];

    let diff = diff_match_options("a.txt", strict, relaxed, &candidates).unwrap();

    assert_eq!(diff.witnesses.len(), 2);
    assert_eq!(diff.witnesses[0].index, 0);
    assert_eq!(diff.witnesses[0].path, "A.txt");
    assert_eq!(diff.witnesses[1].index, 2);
    assert_eq!(diff.witnesses[1].path, "A.txt");
}

#[test]
fn invalid_patterns_are_rejected_before_any_checking() {
    // An invalid pattern on either side must fail the whole check; no
    // partial diff may be produced even if the other side is valid.
    let strict = opts(true, false, false);
    let relaxed = opts(false, false, false);
    let candidates = ["A.txt", "a.txt"];

    // Same-pattern helper with a bad pattern.
    assert!(diff_match_options("a/**b", strict, relaxed, &candidates).is_err());
    assert!(diff_match_options("[", strict, relaxed, &candidates).is_err());

    // Two-pattern form: bad pattern on side A.
    let err_a = diff_patterns("a/**b", strict, "a.txt", relaxed, &candidates);
    assert!(err_a.is_err());

    // Two-pattern form: bad pattern on side B.
    let err_b = diff_patterns("a.txt", strict, "***", relaxed, &candidates);
    assert!(err_b.is_err());

    // Both bad: still a single uniform error, never half a diff.
    assert!(diff_patterns("[", strict, "]", relaxed, &candidates).is_err());
}

#[test]
fn finiteness_guarantee_is_recorded_in_the_result() {
    let strict = opts(true, false, false);
    let relaxed = opts(false, false, false);
    let candidates = ["a.txt"];

    // No witnesses on these candidates, yet the result must NOT claim the
    // configurations are equivalent in general.
    let diff = diff_match_options("a.txt", strict, relaxed, &candidates).unwrap();
    assert!(diff.is_equivalent_on_candidates());
    assert!(diff.finiteness_note.contains("finite-check guarantee"));
    assert!(diff.finiteness_note.contains("1 supplied candidate"));
    assert!(diff.finiteness_note.contains("does NOT prove"));
    assert!(diff.finiteness_note.contains("有限性保证"));
    assert!(diff.finiteness_note.contains("不能凭有限候选宣称两个模式完全等价"));
}

#[test]
fn three_path_example_end_to_end() {
    // The documented three-path example: duplicates preserved, finiteness
    // note attached, read-only (no filesystem touched).
    let case_on = opts(true, false, false);
    let case_off = opts(false, false, false);
    let candidates = ["A.txt", "a.txt", "A.txt"];

    let diff = diff_match_options("a.txt", case_on, case_off, &candidates).unwrap();

    assert_eq!(diff.candidate_count, 3);
    assert_eq!(diff.witnesses.len(), 2);
    assert_eq!(diff.witnesses[0].index, 0);
    assert_eq!(diff.witnesses[1].index, 2);
    assert!(!diff.is_equivalent_on_candidates());
    assert!(diff.finiteness_note.contains("3 supplied candidate"));
    assert!(diff.finiteness_note.contains("2 witness"));
}

#[test]
fn two_patterns_can_be_compared_without_claiming_equivalence() {
    let o = opts(true, false, false);
    let candidates = ["a.txt", "b.txt"];

    let diff = diff_patterns("a.txt", o, "b.txt", o, &candidates).unwrap();
    assert_eq!(diff.witnesses.len(), 2);
    assert_eq!(diff.pattern_a, "a.txt");
    assert_eq!(diff.pattern_b, "b.txt");

    // Same pattern, same options: no witnesses, but still only a finite
    // guarantee.
    let same = diff_patterns("a.txt", o, "a.txt", o, &candidates).unwrap();
    assert!(same.is_equivalent_on_candidates());
    assert!(same.finiteness_note.contains("does NOT prove"));
}

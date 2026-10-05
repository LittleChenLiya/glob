// Copyright 2026 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Read-only diff checker between two glob match configurations over a
//! finite set of candidate paths.
//!
//! This module compares two [`MatchSpec`]s (a compiled [`Pattern`] paired
//! with explicit [`MatchOptions`]) against a finite, caller-supplied list
//! of candidate path strings. It reports each *witness*: a path for which
//! the two sides disagree, together with an explanation of which option
//! field (case, separator, or leading dot) causes the disagreement.
//!
//! # Finite guarantee
//!
//! The diff is **finite** by construction: it only inspects the exact
//! candidate paths supplied by the caller. A result with no witnesses does
//! **not** prove that the two patterns are equivalent for all possible
//! paths; it only proves they agree on the supplied candidates. The
//! [`GlobDiff::finite`] field is always `true` to make this guarantee
//! explicit in the result.
//!
//! # No filesystem access
//!
//! Matching is purely in-memory. No directory is scanned and no file is
//! read.
//!
//! # Examples
//!
//! A three-path example comparing a case-sensitive and a case-insensitive
//! configuration for the pattern `a.txt`:
//!
//! ```rust
//! use glob::diff::diff;
//! use glob::MatchOptions;
//!
//! let candidates = ["A.txt", "a.txt", "b.txt"];
//! let left = MatchOptions { case_sensitive: true, ..MatchOptions::new() };
//! let right = MatchOptions { case_sensitive: false, ..MatchOptions::new() };
//!
//! let result = diff("a.txt", left, "a.txt", right, &candidates).unwrap();
//!
//! // Only `A.txt` is a witness: the case-sensitive side rejects it while
//! // the case-insensitive side accepts it.
//! assert_eq!(result.witnesses.len(), 1);
//! assert_eq!(result.witnesses[0].candidate.path, "A.txt");
//! assert_eq!(result.witnesses[0].candidate.index, 0);
//! assert_eq!(result.witnesses[0].left, false);
//! assert_eq!(result.witnesses[0].right, true);
//! assert_eq!(result.candidate_count, 3);
//! assert_eq!(result.finite, true);
//! ```
//!
//! Run the runnable example with:
//!
//! ```sh
//! cargo run --example diff_checker
//! ```

use crate::{MatchOptions, Pattern, PatternError};

/// One side of a glob comparison: a compiled pattern together with the
/// explicit [`MatchOptions`] used to evaluate it.
#[derive(Debug, Clone)]
pub struct MatchSpec {
    pattern: Pattern,
    options: MatchOptions,
}

impl MatchSpec {
    /// Compile `pattern` and pair it with `options`.
    ///
    /// Returns a [`PatternError`] if `pattern` is invalid.
    pub fn new(pattern: &str, options: MatchOptions) -> Result<Self, PatternError> {
        let pattern = Pattern::new(pattern)?;
        Ok(MatchSpec { pattern, options })
    }

    /// The compiled pattern.
    pub fn pattern(&self) -> &Pattern {
        &self.pattern
    }

    /// The explicit match options.
    pub fn options(&self) -> MatchOptions {
        self.options
    }
}

/// A single candidate path's identity, preserved through the diff.
///
/// Duplicate candidate strings are kept as distinct identities: each
/// occurrence retains its original [`index`](Candidate::index).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The original path string.
    pub path: String,
    /// The original position in the candidate list.
    pub index: usize,
}

/// Explains why a single candidate matches one side but not the other.
///
/// Each variant corresponds to a specific [`MatchOptions`] field, so the
/// separator, case, and leading-dot rule changes are explained separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Difference {
    /// The disagreement is attributable to the case-sensitivity setting
    /// (`case_sensitive`).
    CaseSensitive,
    /// The disagreement is attributable to the literal-separator setting
    /// (`require_literal_separator`).
    LiteralSeparator,
    /// The disagreement is attributable to the literal-leading-dot setting
    /// (`require_literal_leading_dot`).
    LiteralLeadingDot,
    /// More than one option field contributes; each listed field
    /// contributes.
    Multiple(Vec<Difference>),
    /// The option fields do not explain the disagreement (e.g. the two
    /// patterns themselves differ).
    NotAttributable,
}

/// A witness: a candidate path for which the two sides disagree.
#[derive(Debug, Clone)]
pub struct Witness {
    /// The candidate's original identity.
    pub candidate: Candidate,
    /// Whether the left side matches.
    pub left: bool,
    /// Whether the right side matches.
    pub right: bool,
    /// The explanation for the disagreement.
    pub difference: Difference,
}

/// The result of a read-only glob diff.
///
/// The diff is **finite**: it only inspects the explicitly supplied
/// candidate paths. A witness-free result does **not** prove that the two
/// patterns are equivalent for all possible paths. The [`finite`](GlobDiff::finite)
/// field is always `true` to make this guarantee explicit in the result.
#[derive(Debug, Clone)]
pub struct GlobDiff {
    /// Witnesses where the two sides disagree, in candidate order.
    pub witnesses: Vec<Witness>,
    /// The total number of candidate paths inspected.
    pub candidate_count: usize,
    /// Always `true`: the candidate set is finite by construction.
    pub finite: bool,
}

impl GlobDiff {
    /// Returns `true` if no witnesses were found among the finite candidates.
    ///
    /// This does **not** prove the two patterns are equivalent for all
    /// paths; it only proves they agree on the supplied candidates.
    pub fn is_empty(&self) -> bool {
        self.witnesses.is_empty()
    }

    /// Returns the number of witnesses.
    pub fn len(&self) -> usize {
        self.witnesses.len()
    }
}

/// Read-only diff between two glob match configurations over a finite set
/// of candidate paths.
///
/// Both patterns are compiled first. If either pattern is invalid, a
/// [`PatternError`] is returned and **no** partial diff is produced.
///
/// No filesystem access is performed; matching is purely in-memory.
///
/// # Finite guarantee
///
/// Only the supplied `candidates` are inspected. A witness-free result does
/// not prove that the two patterns are equivalent for all possible paths.
///
/// # Examples
///
/// ```rust
/// use glob::diff::diff;
/// use glob::MatchOptions;
///
/// let candidates = ["A.txt", "a.txt"];
/// let left = MatchOptions { case_sensitive: true, ..MatchOptions::new() };
/// let right = MatchOptions { case_sensitive: false, ..MatchOptions::new() };
/// let result = diff("a.txt", left, "a.txt", right, &candidates).unwrap();
/// assert_eq!(result.witnesses.len(), 1);
/// assert_eq!(result.finite, true);
/// ```
pub fn diff(
    left_pattern: &str,
    left_options: MatchOptions,
    right_pattern: &str,
    right_options: MatchOptions,
    candidates: &[&str],
) -> Result<GlobDiff, PatternError> {
    // Compile both patterns first; reject invalid patterns before any
    // matching is performed, so a partial diff can never be returned.
    let left = MatchSpec::new(left_pattern, left_options)?;
    let right = MatchSpec::new(right_pattern, right_options)?;
    Ok(diff_specs(&left, &right, candidates))
}

/// Read-only diff between two already-compiled [`MatchSpec`]s over a finite
/// set of candidate paths.
///
/// # Finite guarantee
///
/// Only the supplied `candidates` are inspected. A witness-free result does
/// not prove that the two patterns are equivalent for all possible paths.
pub fn diff_specs(left: &MatchSpec, right: &MatchSpec, candidates: &[&str]) -> GlobDiff {
    let mut witnesses = Vec::new();

    for (index, &path) in candidates.iter().enumerate() {
        let l = left.pattern.matches_with(path, left.options);
        let r = right.pattern.matches_with(path, right.options);
        if l != r {
            let difference = explain(left, right, path, l, r);
            witnesses.push(Witness {
                candidate: Candidate {
                    path: path.to_string(),
                    index,
                },
                left: l,
                right: r,
                difference,
            });
        }
    }

    GlobDiff {
        witnesses,
        candidate_count: candidates.len(),
        finite: true,
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Field {
    CaseSensitive,
    LiteralSeparator,
    LiteralLeadingDot,
}

fn field_value(spec: &MatchSpec, field: Field) -> bool {
    match field {
        Field::CaseSensitive => spec.options.case_sensitive,
        Field::LiteralSeparator => spec.options.require_literal_separator,
        Field::LiteralLeadingDot => spec.options.require_literal_leading_dot,
    }
}

fn with_field(options: MatchOptions, field: Field, value: bool) -> MatchOptions {
    let mut opts = options;
    match field {
        Field::CaseSensitive => opts.case_sensitive = value,
        Field::LiteralSeparator => opts.require_literal_separator = value,
        Field::LiteralLeadingDot => opts.require_literal_leading_dot = value,
    }
    opts
}

fn field_to_difference(field: Field) -> Difference {
    match field {
        Field::CaseSensitive => Difference::CaseSensitive,
        Field::LiteralSeparator => Difference::LiteralSeparator,
        Field::LiteralLeadingDot => Difference::LiteralLeadingDot,
    }
}

/// Explain why `path` matches one side but not the other.
///
/// For each differing option field, we flip that field on the left spec
/// (keeping the left pattern and the other options) and check whether the
/// match result changes. We do the same from the right spec. A field that
/// changes the result on either side contributes to the disagreement.
/// If no single field flip changes the result, the disagreement is
/// [`NotAttributable`](Difference::NotAttributable) (e.g. the patterns
/// themselves differ).
fn explain(left: &MatchSpec, right: &MatchSpec, path: &str, l: bool, r: bool) -> Difference {
    let fields = [
        Field::CaseSensitive,
        Field::LiteralSeparator,
        Field::LiteralLeadingDot,
    ];

    let mut contributing: Vec<Field> = Vec::new();

    // Test from the left: flip each differing option field to the right's
    // value and see if the match result changes.
    for &field in &fields {
        let lo = field_value(left, field);
        let ro = field_value(right, field);
        if lo != ro {
            let flipped = left
                .pattern
                .matches_with(path, with_field(left.options, field, ro));
            if flipped != l && !contributing.contains(&field) {
                contributing.push(field);
            }
        }
    }

    // Test from the right: flip each differing option field to the left's
    // value and see if the match result changes.
    for &field in &fields {
        let lo = field_value(left, field);
        let ro = field_value(right, field);
        if lo != ro {
            let flipped = right
                .pattern
                .matches_with(path, with_field(right.options, field, lo));
            if flipped != r && !contributing.contains(&field) {
                contributing.push(field);
            }
        }
    }

    match contributing.len() {
        0 => Difference::NotAttributable,
        1 => field_to_difference(contributing[0]),
        _ => Difference::Multiple(contributing.into_iter().map(field_to_difference).collect()),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn case_sensitive() -> MatchOptions {
        MatchOptions {
            case_sensitive: true,
            ..MatchOptions::new()
        }
    }

    fn case_insensitive() -> MatchOptions {
        MatchOptions {
            case_sensitive: false,
            ..MatchOptions::new()
        }
    }

    fn literal_separator() -> MatchOptions {
        MatchOptions {
            require_literal_separator: true,
            ..MatchOptions::new()
        }
    }

    fn literal_leading_dot() -> MatchOptions {
        MatchOptions {
            require_literal_leading_dot: true,
            ..MatchOptions::new()
        }
    }

    #[test]
    fn test_case_difference() {
        // `A.txt` and `a.txt` show an exact difference under case options.
        let candidates = ["A.txt", "a.txt", "b.txt"];
        let result = diff("a.txt", case_sensitive(), "a.txt", case_insensitive(), &candidates)
            .unwrap();

        assert_eq!(result.candidate_count, 3);
        assert_eq!(result.finite, true);
        assert_eq!(result.witnesses.len(), 1);

        let w = &result.witnesses[0];
        assert_eq!(w.candidate.path, "A.txt");
        assert_eq!(w.candidate.index, 0);
        assert_eq!(w.left, false);
        assert_eq!(w.right, true);
        assert_eq!(w.difference, Difference::CaseSensitive);
    }

    #[test]
    fn test_no_difference() {
        let candidates = ["a.txt", "b.txt"];
        let result = diff("a.txt", case_sensitive(), "a.txt", case_sensitive(), &candidates)
            .unwrap();
        assert_eq!(result.witnesses.len(), 0);
        assert_eq!(result.is_empty(), true);
        assert_eq!(result.finite, true);
    }

    #[test]
    fn test_duplicate_identity_preserved() {
        // The same path appearing twice in the candidate list yields two
        // witnesses with distinct identities (different indices).
        let candidates = ["A.txt", "A.txt"];
        let result = diff("a.txt", case_sensitive(), "a.txt", case_insensitive(), &candidates)
            .unwrap();

        assert_eq!(result.witnesses.len(), 2);
        assert_eq!(result.witnesses[0].candidate.path, "A.txt");
        assert_eq!(result.witnesses[0].candidate.index, 0);
        assert_eq!(result.witnesses[1].candidate.path, "A.txt");
        assert_eq!(result.witnesses[1].candidate.index, 1);
    }

    #[test]
    fn test_dot_difference() {
        // Dot file rule changes are explained separately.
        let candidates = [".hello.txt", "hello.txt"];
        let result = diff(
            "*.txt",
            literal_leading_dot(),
            "*.txt",
            MatchOptions::new(),
            &candidates,
        )
        .unwrap();

        assert_eq!(result.witnesses.len(), 1);
        let w = &result.witnesses[0];
        assert_eq!(w.candidate.path, ".hello.txt");
        assert_eq!(w.candidate.index, 0);
        assert_eq!(w.left, false);
        assert_eq!(w.right, true);
        assert_eq!(w.difference, Difference::LiteralLeadingDot);
    }

    #[test]
    fn test_separator_difference() {
        let candidates = ["a/b", "a*b"];
        let result = diff("a*b", literal_separator(), "a*b", MatchOptions::new(), &candidates)
            .unwrap();

        // `a/b` is the witness: literal-separator rejects it while the
        // default (non-literal) setting accepts it.
        assert_eq!(result.witnesses.len(), 1);
        let w = &result.witnesses[0];
        assert_eq!(w.candidate.path, "a/b");
        assert_eq!(w.candidate.index, 0);
        assert_eq!(w.left, false);
        assert_eq!(w.right, true);
        assert_eq!(w.difference, Difference::LiteralSeparator);
    }

    #[test]
    fn test_invalid_pattern_rejected_before_checking() {
        // If either pattern is invalid, an error is returned and no partial
        // diff is produced.
        let candidates = ["a.txt"];

        // Left pattern invalid.
        let err = diff("a/**b", case_sensitive(), "a.txt", case_sensitive(), &candidates)
            .unwrap_err();
        assert_eq!(err.pos, 4);

        // Right pattern invalid.
        let err = diff("a.txt", case_sensitive(), "a/**b", case_sensitive(), &candidates)
            .unwrap_err();
        assert_eq!(err.pos, 4);
    }

    #[test]
    fn test_not_attributable_when_patterns_differ() {
        // When the patterns themselves differ (and options are identical),
        // the disagreement is not attributable to an option field.
        let candidates = ["a.txt"];
        let result = diff("a.txt", case_sensitive(), "b.txt", case_sensitive(), &candidates)
            .unwrap();

        assert_eq!(result.witnesses.len(), 1);
        let w = &result.witnesses[0];
        assert_eq!(w.candidate.path, "a.txt");
        assert_eq!(w.left, true);
        assert_eq!(w.right, false);
        assert_eq!(w.difference, Difference::NotAttributable);
    }

    #[test]
    fn test_diff_specs() {
        let left = MatchSpec::new("a.txt", case_sensitive()).unwrap();
        let right = MatchSpec::new("a.txt", case_insensitive()).unwrap();
        let candidates = ["A.txt", "A.txt", "b.txt"];

        let result = diff_specs(&left, &right, &candidates);
        assert_eq!(result.candidate_count, 3);
        assert_eq!(result.finite, true);
        assert_eq!(result.witnesses.len(), 2);
        assert_eq!(result.witnesses[0].candidate.index, 0);
        assert_eq!(result.witnesses[1].candidate.index, 1);
    }

    #[test]
    fn test_match_spec_accessors() {
        let spec = MatchSpec::new("a.txt", case_sensitive()).unwrap();
        assert_eq!(spec.pattern().as_str(), "a.txt");
        assert_eq!(spec.options(), case_sensitive());
    }

    #[test]
    fn test_empty_candidates() {
        let result = diff("a.txt", case_sensitive(), "a.txt", case_insensitive(), &[]).unwrap();
        assert_eq!(result.candidate_count, 0);
        assert_eq!(result.witnesses.len(), 0);
        assert_eq!(result.finite, true);
        assert_eq!(result.is_empty(), true);
    }
}

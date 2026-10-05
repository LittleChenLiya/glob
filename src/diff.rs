// Copyright 2014 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Read-only diffing of glob matching behaviour over a *finite* candidate set.
//!
//! This module answers a narrow question: given one or two patterns, two
//! explicit [`MatchOptions`] sets, and a caller-supplied finite list of
//! candidate paths, which candidates match under one configuration but not
//! the other? Each such candidate is reported as a [`Witness`], together with
//! a per-field explanation (case sensitivity, literal separator, literal
//! leading dot are explained separately).
//!
//! The checker is purely read-only: it only calls [`Pattern::matches_with`]
//! on the supplied strings. It never scans the filesystem, never reads
//! directories, and never creates files.
//!
//! # Finiteness guarantee
//!
//! The result only covers the candidates that were passed in. An empty
//! witness list means "no difference observed on these N candidates"; it
//! does **not** prove the two configurations are equivalent on all possible
//! paths. This guarantee is also recorded verbatim in
//! [`OptionsDiff::finiteness_note`] so it travels with the result.
//!
//! # Error handling
//!
//! Both patterns are compiled and rejected *before* any candidate is
//! checked. If either pattern is invalid, the whole check fails with a
//! [`PatternError`] and no partial diff is ever returned.
//!
//! # Three-path example
//!
//! ```rust
//! use glob::{diff_match_options, MatchOptions};
//!
//! let case_on = MatchOptions {
//!     case_sensitive: true,
//!     require_literal_separator: false,
//!     require_literal_leading_dot: false,
//! };
//! let case_off = MatchOptions {
//!     case_sensitive: false,
//!     ..case_on
//! };
//!
//! // Three candidate paths; note "A.txt" appears twice on purpose.
//! let candidates = ["A.txt", "a.txt", "A.txt"];
//! let diff = diff_match_options("a.txt", case_on, case_off, &candidates).unwrap();
//!
//! // "A.txt" matches only when case sensitivity is off, and both duplicate
//! // occurrences are reported with their own identity (index).
//! assert_eq!(diff.witnesses.len(), 2);
//! assert_eq!(diff.witnesses[0].index, 0);
//! assert_eq!(diff.witnesses[1].index, 2);
//! assert!(!diff.is_equivalent_on_candidates());
//! println!("{}", diff.finiteness_note);
//! ```
//!
//! Run the checks for this module with:
//!
//! ```text
//! cargo test --test diff
//! cargo test --doc diff
//! ```

use crate::{MatchOptions, Pattern, PatternError};
use std::fmt;

/// Which `MatchOptions` field explains (part of) an observed difference.
///
/// Each field is reported independently: a single witness can carry several
/// reasons when flipping any one of several fields alone would change its
/// match result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffReason {
    /// `MatchOptions::case_sensitive` differs and flipping it alone changes
    /// whether this candidate matches (e.g. `A.txt` vs `a.txt`).
    CaseSensitivity,
    /// `MatchOptions::require_literal_separator` differs and flipping it
    /// alone changes whether this candidate matches (e.g. whether `*` may
    /// cross a `/`).
    LiteralSeparator,
    /// `MatchOptions::require_literal_leading_dot` differs and flipping it
    /// alone changes whether this candidate matches (dot-file rule).
    LiteralLeadingDot,
}

impl DiffReason {
    /// A short human-readable explanation of this reason.
    pub fn explain(self) -> &'static str {
        match self {
            DiffReason::CaseSensitivity => concat!(
                "case_sensitive differs: flipping it alone changes this match ",
                "(e.g. `A.txt` vs `a.txt`)"
            ),
            DiffReason::LiteralSeparator => concat!(
                "require_literal_separator differs: flipping it alone changes ",
                "this match (whether `*`/`?`/`[...]` may cross a path separator)"
            ),
            DiffReason::LiteralLeadingDot => concat!(
                "require_literal_leading_dot differs: flipping it alone changes ",
                "this match (dot-file rule for components starting with `.`)"
            ),
        }
    }
}

impl fmt::Display for DiffReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.explain())
    }
}

/// One candidate path whose match result differs between the two
/// configurations.
///
/// The candidate keeps its original identity: `path` is stored verbatim and
/// `index` is its position in the caller-supplied candidate slice, so
/// duplicate occurrences of the same path are reported as distinct witnesses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Witness {
    /// Position of this candidate in the input slice.
    pub index: usize,
    /// The candidate path, exactly as supplied.
    pub path: String,
    /// Whether the candidate matched under configuration A.
    pub matched_by_a: bool,
    /// Whether the candidate matched under configuration B.
    pub matched_by_b: bool,
    /// Independent per-field explanations of the difference.
    pub reasons: Vec<DiffReason>,
}

/// The outcome of diffing two (pattern, options) configurations over a
/// finite candidate set.
#[derive(Debug, Clone)]
pub struct OptionsDiff {
    /// Pattern string of configuration A (as supplied).
    pub pattern_a: String,
    /// Pattern string of configuration B (as supplied).
    pub pattern_b: String,
    /// Options of configuration A.
    pub options_a: MatchOptions,
    /// Options of configuration B.
    pub options_b: MatchOptions,
    /// How many candidate paths were checked.
    pub candidate_count: usize,
    /// Candidates that matched under exactly one configuration, in input
    /// order. Duplicates in the input appear as separate witnesses.
    pub witnesses: Vec<Witness>,
    /// The finiteness guarantee, recorded in the result itself: the verdict
    /// only covers `candidate_count` supplied paths and must not be read as
    /// a proof of equivalence over all paths.
    pub finiteness_note: String,
}

impl OptionsDiff {
    /// True when no candidate matched differently.
    ///
    /// This only means "equivalent on the supplied finite candidates"; see
    /// [`OptionsDiff::finiteness_note`].
    pub fn is_equivalent_on_candidates(&self) -> bool {
        self.witnesses.is_empty()
    }
}

fn finiteness_note(candidate_count: usize, witness_count: usize) -> String {
    format!(
        concat!(
            "finite-check guarantee: this diff only covers the {} supplied ",
            "candidate path(s) and found {} witness(es); an empty witness list ",
            "does NOT prove the two configurations are equivalent on all ",
            "possible paths / 有限性保证：本结果仅覆盖给定的 {} 条候选路径，",
            "发现 {} 条见证；不能凭有限候选宣称两个模式完全等价。"
        ),
        candidate_count, witness_count, candidate_count, witness_count
    )
}

/// Attributes reasons for one divergent candidate by flipping each differing
/// option field of `from` towards `to`, one field at a time, and keeping the
/// flips that alone change the match result.
fn attribute_reasons(
    pattern: &Pattern,
    path: &str,
    from: MatchOptions,
    to: MatchOptions,
) -> Vec<DiffReason> {
    let base = pattern.matches_with(path, from);
    let mut reasons = Vec::new();

    if from.case_sensitive != to.case_sensitive {
        let flipped = MatchOptions {
            case_sensitive: to.case_sensitive,
            ..from
        };
        if pattern.matches_with(path, flipped) != base {
            reasons.push(DiffReason::CaseSensitivity);
        }
    }
    if from.require_literal_separator != to.require_literal_separator {
        let flipped = MatchOptions {
            require_literal_separator: to.require_literal_separator,
            ..from
        };
        if pattern.matches_with(path, flipped) != base {
            reasons.push(DiffReason::LiteralSeparator);
        }
    }
    if from.require_literal_leading_dot != to.require_literal_leading_dot {
        let flipped = MatchOptions {
            require_literal_leading_dot: to.require_literal_leading_dot,
            ..from
        };
        if pattern.matches_with(path, flipped) != base {
            reasons.push(DiffReason::LiteralLeadingDot);
        }
    }
    reasons
}

/// Diffs one pattern under two explicit [`MatchOptions`] sets over a finite
/// candidate list.
///
/// This is a convenience wrapper around [`diff_patterns`] that uses the same
/// pattern for both configurations. All guarantees (read-only, no filesystem
/// access, patterns rejected before checking, finite coverage only) are
/// identical.
///
/// # Errors
///
/// Returns the [`PatternError`] if `pattern` is invalid. The pattern is
/// compiled before any candidate is inspected, so a bad pattern can never
/// yield a partial diff.
pub fn diff_match_options(
    pattern: &str,
    options_a: MatchOptions,
    options_b: MatchOptions,
    candidates: &[&str],
) -> Result<OptionsDiff, PatternError> {
    diff_patterns(pattern, options_a, pattern, options_b, candidates)
}

/// Diffs two (pattern, options) configurations over a finite candidate list.
///
/// Every candidate is matched read-only via [`Pattern::matches_with`]; no
/// filesystem scanning is performed. Candidates that match under exactly one
/// configuration are returned as [`Witness`]es, in input order, each keeping
/// its original identity (`index` + verbatim `path`), so duplicate paths are
/// reported once per occurrence.
///
/// # Errors
///
/// Both patterns are compiled and rejected *before* a single candidate is
/// checked. If either is invalid the whole call fails with its
/// [`PatternError`]; no partial diff is ever returned.
pub fn diff_patterns(
    pattern_a: &str,
    options_a: MatchOptions,
    pattern_b: &str,
    options_b: MatchOptions,
    candidates: &[&str],
) -> Result<OptionsDiff, PatternError> {
    // Uniformly reject invalid patterns BEFORE checking any candidate, so a
    // malformed pattern can never produce half a diff.
    let compiled_a = Pattern::new(pattern_a)?;
    let compiled_b = Pattern::new(pattern_b)?;

    let mut witnesses = Vec::new();
    for (index, &path) in candidates.iter().enumerate() {
        let matched_by_a = compiled_a.matches_with(path, options_a);
        let matched_by_b = compiled_b.matches_with(path, options_b);
        if matched_by_a == matched_by_b {
            continue;
        }
        // Explain the divergence field-by-field. When the two pattern texts
        // also differ, the divergence may stem from the patterns themselves;
        // both texts are recorded on the result for the caller to compare.
        let reasons = attribute_reasons(&compiled_a, path, options_a, options_b);
        witnesses.push(Witness {
            index,
            path: path.to_string(),
            matched_by_a,
            matched_by_b,
            reasons,
        });
    }

    let candidate_count = candidates.len();
    let finiteness_note = finiteness_note(candidate_count, witnesses.len());
    Ok(OptionsDiff {
        pattern_a: pattern_a.to_string(),
        pattern_b: pattern_b.to_string(),
        options_a,
        options_b,
        candidate_count,
        witnesses,
        finiteness_note,
    })
}

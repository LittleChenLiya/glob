// Copyright 2014 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Pattern compatibility analysis.
//!
//! This module provides a standalone API for deciding whether two glob
//! patterns can match a common path and, when they can, producing the
//! shortest such path (a "witness").
//!
//! The analysis supports literal characters, `?`, `*`, character classes
//! (`[...]` and `[!...]`) and recursive wildcards (`**`) used as complete
//! path segments. The path separator is always `/` and `*`/`?`/character
//! classes never match across a separator, mirroring
//! `MatchOptions::require_literal_separator`. Unsupported syntax (such as
//! `\`) is reported as an error instead of being silently misinterpreted.
//!
//! # Examples
//!
//! ```rust
//! use glob::intersect::{intersect_patterns, IntersectOptions};
//!
//! let witness = intersect_patterns("src/*.rs", "src/main.*", IntersectOptions::new()).unwrap();
//! assert_eq!(witness.as_deref(), Some("src/main.rs"));
//!
//! let disjoint = intersect_patterns("src/*.rs", "tests/*.rs", IntersectOptions::new()).unwrap();
//! assert_eq!(disjoint, None);
//! ```

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use crate::CharSpecifier::{CharRange, SingleChar};
use crate::PatternToken::{AnyChar, AnyExcept, AnyRecursiveSequence, AnySequence, AnyWithin, Char};
use crate::{
    chars_eq, in_char_specifiers, MatchOptions, Pattern, PatternError, PatternToken,
};

/// The default upper bound on the number of automaton states the analysis
/// is allowed to explore before giving up with
/// [`IntersectError::StateBudgetExceeded`].
pub const DEFAULT_STATE_BUDGET: usize = 100_000;

const ERROR_UNSUPPORTED_BACKSLASH: &str =
    "backslash is not supported by pattern intersection analysis; use `/` as the path separator";

/// Options controlling the behaviour of [`intersect_patterns`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct IntersectOptions {
    /// Whether or not literal characters and character classes are compared
    /// case-sensitively. Like [`MatchOptions::case_sensitive`], this only
    /// considers ASCII case relationships.
    pub case_sensitive: bool,

    /// Whether or not wildcards (`*`, `?`, `**` and `[...]`) are allowed to
    /// match a path component that starts with a `.` (a dot file). When this
    /// is `false`, a leading `.` must appear literally in the pattern, which
    /// mirrors `MatchOptions::require_literal_leading_dot`.
    pub allow_dot_files: bool,

    /// The maximum number of automaton states the analysis may explore
    /// before returning [`IntersectError::StateBudgetExceeded`]. This bounds
    /// the time and memory used by pathological patterns.
    pub state_budget: usize,
}

impl Default for IntersectOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl IntersectOptions {
    /// Constructs a new `IntersectOptions` with default field values:
    /// case-sensitive matching, dot files matchable by wildcards and a state
    /// budget of [`DEFAULT_STATE_BUDGET`].
    pub fn new() -> Self {
        IntersectOptions {
            case_sensitive: true,
            allow_dot_files: true,
            state_budget: DEFAULT_STATE_BUDGET,
        }
    }

    /// The `MatchOptions` a produced witness is guaranteed to match both
    /// patterns under.
    fn match_options(&self) -> MatchOptions {
        MatchOptions {
            case_sensitive: self.case_sensitive,
            require_literal_separator: true,
            require_literal_leading_dot: !self.allow_dot_files,
        }
    }
}

/// An error returned by [`intersect_patterns`].
#[derive(Debug)]
pub enum IntersectError {
    /// The first pattern is invalid or uses unsupported syntax.
    InvalidFirstPattern(PatternError),
    /// The second pattern is invalid or uses unsupported syntax.
    InvalidSecondPattern(PatternError),
    /// The analysis explored more states than
    /// [`IntersectOptions::state_budget`] allows.
    StateBudgetExceeded,
}

impl fmt::Display for IntersectError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            IntersectError::InvalidFirstPattern(ref e) => {
                write!(f, "invalid first pattern: {}", e)
            }
            IntersectError::InvalidSecondPattern(ref e) => {
                write!(f, "invalid second pattern: {}", e)
            }
            IntersectError::StateBudgetExceeded => write!(
                f,
                "pattern intersection analysis exceeded its state budget"
            ),
        }
    }
}

impl Error for IntersectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match *self {
            IntersectError::InvalidFirstPattern(ref e)
            | IntersectError::InvalidSecondPattern(ref e) => Some(e),
            IntersectError::StateBudgetExceeded => None,
        }
    }
}

/// Decides whether two glob patterns can match a common path.
///
/// Both patterns are compiled with the same syntax rules as [`Pattern`];
/// additionally `\` is rejected because the path separator is fixed to `/`.
/// `**` is only supported as a complete path segment, exactly as in
/// [`Pattern::new`].
///
/// On success this returns `Ok(Some(witness))` where `witness` is the
/// shortest path (fewest characters, using `/` as the separator) matched by
/// both patterns, or `Ok(None)` when the patterns cannot match a common
/// path. The witness is guaranteed to match both patterns under
/// `MatchOptions { case_sensitive: options.case_sensitive,
/// require_literal_separator: true, require_literal_leading_dot:
/// !options.allow_dot_files }`.
///
/// # Examples
///
/// ```rust
/// use glob::intersect::{intersect_patterns, IntersectOptions};
///
/// let options = IntersectOptions::new();
/// assert!(intersect_patterns("*.rs", "*.txt", options).unwrap().is_none());
/// assert!(intersect_patterns("**/*.rs", "src/**", options).unwrap().is_some());
/// ```
pub fn intersect_patterns(
    first: &str,
    second: &str,
    options: IntersectOptions,
) -> Result<Option<String>, IntersectError> {
    let first_tokens = compile(first).map_err(IntersectError::InvalidFirstPattern)?;
    let second_tokens = compile(second).map_err(IntersectError::InvalidSecondPattern)?;
    shortest_common_path(&first_tokens, &second_tokens, &options)
}

/// Compiles a pattern into its token stream, rejecting syntax that the
/// analysis does not support.
fn compile(pattern: &str) -> Result<Vec<PatternToken>, PatternError> {
    if let Some(pos) = pattern.find('\\') {
        return Err(PatternError {
            pos,
            msg: ERROR_UNSUPPORTED_BACKSLASH,
        });
    }
    Ok(Pattern::new(pattern)?.tokens)
}

/// Whether the token matches a (possibly empty) sequence of characters and
/// can therefore be skipped without consuming input.
fn is_star_like(token: &PatternToken) -> bool {
    matches!(*token, AnySequence | AnyRecursiveSequence)
}

/// Whether a star-like token may stop consuming input at a position where
/// the previously consumed character is a separator (or the start of the
/// path). This mirrors `Pattern::matches_from`, which only attempts to match
/// the remainder of the pattern after a recursive wildcard at separator
/// boundaries. A trailing run of star-like tokens may also stop anywhere,
/// because the matcher accepts once both the pattern and the path are
/// exhausted simultaneously.
fn skippable(token: &PatternToken, follows_separator: bool, rest_are_stars: bool) -> bool {
    match *token {
        AnySequence => true,
        AnyRecursiveSequence => follows_separator || rest_are_stars,
        _ => false,
    }
}

/// Whether `token` can consume the character `c` of a candidate witness
/// path, given whether the previous character was a separator.
fn token_matches(
    token: &PatternToken,
    c: char,
    follows_separator: bool,
    options: &IntersectOptions,
    match_options: &MatchOptions,
) -> bool {
    let is_sep = c == '/';
    let hidden = follows_separator && !options.allow_dot_files && c == '.';
    match *token {
        Char(c2) => chars_eq(c, c2, options.case_sensitive),
        AnyRecursiveSequence => !hidden,
        AnySequence | AnyChar => !is_sep && !hidden,
        AnyWithin(ref specifiers) => {
            !is_sep && !hidden && in_char_specifiers(specifiers, c, *match_options)
        }
        AnyExcept(ref specifiers) => {
            !is_sep && !hidden && !in_char_specifiers(specifiers, c, *match_options)
        }
    }
}

fn push_candidate(candidates: &mut Vec<char>, c: char) {
    if !candidates.contains(&c) {
        candidates.push(c);
    }
}

fn prev_char(c: char) -> Option<char> {
    let n = c as u32;
    if n == 0 {
        None
    } else {
        char::from_u32(n - 1)
    }
}

fn next_char(c: char) -> Option<char> {
    let mut n = c as u32 + 1;
    if (0xD800..0xE000).contains(&n) {
        n = 0xE000;
    }
    char::from_u32(n)
}

/// Characters worth trying as the next character of a witness when the two
/// given tokens both have to consume it. The list is complete for the
/// supported token combinations: any character accepted by a literal or a
/// positive character class is covered by the literal itself or the start of
/// one of its ranges, and the complement of a negated class is covered by
/// the characters surrounding its ranges. A handful of defaults and a
/// printable-ASCII fallback in `common_char` cover the remaining cases.
fn candidate_chars(a: &PatternToken, b: &PatternToken) -> Vec<char> {
    let mut candidates = Vec::new();
    for token in &[a, b] {
        match **token {
            Char(c) => push_candidate(&mut candidates, c),
            AnyWithin(ref specifiers) | AnyExcept(ref specifiers) => {
                for specifier in specifiers {
                    match *specifier {
                        SingleChar(c) => {
                            push_candidate(&mut candidates, c);
                            if let Some(prev) = prev_char(c) {
                                push_candidate(&mut candidates, prev);
                            }
                            if let Some(next) = next_char(c) {
                                push_candidate(&mut candidates, next);
                            }
                        }
                        CharRange(lo, hi) => {
                            push_candidate(&mut candidates, lo);
                            push_candidate(&mut candidates, hi);
                            if let Some(prev) = prev_char(lo) {
                                push_candidate(&mut candidates, prev);
                            }
                            if let Some(next) = next_char(hi) {
                                push_candidate(&mut candidates, next);
                            }
                        }
                    }
                }
            }
            _ => (),
        }
    }
    for c in &[
        'a',
        'A',
        '0',
        '_',
        '-',
        '.',
        '!',
        ' ',
        '~',
        '#',
        '\u{e9}',
        '\u{4e2d}',
    ] {
        push_candidate(&mut candidates, *c);
    }
    candidates
}

/// Finds a character that both tokens can consume at the current position,
/// preferring "nice" characters for readability of the resulting witness.
/// Also reports whether both tokens can consume a `/` separator.
///
/// The witness never contains a `\`: the separator is unified to `/`, even
/// though the underlying matcher would treat a backslash as a separator on
/// Windows.
fn common_char(
    a: &PatternToken,
    b: &PatternToken,
    follows_separator: bool,
    options: &IntersectOptions,
    match_options: &MatchOptions,
) -> (Option<char>, bool) {
    let both_match = |c: char| {
        token_matches(a, c, follows_separator, options, match_options)
            && token_matches(b, c, follows_separator, options, match_options)
    };

    let mut found = candidate_chars(a, b)
        .into_iter()
        .filter(|&c| c != '/' && c != '\\')
        .find(|&c| both_match(c));

    if found.is_none() {
        // Fallback for exotic negated classes: scan printable ASCII.
        found = (0x21u8..0x7f)
            .map(char::from)
            .filter(|&c| c != '\\')
            .find(|&c| both_match(c));
    }

    let separator = both_match('/');
    (found, separator)
}

/// Explores the product of the two token automata with Dijkstra's algorithm
/// (each emitted character costs 1, skipping a star costs 0), returning the
/// shortest witness path or `None` when the patterns are disjoint.
fn shortest_common_path(
    a: &[PatternToken],
    b: &[PatternToken],
    options: &IntersectOptions,
) -> Result<Option<String>, IntersectError> {
    let match_options = options.match_options();

    // Whether every token from index `i` onwards is star-like, in which case
    // a recursive wildcard may stop consuming at any position (see
    // `skippable`).
    let trailing_stars = |tokens: &[PatternToken]| -> Vec<bool> {
        let mut result = vec![false; tokens.len()];
        let mut all_stars = true;
        for (i, token) in tokens.iter().enumerate().rev() {
            all_stars = all_stars && is_star_like(token);
            result[i] = all_stars;
        }
        result
    };
    let a_trailing = trailing_stars(a);
    let b_trailing = trailing_stars(b);

    type State = (usize, usize, bool);
    let mut best: HashMap<State, usize> = HashMap::new();
    let mut heap: BinaryHeap<Reverse<(usize, String, State)>> = BinaryHeap::new();
    heap.push(Reverse((0, String::new(), (0, 0, true))));
    let mut explored = 0usize;

    while let Some(Reverse((cost, witness, (i, j, follows_separator)))) = heap.pop() {
        let state = (i, j, follows_separator);
        if best.get(&state).map_or(false, |&c| c <= cost) {
            continue;
        }
        best.insert(state, cost);
        explored += 1;
        if explored > options.state_budget {
            return Err(IntersectError::StateBudgetExceeded);
        }

        if i == a.len() && j == b.len() {
            return Ok(Some(witness));
        }

        // Skip a star-like token on either side without consuming input.
        if let Some(token) = a.get(i) {
            if skippable(token, follows_separator, a_trailing[i]) {
                heap.push(Reverse((
                    cost,
                    witness.clone(),
                    (i + 1, j, follows_separator),
                )));
            }
        }
        if let Some(token) = b.get(j) {
            if skippable(token, follows_separator, b_trailing[j]) {
                heap.push(Reverse((
                    cost,
                    witness.clone(),
                    (i, j + 1, follows_separator),
                )));
            }
        }

        // Emit one witness character consumed by both current tokens.
        if let (Some(ta), Some(tb)) = (a.get(i), b.get(j)) {
            let next_i = i + usize::from(!is_star_like(ta));
            let next_j = j + usize::from(!is_star_like(tb));
            let (character, separator) =
                common_char(ta, tb, follows_separator, options, &match_options);
            if let Some(c) = character {
                let mut next = witness.clone();
                next.push(c);
                heap.push(Reverse((cost + 1, next, (next_i, next_j, false))));
            }
            if separator {
                let mut next = witness;
                next.push('/');
                heap.push(Reverse((cost + 1, next, (next_i, next_j, true))));
            }
        }
    }

    Ok(None)
}

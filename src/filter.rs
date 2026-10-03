// Copyright 2026 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Stable, in-memory multi-pattern path filtering.
//!
//! This module provides [`filter_paths`] and [`filter_paths_with`], two
//! deterministic, allocation-light entry points for sifting a batch of
//! in-memory path strings against several compiled [`Pattern`]s at once.
//!
//! Neither function touches the filesystem: matching is performed in memory
//! against the original path strings, so the functions are safe to use with
//! paths that do not (or must not) exist on disk.
//!
//! # Dedup identity
//!
//! The same path string is reported at most once. By default two paths are
//! considered identical exactly when their original strings are identical,
//! i.e. matching is case-sensitive and separator-sensitive: `A.rs` and
//! `a.rs` are distinct, and `a/b` and `a\b` are distinct.
//!
//! An explicit *normalizer* (see [`filter_paths_with`]) is the only way to
//! change dedup identity: it maps each original string to a key, and paths
//! sharing a key are merged into one result. The original string is still
//! used for both matching and display (the first occurrence wins), so a
//! normalizer never changes what a pattern matches. If the normalizer fails
//! for any path, the whole batch is rejected and no partial result is
//! returned.
//!
//! # Stability
//!
//! Results are yielded in first-occurrence order of their dedup key, and
//! each result's [`sources`](FilteredPath::sources) list the matched
//! patterns in the patterns' declaration order.
//!
//! # Examples
//!
//! ```rust
//! use glob::Pattern;
//! use glob::filter::filter_paths;
//!
//! let patterns = [
//!     Pattern::new("src/*").unwrap(),
//!     Pattern::new("**/*.rs").unwrap(),
//! ];
//! let paths = ["src/a.rs", "src/a.rs", "src/b.txt", "A.rs", "a.rs"];
//!
//! let hits = filter_paths(&patterns, paths.iter().copied());
//!
//! // `src/a.rs` matches both patterns and is reported only once, with both
//! // sources. The duplicate input is collapsed. `A.rs` and `a.rs` stay
//! // distinct because matching is case-sensitive by default.
//! assert_eq!(hits.len(), 4);
//! assert_eq!(hits[0].path(), "src/a.rs");
//! assert_eq!(hits[0].sources(), &[0, 1]);
//! assert_eq!(hits[1].path(), "src/b.txt");
//! assert_eq!(hits[1].sources(), &[0]);
//! assert_eq!(hits[2].path(), "A.rs");
//! assert_eq!(hits[3].path(), "a.rs");
//! ```

use crate::Pattern;
use std::collections::HashMap;
use std::hash::Hash;

/// A path that was matched against one or more patterns.
///
/// Returned by [`filter_paths`] and [`filter_paths_with`]. The same path
/// string is yielded at most once; [`sources`](FilteredPath::sources) lists,
/// in pattern declaration order, the indices of every pattern that matched
/// the path. A path that matched no pattern is still returned with an empty
/// source list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilteredPath<'a> {
    path: &'a str,
    sources: Vec<usize>,
}

impl<'a> FilteredPath<'a> {
    /// The original path string.
    ///
    /// This is the string that was matched against the patterns and that is
    /// used for display. It is never altered by the dedup normalizer; when
    /// several normalized-equal strings are merged, the first occurrence's
    /// string is kept here.
    pub fn path(&self) -> &'a str {
        self.path
    }

    /// Indices, in pattern declaration order, of the patterns that matched
    /// [`path`](FilteredPath::path).
    ///
    /// The list is sorted, contains no duplicates, and is empty when no
    /// pattern matched.
    pub fn sources(&self) -> &[usize] {
        &self.sources
    }
}

/// Sift a batch of in-memory path strings against several compiled patterns.
///
/// Every distinct path is returned in first-occurrence order, annotated with
/// the indices (in declaration order) of the patterns it matched. Paths that
/// matched no pattern are returned with an empty
/// [`sources`](FilteredPath::sources) list.
///
/// Dedup identity is the original string itself: matching is case-sensitive
/// and separator-sensitive. Use [`filter_paths_with`] with an explicit
/// normalizer to change dedup identity.
///
/// This function never touches the filesystem.
///
/// # Examples
///
/// ```rust
/// use glob::Pattern;
/// use glob::filter::filter_paths;
///
/// let patterns = [Pattern::new("*.rs").unwrap()];
/// let paths = ["a.rs", "b.txt", "a.rs"];
///
/// let hits = filter_paths(&patterns, paths.iter().copied());
/// assert_eq!(hits.len(), 2);
/// assert_eq!(hits[0].path(), "a.rs");
/// assert_eq!(hits[0].sources(), &[0]);
/// assert_eq!(hits[1].path(), "b.txt");
/// assert!(hits[1].sources().is_empty());
/// ```
pub fn filter_paths<'a, I>(patterns: &[Pattern], paths: I) -> Vec<FilteredPath<'a>>
where
    I: IntoIterator<Item = &'a str>,
{
    // The identity normalizer is infallible, so this never returns an error.
    filter_paths_with(patterns, paths, |s| {
        Ok::<_, std::convert::Infallible>(s)
    })
    .unwrap_or_else(|e| match e {})
}

/// Sift a batch of in-memory path strings against several compiled patterns,
/// using an explicit normalizer to decide dedup identity.
///
/// The normalizer maps each original path string to a key. Paths with equal
/// keys are merged into a single result: the first occurrence's original
/// string is kept for display and matching, and the matched-pattern indices
/// of all merged occurrences are unioned (in declaration order). The
/// original string is still what patterns are matched against, so the
/// normalizer only affects which paths are considered the same.
///
/// The whole batch is rejected: if the normalizer returns an error for any
/// path, an error is returned and no partial result is produced.
///
/// This function never touches the filesystem.
///
/// # Examples
///
/// ```rust
/// use glob::Pattern;
/// use glob::filter::filter_paths_with;
///
/// let patterns = [Pattern::new("a.rs").unwrap(), Pattern::new("*.rs").unwrap()];
/// let paths = ["a.rs", "A.rs"];
///
/// // A case-folding normalizer merges `a.rs` and `A.rs`. The merged result
/// // keeps the first occurrence's string and unions the sources.
/// let hits = filter_paths_with(&patterns, paths.iter().copied(), |s| {
///     Ok::<_, std::convert::Infallible>(s.to_lowercase())
/// })
/// .unwrap();
///
/// assert_eq!(hits.len(), 1);
/// assert_eq!(hits[0].path(), "a.rs");
/// assert_eq!(hits[0].sources(), &[0, 1]);
/// ```
pub fn filter_paths_with<'a, I, K, E, F>(
    patterns: &[Pattern],
    paths: I,
    normalize: F,
) -> Result<Vec<FilteredPath<'a>>, E>
where
    I: IntoIterator<Item = &'a str>,
    K: Eq + Hash,
    // All input strings share the `'a` lifetime, so the normalizer may
    // return a key that borrows from the path it is given.
    F: FnMut(&'a str) -> Result<K, E>,
{
    let paths: Vec<&str> = paths.into_iter().collect();

    // Compute every key before matching anything. If the normalizer fails on
    // any path, the whole batch is rejected and no partial result escapes.
    let keys: Vec<K> = paths
        .iter()
        .copied()
        .map(normalize)
        .collect::<Result<_, _>>()?;

    let mut results: Vec<FilteredPath<'a>> = Vec::new();
    let mut seen: HashMap<K, usize> = HashMap::new();

    for (path, key) in paths.into_iter().zip(keys) {
        // Sources are collected in pattern declaration order, so they are
        // already sorted and unique for a single path.
        let mut sources = Vec::new();
        for (i, pattern) in patterns.iter().enumerate() {
            if pattern.matches(path) {
                sources.push(i);
            }
        }

        match seen.get(&key) {
            Some(&idx) => {
                // Merge with the earlier occurrence: keep its string for
                // display, but union the sources in declaration order.
                let entry = &mut results[idx];
                for src in sources {
                    if let Err(pos) = entry.sources.binary_search(&src) {
                        entry.sources.insert(pos, src);
                    }
                }
            }
            None => {
                seen.insert(key, results.len());
                results.push(FilteredPath { path, sources });
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::{filter_paths, filter_paths_with, FilteredPath};
    use crate::Pattern;

    fn patterns() -> Vec<Pattern> {
        vec![
            Pattern::new("src/*").unwrap(),
            Pattern::new("**/*.rs").unwrap(),
        ]
    }

    fn sources<'a>(hits: &'a [FilteredPath<'a>]) -> Vec<(&'a str, Vec<usize>)> {
        hits.iter()
            .map(|h| (h.path(), h.sources().to_vec()))
            .collect()
    }

    #[test]
    fn hits_are_reported_once_with_all_sources() {
        // `src/a.rs` hits both patterns; the duplicate input is collapsed.
        let paths = ["src/a.rs", "src/a.rs", "src/b.txt"];
        let hits = filter_paths(&patterns(), paths.iter().copied());
        assert_eq!(
            sources(&hits),
            vec![("src/a.rs", vec![0, 1]), ("src/b.txt", vec![0])]
        );
    }

    #[test]
    fn default_identity_is_case_and_separator_sensitive() {
        let paths = ["A.rs", "a.rs", "a/b", r"a\b"];
        let hits = filter_paths(&patterns(), paths.iter().copied());
        assert_eq!(hits.len(), 4);
        assert_eq!(hits[0].path(), "A.rs");
        assert_eq!(hits[1].path(), "a.rs");
        assert_eq!(hits[2].path(), "a/b");
        assert_eq!(hits[3].path(), r"a\b");
    }

    #[test]
    fn results_follow_first_occurrence_order() {
        let paths = ["z.rs", "a.txt", "m.rs", "a.txt", "z.rs"];
        let hits = filter_paths(&patterns(), paths.iter().copied());
        assert_eq!(
            sources(&hits),
            vec![
                ("z.rs", vec![1]),
                ("a.txt", vec![]),
                ("m.rs", vec![1]),
            ]
        );
    }

    #[test]
    fn sources_follow_pattern_declaration_order() {
        let patterns = vec![
            Pattern::new("*.rs").unwrap(),
            Pattern::new("a.rs").unwrap(),
            Pattern::new("**/*.rs").unwrap(),
        ];
        let hits = filter_paths(&patterns, ["a.rs"].iter().copied());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sources(), &[0, 1, 2]);
    }

    #[test]
    fn empty_pattern_group_returns_paths_with_empty_sources() {
        let hits = filter_paths(&[], ["a.rs", "b.txt"].iter().copied());
        assert_eq!(sources(&hits), vec![("a.rs", vec![]), ("b.txt", vec![])]);
    }

    #[test]
    fn empty_input_returns_empty_results() {
        let hits = filter_paths(&patterns(), std::iter::empty::<&str>());
        assert!(hits.is_empty());
    }

    #[test]
    fn normalizer_failure_rejects_whole_batch() {
        let patterns = [Pattern::new("*.rs").unwrap()];
        let paths = ["a.rs", "bad.rs", "c.rs"];

        let mut calls = 0;
        let result = filter_paths_with(&patterns, paths.iter().copied(), |s| {
            calls += 1;
            if s == "bad.rs" {
                Err("normalizer failure")
            } else {
                Ok(s.to_lowercase())
            }
        });

        // The whole batch is rejected: an error is returned, and the
        // normalizer short-circuits at the first failing path (no partial
        // result is produced).
        assert!(result.is_err());
        assert_eq!(calls, 2);
    }

    #[test]
    fn normalizer_merges_sources_without_dropping_any() {
        let patterns = [Pattern::new("a.rs").unwrap(), Pattern::new("*.rs").unwrap()];
        let paths = ["a.rs", "A.rs", "a.rs"];

        let hits = filter_paths_with(&patterns, paths.iter().copied(), |s| {
            Ok::<_, std::convert::Infallible>(s.to_lowercase())
        })
        .unwrap();

        // `a.rs` and `A.rs` merge; the first occurrence's string is kept for
        // display, but the sources hit by both are unioned rather than only
        // the first occurrence's sources being kept.
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path(), "a.rs");
        assert_eq!(hits[0].sources(), &[0, 1]);
    }

    #[test]
    fn normalizer_merges_separator_variants() {
        let patterns = [Pattern::new("**/*.rs").unwrap()];
        let paths = ["src/a.rs", r"src\a.rs"];

        let hits = filter_paths_with(&patterns, paths.iter().copied(), |s| {
            Ok::<_, std::convert::Infallible>(s.replace('\\', "/"))
        })
        .unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path(), "src/a.rs");
        assert_eq!(hits[0].sources(), &[0]);
    }

    #[test]
    fn normalizer_does_not_change_what_patterns_match() {
        // Matching is done against the original string, so a normalizer that
        // rewrites the key must not make a non-matching path match.
        let patterns = [Pattern::new("a.rs").unwrap()];
        let paths = ["b.rs"];

        let hits = filter_paths_with(&patterns, paths.iter().copied(), |s| {
            Ok::<_, std::convert::Infallible>(s.to_lowercase())
        })
        .unwrap();

        assert_eq!(hits.len(), 1);
        assert!(hits[0].sources().is_empty());
    }
}

//! In-memory filtering of path strings against multiple [`Pattern`]s.
//!
//! Unlike [`crate::glob`] and [`crate::glob_with`], the entry points in this
//! module never touch the filesystem. They operate purely on a caller-supplied
//! collection of path strings: every string is tested against every declared
//! pattern and, when it matches at least one pattern, it is reported exactly
//! once together with the list of patterns it hit (its *sources*).
//!
//! Build a [`PatternFilter`] once with [`PatternFilter::new`] (or
//! [`PatternFilter::with_options`]) and reuse it to filter any number of
//! batches.
//!
//! # Identity and deduplication
//!
//! - Path strings are deduplicated by **identity**, not by the patterns they
//!   match. Without a normalizer the identity is the original string itself,
//!   compared byte-for-byte: matching is case sensitive and `/` and `\` are
//!   significant, so `"A.rs"` and `"a.rs"` (or `"a/b"` and `"a\\b"`) are
//!   different paths and produce separate results.
//! - Only an explicit normalizer passed to
//!   [`PatternFilter::filter_normalized`] can change the identity used for
//!   deduplication. When two different original strings normalize to the same
//!   identity they become one result; the result keeps the first original
//!   string seen for display purposes, and the sources contributed by *every*
//!   matching original string are merged.
//! - Matching always uses the **original, unnormalized** string, even when a
//!   normalizer is supplied. The normalizer only controls deduplication.
//!
//! # Sources
//!
//! Sources are pattern indices in the order the patterns were declared
//! (duplicates removed), so a path that is matched by both `src/*` and
//! `**/*.rs` carries both sources rather than just the first one.
//!
//! Results are returned in the order in which each identity is first seen in
//! the input, which makes the output stable for a given input order.
//!
//! # Failures
//!
//! Invalid pattern syntax is rejected up front when constructing the filter.
//! When a normalizer is used, a failure for any single input rejects the whole
//! batch with [`NormalizeError`]: no partial results are returned.
//!
//! # Examples
//!
//! ```rust
//! use glob::PatternFilter;
//!
//! let filter = PatternFilter::new(["src/*", "**/*.rs"]).unwrap();
//!
//! // `src/a.rs` matches both patterns, the duplicate input adds no new result.
//! let hits = filter.filter(["src/a.rs", "src/a.rs", "README.md"]);
//! assert_eq!(hits.len(), 1);
//! assert_eq!(hits[0].path(), "src/a.rs");
//! assert_eq!(
//!     hits[0].source_patterns(&filter),
//!     vec!["src/*", "**/*.rs"],
//! );
//! ```
//!
//! A normalizer can make differently-spelled strings share an identity. The
//! display value and the strings used for matching are still the originals:
//!
//! ```rust
//! use glob::PatternFilter;
//!
//! let filter = PatternFilter::new(["FOO*", "*bar"]).unwrap();
//! let hits = filter
//!     .filter_normalized(
//!         ["FOObar", "foobar"],
//!         |s| Ok::<_, std::convert::Infallible>(s.to_ascii_lowercase()),
//!     )
//!     .unwrap();
//! assert_eq!(hits.len(), 1);
//! assert_eq!(hits[0].path(), "FOObar"); // first original string
//! assert_eq!(hits[0].source_patterns(&filter), vec!["FOO*", "*bar"]);
//! ```

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::ops::Deref;

use crate::{MatchOptions, Pattern, PatternError};

/// The match options used by [`PatternFilter::new`]: matching is case
/// sensitive and path separators must be matched literally (so `*` never
/// consumes a `/`), while leading dots are not treated specially.
pub const DEFAULT_MATCH_OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

/// A stable entry point for filtering path strings in memory against several
/// glob patterns at once.
///
/// Construct a filter with [`PatternFilter::new`] or
/// [`PatternFilter::with_options`], then call [`PatternFilter::filter`] or
/// [`PatternFilter::filter_normalized`] on any number of input batches. The
/// filter never accesses the filesystem.
///
/// See the [module-level documentation](self) for the deduplication, ordering
/// and failure rules.
#[derive(Clone, Debug)]
pub struct PatternFilter {
    patterns: Vec<Pattern>,
    options: MatchOptions,
}

impl PatternFilter {
    /// Compile `patterns` using the [default match options](DEFAULT_MATCH_OPTIONS).
    ///
    /// Patterns are kept in declaration order; that order is also the order in
    /// which sources are reported on matched paths.
    ///
    /// # Errors
    ///
    /// Returns [`PatternBuildError`] for the first pattern that fails to
    /// compile.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use glob::PatternFilter;
    ///
    /// let filter = PatternFilter::new(["src/**/*.rs", "tests/**/*.rs"]).unwrap();
    /// ```
    pub fn new<I, P>(patterns: I) -> Result<Self, PatternBuildError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<str>,
    {
        Self::with_options(patterns, DEFAULT_MATCH_OPTIONS)
    }

    /// Compile `patterns` with explicit [`MatchOptions`].
    ///
    /// The options control matching only; they never change how input strings
    /// are deduplicated (use [`PatternFilter::filter_normalized`] for that).
    ///
    /// # Errors
    ///
    /// Returns [`PatternBuildError`] for the first pattern that fails to
    /// compile.
    pub fn with_options<I, P>(
        patterns: I,
        options: MatchOptions,
    ) -> Result<Self, PatternBuildError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<str>,
    {
        let patterns = patterns
            .into_iter()
            .enumerate()
            .map(|(index, pattern)| {
                let pattern = pattern.as_ref();
                Pattern::new(pattern).map_err(|error| PatternBuildError {
                    index,
                    pattern: pattern.to_owned(),
                    error,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { patterns, options })
    }

    /// Returns the compiled patterns, in declaration order.
    pub fn patterns(&self) -> &[Pattern] {
        &self.patterns
    }

    /// Returns the match options this filter was created with.
    pub fn options(&self) -> MatchOptions {
        self.options
    }

    /// Returns the declaration-order indices of all patterns that match
    /// `path`.
    ///
    /// Matching is performed on the given string as-is. The returned slice is
    /// empty when no pattern matches.
    pub fn source_indices(&self, path: &str) -> Vec<usize> {
        let mut indices = Vec::new();
        for (index, pattern) in self.patterns.iter().enumerate() {
            if pattern.matches_with(path, self.options) {
                indices.push(index);
            }
        }
        indices
    }

    /// Returns whether at least one declared pattern matches `path`.
    pub fn matches(&self, path: &str) -> bool {
        self.patterns
            .iter()
            .any(|pattern| pattern.matches_with(path, self.options))
    }

    /// Filter an in-memory batch of path strings, deduplicating by the exact
    /// original strings.
    ///
    /// Each distinct string that matches at least one pattern yields one
    /// [`MatchedPath`]. Results are ordered by the first occurrence of each
    /// string in `paths`. This method cannot fail.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use glob::PatternFilter;
    ///
    /// let filter = PatternFilter::new(["*.rs"]).unwrap();
    /// let hits = filter.filter(["a.rs", "a.rs", "b.txt"]);
    /// assert_eq!(hits.len(), 1);
    /// assert_eq!(hits[0].path(), "a.rs");
    /// ```
    pub fn filter<I, P>(&self, paths: I) -> Vec<MatchedPath>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<str>,
    {
        // The identity normalizer cannot fail; see `filter_core` for the
        // actual implementation.
        match self.filter_core(paths, |path| Ok::<_, std::convert::Infallible>(path.to_owned())) {
            Ok(hits) => hits,
            Err(error) => match error.source {},
        }
    }

    /// Filter an in-memory batch of path strings, using `normalize` to derive
    /// the identity used for deduplication.
    ///
    /// `normalize` is called exactly once per input string, in input order,
    /// and receives the **original** string. Strings that normalize to the
    /// same value are merged into one result: the result displays the first
    /// original string seen and unions the sources of all merged strings that
    /// match. Pattern matching itself still uses the original strings.
    ///
    /// # Errors
    ///
    /// If `normalize` fails for any input, the whole batch is rejected with
    /// [`NormalizeError`] and no partial results are returned.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use glob::PatternFilter;
    ///
    /// // Merge differently-cased spellings of the same path.
    /// let filter = PatternFilter::new(["*.rs"]).unwrap();
    /// let hits = filter
    ///     .filter_normalized(
    ///         ["A.rs", "a.rs"],
    ///         |s| Ok::<_, std::convert::Infallible>(s.to_ascii_lowercase()),
    ///     )
    ///     .unwrap();
    /// assert_eq!(hits.len(), 1);
    /// assert_eq!(hits[0].path(), "A.rs");
    /// ```
    pub fn filter_normalized<I, P, N, E>(
        &self,
        paths: I,
        normalize: N,
    ) -> Result<Vec<MatchedPath>, NormalizeError<E>>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<str>,
        N: FnMut(&str) -> Result<String, E>,
    {
        self.filter_core(paths, normalize)
    }

    fn filter_core<I, P, N, E>(
        &self,
        paths: I,
        mut normalize: N,
    ) -> Result<Vec<MatchedPath>, NormalizeError<E>>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<str>,
        N: FnMut(&str) -> Result<String, E>,
    {
        // Identity -> position of the group in `results`.
        let mut index_of: HashMap<String, usize> = HashMap::new();
        let mut results: Vec<MatchedPath> = Vec::new();

        for (index, input) in paths.into_iter().enumerate() {
            let original = input.as_ref();

            // Normalize all inputs as they arrive; a failure rejects the
            // whole batch before this string can contribute any result.
            let identity = normalize(original).map_err(|source| NormalizeError {
                index,
                path: original.to_owned(),
                source,
            })?;

            // Matching always uses the original string. Iterate patterns in
            // declaration order, so `hits` is already sorted and unique.
            let mut hits: Vec<usize> = Vec::new();
            for (pattern_index, pattern) in self.patterns.iter().enumerate() {
                if pattern.matches_with(original, self.options) {
                    hits.push(pattern_index);
                }
            }
            if hits.is_empty() {
                continue;
            }

            match index_of.get(&identity) {
                Some(&existing) => {
                    let group = &mut results[existing];
                    for pattern_index in hits {
                        // Merge while keeping declaration order and uniqueness.
                        match group.source_indices.binary_search(&pattern_index) {
                            Ok(_) => {}
                            Err(position) => group.source_indices.insert(position, pattern_index),
                        }
                    }
                }
                None => {
                    index_of.insert(identity, results.len());
                    results.push(MatchedPath {
                        path: original.to_owned(),
                        source_indices: hits,
                    });
                }
            }
        }

        Ok(results)
    }
}

/// A single path string matched by a [`PatternFilter`], together with the
/// indices of every pattern it hit.
///
/// Use [`MatchedPath::path`] (or the `Display`/`Deref<Target = str>`
/// implementations) to access the original string, and
/// [`MatchedPath::source_indices`] or [`MatchedPath::source_patterns`] to
/// inspect which patterns matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchedPath {
    path: String,
    source_indices: Vec<usize>,
}

impl MatchedPath {
    /// Returns the original path string of this result.
    ///
    /// When several original strings share a normalized identity this is the
    /// first one that was seen in the input.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the indices of the patterns that matched this path, in pattern
    /// declaration order, without duplicates.
    pub fn source_indices(&self) -> &[usize] {
        &self.source_indices
    }

    /// Returns the source pattern strings, in declaration order, by looking up
    /// the indices in `filter`.
    ///
    /// # Panics
    ///
    /// Panics if `filter` is not the [`PatternFilter`] that produced this
    /// result and does not contain one of the recorded source indices.
    pub fn source_patterns<'a>(&self, filter: &'a PatternFilter) -> Vec<&'a str> {
        self.source_indices
            .iter()
            .map(|&index| {
                filter
                    .patterns
                    .get(index)
                    .expect("source index does not belong to the given PatternFilter")
                    .as_str()
            })
            .collect()
    }
}

impl fmt::Display for MatchedPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.path, f)
    }
}

impl AsRef<str> for MatchedPath {
    fn as_ref(&self) -> &str {
        &self.path
    }
}

impl Deref for MatchedPath {
    type Target = str;

    fn deref(&self) -> &str {
        &self.path
    }
}

/// An error returned when a pattern passed to [`PatternFilter::new`] or
/// [`PatternFilter::with_options`] fails to compile.
#[derive(Debug)]
pub struct PatternBuildError {
    /// Zero-based index of the invalid pattern in the declared pattern list.
    pub index: usize,

    /// The pattern string that failed to compile.
    pub pattern: String,

    /// The underlying [`PatternError`].
    pub error: PatternError,
}

impl fmt::Display for PatternBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to compile pattern {:?} at index {}: {}",
            self.pattern, self.index, self.error,
        )
    }
}

impl Error for PatternBuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

/// An error returned by [`PatternFilter::filter_normalized`] when the
/// normalizer fails for an input string.
///
/// When this error is returned the whole batch was rejected and no partial
/// results are available.
#[derive(Debug)]
pub struct NormalizeError<E = std::convert::Infallible> {
    /// Zero-based index of the input string for which normalization failed.
    pub index: usize,

    /// The original input string for which normalization failed.
    pub path: String,

    /// The error produced by the normalizer.
    pub source: E,
}

impl<E: fmt::Display> fmt::Display for NormalizeError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "normalization failed for path {:?} at index {}: {}",
            self.path, self.index, self.source,
        )
    }
}

impl<E: Error + 'static> Error for NormalizeError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns_of<'a>(filter: &'a PatternFilter, hits: &[MatchedPath]) -> Vec<Vec<&'a str>> {
        hits.iter()
            .map(|hit| hit.source_patterns(filter))
            .collect()
    }

    #[test]
    fn multiple_sources_and_dedup() {
        let filter = PatternFilter::new(["src/*", "**/*.rs"]).unwrap();
        let hits = filter.filter(["src/a.rs", "src/a.rs", "src/a.rs"]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path(), "src/a.rs");
        assert_eq!(hits[0].source_indices(), &[0, 1]);
        assert_eq!(
            patterns_of(&filter, &hits),
            vec![vec!["src/*", "**/*.rs"]]
        );
    }

    #[test]
    fn unmatched_paths_are_dropped() {
        let filter = PatternFilter::new(["*.rs"]).unwrap();
        let hits = filter.filter(["a.rs", "b.txt", "c.rs"]);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path(), "a.rs");
        assert_eq!(hits[1].path(), "c.rs");
    }

    #[test]
    fn case_significant_by_default() {
        let filter = PatternFilter::new(["*.rs"]).unwrap();
        let hits = filter.filter(["A.rs", "a.rs"]);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path(), "A.rs");
        assert_eq!(hits[1].path(), "a.rs");
    }

    #[test]
    fn separator_significant_by_default() {
        let filter = PatternFilter::new(["src/*"]).unwrap();
        assert!(filter.matches("src/a.rs"));
        assert!(!filter.matches("src/a/b.rs"));

        let relaxed = PatternFilter::with_options(
            ["src/*"],
            MatchOptions {
                case_sensitive: true,
                require_literal_separator: false,
                require_literal_leading_dot: false,
            },
        )
        .unwrap();
        assert!(relaxed.matches("src/a/b.rs"));
    }

    #[test]
    fn stable_ordering_follows_first_seen_input() {
        let filter = PatternFilter::new(["*"]).unwrap();
        let hits = filter.filter(["c", "a", "b", "a", "c"]);
        let paths: Vec<&str> = hits.iter().map(MatchedPath::path).collect();
        assert_eq!(paths, vec!["c", "a", "b"]);
    }

    #[test]
    fn normalized_identity_merges_sources_from_distinct_strings() {
        // "FOObar" matches only "FOO*", "foobar" matches only "*bar"; both
        // fold to the same identity, so the single merged result carries both
        // sources in declaration order and keeps the first original string.
        let filter = PatternFilter::new(["FOO*", "*bar"]).unwrap();
        let hits = filter
            .filter_normalized(["FOObar", "foobar"], |s| {
                Ok::<_, std::convert::Infallible>(s.to_ascii_lowercase())
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path(), "FOObar");
        assert_eq!(hits[0].source_indices(), &[0, 1]);
        assert_eq!(
            patterns_of(&filter, &hits),
            vec![vec!["FOO*", "*bar"]]
        );
    }

    #[test]
    fn normalizer_failure_rejects_whole_batch() {
        let filter = PatternFilter::new(["*"]).unwrap();
        let result = filter.filter_normalized(["ok", "bad", "also-ok"], |s| {
            if s == "bad" {
                Err("bad path")
            } else {
                Ok(s.to_owned())
            }
        });
        let error = result.unwrap_err();
        assert_eq!(error.index, 1);
        assert_eq!(error.path, "bad");
        assert_eq!(error.source, "bad path");
    }

    #[test]
    fn normalizer_receives_originals_and_matching_uses_originals() {
        let filter = PatternFilter::new(["UPPER*"]).unwrap();
        let seen = std::cell::RefCell::new(Vec::new());
        let hits = filter
            .filter_normalized(["UPPER", "upper"], |s| {
                seen.borrow_mut().push(s.to_owned());
                Ok::<_, std::convert::Infallible>(s.to_ascii_lowercase())
            })
            .unwrap();
        assert_eq!(&*seen.borrow(), &["UPPER", "upper"]);
        // Only the original "UPPER" matches the case-sensitive pattern, but the
        // merged group still reports that match.
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path(), "UPPER");
        assert_eq!(hits[0].source_indices(), &[0]);
    }

    #[test]
    fn empty_groups() {
        // No patterns at all: nothing matches.
        let filter = PatternFilter::new(std::iter::empty::<&str>()).unwrap();
        assert!(filter.filter(["a", "b"]).is_empty());
        assert!(!filter.matches("a"));

        // Patterns but no hits.
        let filter = PatternFilter::new(["*.rs"]).unwrap();
        assert!(filter.filter(["a.txt", "b.md"]).is_empty());

        // No inputs.
        assert!(filter.filter(std::iter::empty::<&str>()).is_empty());
    }

    #[test]
    fn invalid_pattern_is_reported_with_index() {
        let error = PatternFilter::new(["ok", "**a"]).unwrap_err();
        assert_eq!(error.index, 1);
        assert_eq!(error.pattern, "**a");
        assert!(Error::source(&error).is_some());
    }

    #[test]
    fn display_and_deref_show_original_path() {
        let filter = PatternFilter::new(["*"]).unwrap();
        let hits = filter.filter(["a.rs"]);
        assert_eq!(hits[0].to_string(), "a.rs");
        assert_eq!(&*hits[0], "a.rs");
        assert_eq!(hits[0].as_ref(), "a.rs");
    }
}


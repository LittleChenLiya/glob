// Copyright 2014 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Support for matching file paths against Unix shell style patterns.
//!
//! The `glob` and `glob_with` functions allow querying the filesystem for all
//! files that match a particular pattern (similar to the libc `glob` function).
//! The methods on the `Pattern` type provide functionality for checking if
//! individual paths match a particular pattern (similar to the libc `fnmatch`
//! function).
//!
//! For consistency across platforms, and for Windows support, this module
//! is implemented entirely in Rust rather than deferring to the libc
//! `glob`/`fnmatch` functions.
//!
//! # Examples
//!
//! To print all jpg files in `/media/` and all of its subdirectories.
//!
//! ```rust,no_run
//! use glob::glob;
//!
//! for entry in glob("/media/**/*.jpg").expect("Failed to read glob pattern") {
//!     match entry {
//!         Ok(path) => println!("{:?}", path.display()),
//!         Err(e) => println!("{:?}", e),
//!     }
//! }
//! ```
//!
//! To print all files containing the letter "a", case insensitive, in a `local`
//! directory relative to the current working directory. This ignores errors
//! instead of printing them.
//!
//! ```rust,no_run
//! use glob::glob_with;
//! use glob::MatchOptions;
//!
//! let options = MatchOptions {
//!     case_sensitive: false,
//!     require_literal_separator: false,
//!     require_literal_leading_dot: false,
//! };
//! for entry in glob_with("local/*a*", options).unwrap() {
//!     if let Ok(path) = entry {
//!         println!("{:?}", path.display())
//!     }
//! }
//! ```

#![doc(
    html_logo_url = "https://www.rust-lang.org/logos/rust-logo-128x128-blk-v2.png",
    html_favicon_url = "https://www.rust-lang.org/favicon.ico",
    html_root_url = "https://docs.rs/glob/0.3.1"
)]
#![deny(missing_docs)]
#![allow(clippy::while_let_loop)]

#[cfg(test)]
doc_comment::doctest!("../README.md");

use std::cmp;
use std::cmp::Ordering;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::fs::DirEntry;
use std::io;
use std::ops::Deref;
use std::path::{self, Component, Path, PathBuf};
use std::str::FromStr;

use CharSpecifier::{CharRange, SingleChar};
use MatchResult::{EntirePatternDoesntMatch, Match, SubPatternDoesntMatch};
use PatternToken::AnyExcept;
use PatternToken::{AnyChar, AnyRecursiveSequence, AnySequence, AnyWithin, Char};

/// An iterator that yields `Path`s from the filesystem that match a particular
/// pattern.
///
/// Note that it yields `GlobResult` in order to report any `IoErrors` that may
/// arise during iteration. If a directory matches but is unreadable,
/// thereby preventing its contents from being checked for matches, a
/// `GlobError` is returned to express this.
///
/// See the `glob` function for more details.
#[derive(Debug)]
pub struct Paths {
    dir_patterns: Vec<Pattern>,
    require_dir: bool,
    options: MatchOptions,
    todo: Vec<Result<(PathWrapper, usize), GlobError>>,
    scope: Option<PathWrapper>,
}

/// Return an iterator that produces all the `Path`s that match the given
/// pattern using default match options, which may be absolute or relative to
/// the current working directory.
///
/// This may return an error if the pattern is invalid.
///
/// This method uses the default match options and is equivalent to calling
/// `glob_with(pattern, MatchOptions::new())`. Use `glob_with` directly if you
/// want to use non-default match options.
///
/// When iterating, each result is a `GlobResult` which expresses the
/// possibility that there was an `IoError` when attempting to read the contents
/// of the matched path.  In other words, each item returned by the iterator
/// will either be an `Ok(Path)` if the path matched, or an `Err(GlobError)` if
/// the path (partially) matched _but_ its contents could not be read in order
/// to determine if its contents matched.
///
/// See the `Paths` documentation for more information.
///
/// # Examples
///
/// Consider a directory `/media/pictures` containing only the files
/// `kittens.jpg`, `puppies.jpg` and `hamsters.gif`:
///
/// ```rust,no_run
/// use glob::glob;
///
/// for entry in glob("/media/pictures/*.jpg").unwrap() {
///     match entry {
///         Ok(path) => println!("{:?}", path.display()),
///
///         // if the path matched but was unreadable,
///         // thereby preventing its contents from matching
///         Err(e) => println!("{:?}", e),
///     }
/// }
/// ```
///
/// The above code will print:
///
/// ```ignore
/// /media/pictures/kittens.jpg
/// /media/pictures/puppies.jpg
/// ```
///
/// If you want to ignore unreadable paths, you can use something like
/// `filter_map`:
///
/// ```rust
/// use glob::glob;
/// use std::result::Result;
///
/// for path in glob("/media/pictures/*.jpg").unwrap().filter_map(Result::ok) {
///     println!("{}", path.display());
/// }
/// ```
/// Paths are yielded in alphabetical order.
pub fn glob(pattern: &str) -> Result<Paths, PatternError> {
    glob_with(pattern, MatchOptions::new())
}

/// Return an iterator that produces all the `Path`s that match the given
/// pattern using the specified match options, which may be absolute or relative
/// to the current working directory.
///
/// This may return an error if the pattern is invalid.
///
/// This function accepts Unix shell style patterns as described by
/// `Pattern::new(..)`.  The options given are passed through unchanged to
/// `Pattern::matches_with(..)` with the exception that
/// `require_literal_separator` is always set to `true` regardless of the value
/// passed to this function.
///
/// Paths are yielded in alphabetical order.
pub fn glob_with(pattern: &str, options: MatchOptions) -> Result<Paths, PatternError> {
    #[cfg(windows)]
    fn check_windows_verbatim(p: &Path) -> bool {
        match p.components().next() {
            Some(Component::Prefix(ref p)) => {
                // Allow VerbatimDisk paths. std canonicalize() generates them, and they work fine
                p.kind().is_verbatim()
                    && if let std::path::Prefix::VerbatimDisk(_) = p.kind() {
                        false
                    } else {
                        true
                    }
            }
            _ => false,
        }
    }
    #[cfg(not(windows))]
    fn check_windows_verbatim(_: &Path) -> bool {
        false
    }

    #[cfg(windows)]
    fn to_scope(p: &Path) -> PathBuf {
        // FIXME handle volume relative paths here
        p.to_path_buf()
    }
    #[cfg(not(windows))]
    fn to_scope(p: &Path) -> PathBuf {
        p.to_path_buf()
    }

    // make sure that the pattern is valid first, else early return with error
    let _ = Pattern::new(pattern)?;

    let mut components = Path::new(pattern).components().peekable();
    loop {
        match components.peek() {
            Some(&Component::Prefix(..)) | Some(&Component::RootDir) => {
                components.next();
            }
            _ => break,
        }
    }
    let rest = components.map(|s| s.as_os_str()).collect::<PathBuf>();
    let normalized_pattern = Path::new(pattern).iter().collect::<PathBuf>();
    let root_len = normalized_pattern.to_str().unwrap().len() - rest.to_str().unwrap().len();
    let root = if root_len > 0 {
        Some(Path::new(&pattern[..root_len]))
    } else {
        None
    };

    if root_len > 0 && check_windows_verbatim(root.unwrap()) {
        // FIXME: How do we want to handle verbatim paths? I'm inclined to
        // return nothing, since we can't very well find all UNC shares with a
        // 1-letter server name.
        return Ok(Paths {
            dir_patterns: Vec::new(),
            require_dir: false,
            options,
            todo: Vec::new(),
            scope: None,
        });
    }

    let scope = root.map_or_else(|| PathBuf::from("."), to_scope);
    let scope = PathWrapper::from_path(scope);

    let mut dir_patterns = Vec::new();
    let components =
        pattern[cmp::min(root_len, pattern.len())..].split_terminator(path::is_separator);

    for component in components {
        dir_patterns.push(Pattern::new(component)?);
    }

    if root_len == pattern.len() {
        dir_patterns.push(Pattern {
            original: "".to_string(),
            tokens: Vec::new(),
            is_recursive: false,
            has_metachars: false,
            segments: Vec::new(),
        });
    }

    let last_is_separator = pattern.chars().next_back().map(path::is_separator);
    let require_dir = last_is_separator == Some(true);
    let todo = Vec::new();

    Ok(Paths {
        dir_patterns,
        require_dir,
        options,
        todo,
        scope: Some(scope),
    })
}

/// A glob iteration error.
///
/// This is typically returned when a particular path cannot be read
/// to determine if its contents match the glob pattern. This is possible
/// if the program lacks the appropriate permissions, for example.
#[derive(Debug)]
pub struct GlobError {
    path: PathBuf,
    error: io::Error,
}

impl GlobError {
    /// The Path that the error corresponds to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The error in question.
    pub fn error(&self) -> &io::Error {
        &self.error
    }

    /// Consumes self, returning the _raw_ underlying `io::Error`
    #[deprecated(note = "use `.into` instead")]
    pub fn into_error(self) -> io::Error {
        self.error
    }
}

impl From<GlobError> for io::Error {
    fn from(value: GlobError) -> Self {
        value.error
    }
}

impl Error for GlobError {
    #[allow(deprecated)]
    fn description(&self) -> &str {
        self.error.description()
    }

    #[allow(unknown_lints, bare_trait_objects)]
    fn cause(&self) -> Option<&dyn Error> {
        Some(&self.error)
    }
}

impl fmt::Display for GlobError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "attempting to read `{}` resulted in an error: {}",
            self.path.display(),
            self.error
        )
    }
}

#[derive(Debug)]
struct PathWrapper {
    path: PathBuf,
    is_directory: bool,
}

impl PathWrapper {
    fn from_dir_entry(path: PathBuf, e: DirEntry) -> Self {
        let is_directory = e
            .file_type()
            .ok()
            .and_then(|file_type| {
                // We need to use fs::metadata to resolve the actual path
                // if it's a symlink.
                if file_type.is_symlink() {
                    None
                } else {
                    Some(file_type.is_dir())
                }
            })
            .or_else(|| fs::metadata(&path).map(|m| m.is_dir()).ok())
            .unwrap_or(false);
        Self { path, is_directory }
    }
    fn from_path(path: PathBuf) -> Self {
        let is_directory = fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false);
        Self { path, is_directory }
    }

    fn into_path(self) -> PathBuf {
        self.path
    }
}

impl Deref for PathWrapper {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.path.deref()
    }
}

impl AsRef<Path> for PathWrapper {
    fn as_ref(&self) -> &Path {
        self.path.as_ref()
    }
}

/// An alias for a glob iteration result.
///
/// This represents either a matched path or a glob iteration error,
/// such as failing to read a particular directory's contents.
pub type GlobResult = Result<PathBuf, GlobError>;

impl Iterator for Paths {
    type Item = GlobResult;

    fn next(&mut self) -> Option<GlobResult> {
        // the todo buffer hasn't been initialized yet, so it's done at this
        // point rather than in glob() so that the errors are unified that is,
        // failing to fill the buffer is an iteration error construction of the
        // iterator (i.e. glob()) only fails if it fails to compile the Pattern
        if let Some(scope) = self.scope.take() {
            if !self.dir_patterns.is_empty() {
                // Shouldn't happen, but we're using -1 as a special index.
                assert!(self.dir_patterns.len() < usize::MAX);

                fill_todo(&mut self.todo, &self.dir_patterns, 0, &scope, self.options);
            }
        }

        loop {
            if self.dir_patterns.is_empty() || self.todo.is_empty() {
                return None;
            }

            let (path, mut idx) = match self.todo.pop().unwrap() {
                Ok(pair) => pair,
                Err(e) => return Some(Err(e)),
            };

            // idx -1: was already checked by fill_todo, maybe path was '.' or
            // '..' that we can't match here because of normalization.
            if idx == usize::MAX {
                if self.require_dir && !path.is_directory {
                    continue;
                }
                return Some(Ok(path.into_path()));
            }

            if self.dir_patterns[idx].is_recursive {
                let mut next = idx;

                // collapse consecutive recursive patterns
                while (next + 1) < self.dir_patterns.len()
                    && self.dir_patterns[next + 1].is_recursive
                {
                    next += 1;
                }

                if path.is_directory {
                    // the path is a directory, so it's a match

                    // push this directory's contents
                    fill_todo(
                        &mut self.todo,
                        &self.dir_patterns,
                        next,
                        &path,
                        self.options,
                    );

                    if next == self.dir_patterns.len() - 1 {
                        // pattern ends in recursive pattern, so return this
                        // directory as a result
                        return Some(Ok(path.into_path()));
                    } else {
                        // advanced to the next pattern for this path
                        idx = next + 1;
                    }
                } else if next == self.dir_patterns.len() - 1 {
                    // not a directory and it's the last pattern, meaning no
                    // match
                    continue;
                } else {
                    // advanced to the next pattern for this path
                    idx = next + 1;
                }
            }

            // not recursive, so match normally
            if self.dir_patterns[idx].matches_with(
                {
                    match path.file_name().and_then(|s| s.to_str()) {
                        // FIXME (#9639): How do we handle non-utf8 filenames?
                        // Ignore them for now; ideally we'd still match them
                        // against a *
                        None => continue,
                        Some(x) => x,
                    }
                },
                self.options,
            ) {
                if idx == self.dir_patterns.len() - 1 {
                    // it is not possible for a pattern to match a directory
                    // *AND* its children so we don't need to check the
                    // children

                    if !self.require_dir || path.is_directory {
                        return Some(Ok(path.into_path()));
                    }
                } else {
                    fill_todo(
                        &mut self.todo,
                        &self.dir_patterns,
                        idx + 1,
                        &path,
                        self.options,
                    );
                }
            }
        }
    }
}

/// A pattern parsing error.
#[derive(Debug)]
#[allow(missing_copy_implementations)]
pub struct PatternError {
    /// The approximate character index of where the error occurred.
    pub pos: usize,

    /// A message describing the error.
    pub msg: &'static str,
}

impl Error for PatternError {
    fn description(&self) -> &str {
        self.msg
    }
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Pattern syntax error near position {}: {}",
            self.pos, self.msg
        )
    }
}

/// A compiled Unix shell style pattern.
///
/// - `?` matches any single character.
///
/// - `*` matches any (possibly empty) sequence of characters.
///
/// - `**` matches the current directory and arbitrary
///   subdirectories. To match files in arbitrary subdirectories, use
///   `**/*`.
///
///   This sequence **must** form a single path component, so both
///   `**a` and `b**` are invalid and will result in an error.  A
///   sequence of more than two consecutive `*` characters is also
///   invalid.
///
/// - `[...]` matches any character inside the brackets.  Character sequences
///   can also specify ranges of characters, as ordered by Unicode, so e.g.
///   `[0-9]` specifies any character between 0 and 9 inclusive. An unclosed
///   bracket is invalid.
///
/// - `[!...]` is the negation of `[...]`, i.e. it matches any characters
///   **not** in the brackets.
///
/// - The metacharacters `?`, `*`, `[`, `]` can be matched by using brackets
///   (e.g. `[?]`).  When a `]` occurs immediately following `[` or `[!` then it
///   is interpreted as being part of, rather then ending, the character set, so
///   `]` and NOT `]` can be matched by `[]]` and `[!]]` respectively.  The `-`
///   character can be specified inside a character sequence pattern by placing
///   it at the start or the end, e.g. `[abc-]`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
pub struct Pattern {
    original: String,
    tokens: Vec<PatternToken>,
    is_recursive: bool,
    /// A bool value that indicates whether the pattern contains any metacharacters.
    /// We use this information for some fast path optimizations.
    has_metachars: bool,
    /// The structure segments of this pattern, recorded by the parser.
    segments: Vec<PatternSegment>,
}

/// Show the original glob pattern.
impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.original.fmt(f)
    }
}

impl FromStr for Pattern {
    type Err = PatternError;

    fn from_str(s: &str) -> Result<Self, PatternError> {
        Self::new(s)
    }
}

/// A single segment of a compiled [`Pattern`], as recorded by a [`PatternIndex`].
///
/// Each segment records the byte range it occupies in the original pattern
/// text and its decoded literal value. Literal segments group together runs
/// of ordinary characters; escape segments record a backslash-escaped
/// metacharacter; character-class segments record a `[...]` bracket expression
/// (including negated classes); wildcard segments record `*`, `**`, or `?`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PatternSegment {
    kind: SegmentKind,
    start: usize,
    end: usize,
    decoded: String,
}

impl PatternSegment {
    /// Returns the kind of this segment.
    pub fn kind(&self) -> SegmentKind {
        self.kind
    }

    /// Returns the byte start offset of this segment in the original pattern.
    ///
    /// The offset is measured in bytes from the beginning of the pattern's
    /// UTF-8 encoded text.
    pub fn start(&self) -> usize {
        self.start
    }

    /// Returns the byte end offset (exclusive) of this segment in the original
    /// pattern.
    ///
    /// The offset is measured in bytes from the beginning of the pattern's
    /// UTF-8 encoded text.
    pub fn end(&self) -> usize {
        self.end
    }

    /// Returns the decoded literal value of this segment.
    ///
    /// For literal and escape segments, this is the literal text they match.
    /// For character classes and wildcards, this is the original text of the
    /// segment (since they do not decode to a single literal).
    pub fn decoded(&self) -> &str {
        &self.decoded
    }
}

/// The kind of a segment in a [`PatternIndex`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum SegmentKind {
    /// A run of literal characters.
    Literal,
    /// A backslash-escaped metacharacter, e.g. `\*` or `\?`.
    Escape,
    /// A character class, e.g. `[a-z]` or `[!abc]`.
    CharClass,
    /// A wildcard, e.g. `*`, `**`, or `?`.
    Wildcard,
}

/// A structure index for a compiled [`Pattern`].
///
/// `PatternIndex` breaks a pattern down into a sequence of typed segments,
/// each recording its byte range in the original pattern text and its
/// decoded literal value. The index is produced by the same parser that
/// compiles the pattern, so it accurately reflects the pattern's structure
/// (for example, a `*` inside a character class is recorded as part of the
/// class, not as a wildcard).
///
/// This is useful for tooling that needs to inspect or edit specific parts
/// of a pattern while verifying that the underlying pattern has not changed.
///
/// # Examples
///
/// ```rust
/// use glob::Pattern;
///
/// let pattern = Pattern::new("a\\*/[x*]/b?").unwrap();
/// let index = pattern.index();
///
/// assert_eq!(index.baseline(), "a\\*/[x*]/b?");
/// assert_eq!(index.len(), 6);
/// ```
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PatternIndex {
    baseline: String,
    segments: Vec<PatternSegment>,
}

impl PatternIndex {
    /// Returns the original pattern text that this index was built from.
    pub fn baseline(&self) -> &str {
        &self.baseline
    }

    /// Returns the segments of this index, in order.
    pub fn segments(&self) -> &[PatternSegment] {
        &self.segments
    }

    /// Returns the number of segments in this index.
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// Returns `true` if this index has no segments.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Verifies that this index is still valid for the given pattern.
    ///
    /// Returns `true` if the pattern's original text matches the baseline
    /// this index was built from. Use this to detect when a pattern has
    /// been changed in a way that invalidates the index (for example, after
    /// editing a different part of the pattern).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use glob::Pattern;
    ///
    /// let pattern = Pattern::new("a\\*/[x*]/b?").unwrap();
    /// let index = pattern.index();
    /// assert!(index.verify(&pattern));
    ///
    /// let changed = Pattern::new("x\\*/[x*]/b?").unwrap();
    /// assert!(!index.verify(&changed));
    /// ```
    pub fn verify(&self, pattern: &Pattern) -> bool {
        pattern.as_str() == self.baseline
    }

    /// Edits the literal segment at the given index, replacing its text with
    /// the given value.
    ///
    /// The new value is escaped (using [`Pattern::escape`]) so that it remains
    /// a literal segment. The rest of the pattern is left unchanged. Returns a
    /// new compiled pattern, or an error if the segment is not a literal or
    /// the edited pattern fails to compile.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use glob::Pattern;
    ///
    /// let pattern = Pattern::new("a\\*/[x*]/b?").unwrap();
    /// let index = pattern.index();
    ///
    /// // Edit the first literal segment ("a") to "a2".
    /// let edited = index.edit_literal(0, "a2").unwrap();
    /// assert_eq!(edited.as_str(), "a2\\*/[x*]/b?");
    ///
    /// // The escaped asterisk is still a literal after the edit.
    /// assert!(edited.matches("a2*/x/b?"));
    /// ```
    pub fn edit_literal(
        &self,
        segment_idx: usize,
        new_value: &str,
    ) -> Result<Pattern, PatternError> {
        let segment = self.segments.get(segment_idx).ok_or(PatternError {
            pos: 0,
            msg: "segment index out of bounds",
        })?;
        if segment.kind != SegmentKind::Literal {
            return Err(PatternError {
                pos: segment.start,
                msg: "segment is not a literal",
            });
        }
        let escaped = Pattern::escape(new_value);
        let mut new_pattern = String::with_capacity(self.baseline.len() + escaped.len());
        new_pattern.push_str(&self.baseline[..segment.start]);
        new_pattern.push_str(&escaped);
        new_pattern.push_str(&self.baseline[segment.end..]);
        Pattern::new(&new_pattern)
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
enum PatternToken {
    Char(char),
    AnyChar,
    AnySequence,
    AnyRecursiveSequence,
    AnyWithin(Vec<CharSpecifier>),
    AnyExcept(Vec<CharSpecifier>),
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
enum CharSpecifier {
    SingleChar(char),
    CharRange(char, char),
}

#[derive(Copy, Clone, PartialEq)]
enum MatchResult {
    Match,
    SubPatternDoesntMatch,
    EntirePatternDoesntMatch,
}

const ERROR_WILDCARDS: &str = "wildcards are either regular `*` or recursive `**`";
const ERROR_RECURSIVE_WILDCARDS: &str = "recursive wildcards must form a single path \
                                         component";
const ERROR_INVALID_RANGE: &str = "invalid range pattern";

impl Pattern {
    /// This function compiles Unix shell style patterns.
    ///
    /// An invalid glob pattern will yield a `PatternError`.
    pub fn new(pattern: &str) -> Result<Self, PatternError> {
        let chars = pattern.chars().collect::<Vec<_>>();
        // Byte offset of each char in the original UTF-8 pattern text.
        let byte_offsets: Vec<usize> = {
            let mut offsets = Vec::with_capacity(chars.len());
            let mut pos = 0;
            for c in &chars {
                offsets.push(pos);
                pos += c.len_utf8();
            }
            offsets
        };
        let mut tokens = Vec::new();
        let mut segments = Vec::new();
        let mut is_recursive = false;
        let mut has_metachars = false;
        let mut i = 0;
        // Start char index of the current literal run, if any.
        let mut literal_start: Option<usize> = None;

        // Flush a pending literal run into a Literal segment.
        macro_rules! flush_literal {
            ($end:expr) => {
                if let Some(start) = literal_start.take() {
                    let start_byte = byte_offsets[start];
                    let end_byte = if $end < chars.len() {
                        byte_offsets[$end]
                    } else {
                        pattern.len()
                    };
                    let decoded: String = chars[start..$end].iter().collect();
                    segments.push(PatternSegment {
                        kind: SegmentKind::Literal,
                        start: start_byte,
                        end: end_byte,
                        decoded,
                    });
                }
            };
        }

        while i < chars.len() {
            match chars[i] {
                '?' => {
                    flush_literal!(i);
                    has_metachars = true;
                    tokens.push(AnyChar);
                    let start_byte = byte_offsets[i];
                    i += 1;
                    let end_byte = if i < chars.len() {
                        byte_offsets[i]
                    } else {
                        pattern.len()
                    };
                    segments.push(PatternSegment {
                        kind: SegmentKind::Wildcard,
                        start: start_byte,
                        end: end_byte,
                        decoded: "?".to_string(),
                    });
                }
                '*' => {
                    flush_literal!(i);
                    has_metachars = true;

                    let old = i;

                    while i < chars.len() && chars[i] == '*' {
                        i += 1;
                    }

                    let count = i - old;
                    let star_end = i;
                    let start_byte = byte_offsets[old];
                    let end_byte = if star_end < chars.len() {
                        byte_offsets[star_end]
                    } else {
                        pattern.len()
                    };

                    match count.cmp(&2) {
                        Ordering::Greater => {
                            return Err(PatternError {
                                pos: old + 2,
                                msg: ERROR_WILDCARDS,
                            })
                        }
                        Ordering::Equal => {
                            // ** can only be an entire path component
                            // i.e. a/**/b is valid, but a**/b or a/**b is not
                            // invalid matches are treated literally
                            let is_valid = if star_end == 2
                                || path::is_separator(chars[star_end - count - 1])
                            {
                                // it ends in a '/'
                                if star_end < chars.len() && path::is_separator(chars[star_end]) {
                                    i += 1;
                                    true
                                // or the pattern ends here
                                // this enables the existing globbing mechanism
                                } else if star_end == chars.len() {
                                    true
                                // `**` ends in non-separator
                                } else {
                                    return Err(PatternError {
                                        pos: star_end,
                                        msg: ERROR_RECURSIVE_WILDCARDS,
                                    });
                                }
                            // `**` begins with non-separator
                            } else {
                                return Err(PatternError {
                                    pos: old - 1,
                                    msg: ERROR_RECURSIVE_WILDCARDS,
                                });
                            };

                            if is_valid {
                                // collapse consecutive AnyRecursiveSequence to a
                                // single one

                                let tokens_len = tokens.len();

                                if !(tokens_len > 1
                                    && tokens[tokens_len - 1] == AnyRecursiveSequence)
                                {
                                    is_recursive = true;
                                    tokens.push(AnyRecursiveSequence);
                                }

                                segments.push(PatternSegment {
                                    kind: SegmentKind::Wildcard,
                                    start: start_byte,
                                    end: end_byte,
                                    decoded: "**".to_string(),
                                });
                            }
                        }
                        Ordering::Less => {
                            tokens.push(AnySequence);
                            segments.push(PatternSegment {
                                kind: SegmentKind::Wildcard,
                                start: start_byte,
                                end: end_byte,
                                decoded: "*".to_string(),
                            });
                        }
                    }
                }
                '[' => {
                    flush_literal!(i);
                    has_metachars = true;
                    let class_start = i;

                    if i + 4 <= chars.len() && chars[i + 1] == '!' {
                        match chars[i + 3..].iter().position(|x| *x == ']') {
                            None => (),
                            Some(j) => {
                                let specifier_chars = &chars[i + 2..i + 3 + j];
                                let cs = parse_char_specifiers(specifier_chars);
                                tokens.push(AnyExcept(cs));
                                i += j + 4;
                                let end_byte = if i < chars.len() {
                                    byte_offsets[i]
                                } else {
                                    pattern.len()
                                };
                                segments.push(PatternSegment {
                                    kind: SegmentKind::CharClass,
                                    start: byte_offsets[class_start],
                                    end: end_byte,
                                    decoded: pattern[byte_offsets[class_start]..end_byte]
                                        .to_string(),
                                });
                                continue;
                            }
                        }
                    } else if i + 3 <= chars.len() && chars[i + 1] != '!' {
                        match chars[i + 2..].iter().position(|x| *x == ']') {
                            None => (),
                            Some(j) => {
                                let cs = parse_char_specifiers(&chars[i + 1..i + 2 + j]);
                                tokens.push(AnyWithin(cs));
                                i += j + 3;
                                let end_byte = if i < chars.len() {
                                    byte_offsets[i]
                                } else {
                                    pattern.len()
                                };
                                segments.push(PatternSegment {
                                    kind: SegmentKind::CharClass,
                                    start: byte_offsets[class_start],
                                    end: end_byte,
                                    decoded: pattern[byte_offsets[class_start]..end_byte]
                                        .to_string(),
                                });
                                continue;
                            }
                        }
                    }

                    // if we get here then this is not a valid range pattern
                    return Err(PatternError {
                        pos: i,
                        msg: ERROR_INVALID_RANGE,
                    });
                }
                '\\' => {
                    // A backslash followed by a metacharacter is an escape.
                    if i + 1 < chars.len()
                        && matches!(chars[i + 1], '*' | '?' | '[' | ']' | '\\')
                    {
                        flush_literal!(i);
                        let escaped = chars[i + 1];
                        tokens.push(Char(escaped));
                        let start_byte = byte_offsets[i];
                        i += 2;
                        let end_byte = if i < chars.len() {
                            byte_offsets[i]
                        } else {
                            pattern.len()
                        };
                        segments.push(PatternSegment {
                            kind: SegmentKind::Escape,
                            start: start_byte,
                            end: end_byte,
                            decoded: escaped.to_string(),
                        });
                    } else {
                        // A backslash not followed by a metachar is a literal.
                        if literal_start.is_none() {
                            literal_start = Some(i);
                        }
                        tokens.push(Char('\\'));
                        i += 1;
                    }
                }
                c => {
                    if literal_start.is_none() {
                        literal_start = Some(i);
                    }
                    tokens.push(Char(c));
                    i += 1;
                }
            }
        }

        flush_literal!(chars.len());

        Ok(Self {
            tokens,
            original: pattern.to_string(),
            is_recursive,
            has_metachars,
            segments,
        })
    }

    /// Builds a structure index for this pattern.
    ///
    /// The index breaks the pattern down into typed segments (literal text,
    /// escapes, character classes, and wildcards), each recording its byte
    /// range in the original pattern text and its decoded literal value.
    pub fn index(&self) -> PatternIndex {
        PatternIndex {
            baseline: self.original.clone(),
            segments: self.segments.clone(),
        }
    }

    /// Escape metacharacters within the given string by surrounding them in
    /// brackets. The resulting string will, when compiled into a `Pattern`,
    /// match the input string and nothing else.
    pub fn escape(s: &str) -> String {
        let mut escaped = String::new();
        for c in s.chars() {
            match c {
                // note that ! does not need escaping because it is only special
                // inside brackets
                '?' | '*' | '[' | ']' => {
                    escaped.push('[');
                    escaped.push(c);
                    escaped.push(']');
                }
                c => {
                    escaped.push(c);
                }
            }
        }
        escaped
    }

    /// Return if the given `str` matches this `Pattern` using the default
    /// match options (i.e. `MatchOptions::new()`).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use glob::Pattern;
    ///
    /// assert!(Pattern::new("c?t").unwrap().matches("cat"));
    /// assert!(Pattern::new("k[!e]tteh").unwrap().matches("kitteh"));
    /// assert!(Pattern::new("d*g").unwrap().matches("doog"));
    /// ```
    pub fn matches(&self, str: &str) -> bool {
        self.matches_with(str, MatchOptions::new())
    }

    /// Return if the given `Path`, when converted to a `str`, matches this
    /// `Pattern` using the default match options (i.e. `MatchOptions::new()`).
    pub fn matches_path(&self, path: &Path) -> bool {
        // FIXME (#9639): This needs to handle non-utf8 paths
        path.to_str().map_or(false, |s| self.matches(s))
    }

    /// Return if the given `str` matches this `Pattern` using the specified
    /// match options.
    pub fn matches_with(&self, str: &str, options: MatchOptions) -> bool {
        self.matches_from(true, str.chars(), 0, options) == Match
    }

    /// Return if the given `Path`, when converted to a `str`, matches this
    /// `Pattern` using the specified match options.
    pub fn matches_path_with(&self, path: &Path, options: MatchOptions) -> bool {
        // FIXME (#9639): This needs to handle non-utf8 paths
        path.to_str()
            .map_or(false, |s| self.matches_with(s, options))
    }

    /// Access the original glob pattern.
    pub fn as_str(&self) -> &str {
        &self.original
    }

    fn matches_from(
        &self,
        mut follows_separator: bool,
        mut file: std::str::Chars,
        i: usize,
        options: MatchOptions,
    ) -> MatchResult {
        for (ti, token) in self.tokens[i..].iter().enumerate() {
            match *token {
                AnySequence | AnyRecursiveSequence => {
                    // ** must be at the start.
                    debug_assert!(match *token {
                        AnyRecursiveSequence => follows_separator,
                        _ => true,
                    });

                    // Empty match
                    match self.matches_from(follows_separator, file.clone(), i + ti + 1, options) {
                        SubPatternDoesntMatch => (), // keep trying
                        m => return m,
                    };

                    while let Some(c) = file.next() {
                        if follows_separator && options.require_literal_leading_dot && c == '.' {
                            return SubPatternDoesntMatch;
                        }
                        follows_separator = path::is_separator(c);
                        match *token {
                            AnyRecursiveSequence if !follows_separator => continue,
                            AnySequence
                                if options.require_literal_separator && follows_separator =>
                            {
                                return SubPatternDoesntMatch
                            }
                            _ => (),
                        }
                        match self.matches_from(
                            follows_separator,
                            file.clone(),
                            i + ti + 1,
                            options,
                        ) {
                            SubPatternDoesntMatch => (), // keep trying
                            m => return m,
                        }
                    }
                }
                _ => {
                    let c = match file.next() {
                        Some(c) => c,
                        None => return EntirePatternDoesntMatch,
                    };

                    let is_sep = path::is_separator(c);

                    if !match *token {
                        AnyChar | AnyWithin(..) | AnyExcept(..)
                            if (options.require_literal_separator && is_sep)
                                || (follows_separator
                                    && options.require_literal_leading_dot
                                    && c == '.') =>
                        {
                            false
                        }
                        AnyChar => true,
                        AnyWithin(ref specifiers) => in_char_specifiers(specifiers, c, options),
                        AnyExcept(ref specifiers) => !in_char_specifiers(specifiers, c, options),
                        Char(c2) => chars_eq(c, c2, options.case_sensitive),
                        AnySequence | AnyRecursiveSequence => unreachable!(),
                    } {
                        return SubPatternDoesntMatch;
                    }
                    follows_separator = is_sep;
                }
            }
        }

        // Iter is fused.
        if file.next().is_none() {
            Match
        } else {
            SubPatternDoesntMatch
        }
    }
}

// Fills `todo` with paths under `path` to be matched by `patterns[idx]`,
// special-casing patterns to match `.` and `..`, and avoiding `readdir()`
// calls when there are no metacharacters in the pattern.
fn fill_todo(
    todo: &mut Vec<Result<(PathWrapper, usize), GlobError>>,
    patterns: &[Pattern],
    idx: usize,
    path: &PathWrapper,
    options: MatchOptions,
) {
    let add = |todo: &mut Vec<_>, next_path: PathWrapper| {
        if idx + 1 == patterns.len() {
            // We know it's good, so don't make the iterator match this path
            // against the pattern again. In particular, it can't match
            // . or .. globs since these never show up as path components.
            todo.push(Ok((next_path, usize::MAX)));
        } else {
            fill_todo(todo, patterns, idx + 1, &next_path, options);
        }
    };

    let pattern = &patterns[idx];
    let is_dir = path.is_directory;
    let curdir = path.as_ref() == Path::new(".");
    match (pattern.has_metachars, is_dir) {
        (false, _) => {
            debug_assert!(
                pattern
                    .tokens
                    .iter()
                    .all(|tok| matches!(tok, PatternToken::Char(_))),
                "broken invariant: pattern has metachars but shouldn't"
            );
            let s = pattern.as_str();

            // This pattern component doesn't have any metacharacters, so we
            // don't need to read the current directory to know where to
            // continue. So instead of passing control back to the iterator,
            // we can just check for that one entry and potentially recurse
            // right away.
            let special = "." == s || ".." == s;
            let next_path = if curdir {
                PathBuf::from(s)
            } else {
                path.join(s)
            };
            let next_path = PathWrapper::from_path(next_path);
            if (special && is_dir)
                || (!special
                    && (fs::metadata(&next_path).is_ok()
                        || fs::symlink_metadata(&next_path).is_ok()))
            {
                add(todo, next_path);
            }
        }
        (true, true) => {
            let dirs = fs::read_dir(path).and_then(|d| {
                d.map(|e| {
                    e.map(|e| {
                        // We sort by filename a few lines below. However, it is actually quite
                        // expensive to extract the filename from PathBuf. Since we already know the
                        // filename from `DirEntry` here, we store it proactively (we still
                        // have to heap allocate it).
                        // Then we use this cached filename for sorting, and further on work only
                        // with the full path.
                        let (path, filename) = if curdir {
                            (PathBuf::from(e.path().file_name().unwrap()), e.file_name())
                        } else {
                            (e.path(), e.file_name())
                        };
                        (PathWrapper::from_dir_entry(path, e), filename)
                    })
                })
                .collect::<Result<Vec<(PathWrapper, OsString)>, _>>()
            });
            match dirs {
                Ok(mut children) => {
                    if options.require_literal_leading_dot {
                        children.retain(|x| !x.1.to_str().unwrap().starts_with('.'));
                    }
                    children.sort_by(|p1, p2| p2.1.cmp(&p1.1));
                    todo.extend(children.into_iter().map(|x| Ok((x.0, idx))));

                    // Matching the special directory entries . and .. that
                    // refer to the current and parent directory respectively
                    // requires that the pattern has a leading dot, even if the
                    // `MatchOptions` field `require_literal_leading_dot` is not
                    // set.
                    if !pattern.tokens.is_empty() && pattern.tokens[0] == Char('.') {
                        for &special in &[".", ".."] {
                            if pattern.matches_with(special, options) {
                                add(todo, PathWrapper::from_path(path.join(special)));
                            }
                        }
                    }
                }
                Err(e) => {
                    todo.push(Err(GlobError {
                        path: path.to_path_buf(),
                        error: e,
                    }));
                }
            }
        }
        (true, false) => {
            // not a directory, nothing more to find
        }
    }
}

fn parse_char_specifiers(s: &[char]) -> Vec<CharSpecifier> {
    let mut cs = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if i + 3 <= s.len() && s[i + 1] == '-' {
            cs.push(CharRange(s[i], s[i + 2]));
            i += 3;
        } else {
            cs.push(SingleChar(s[i]));
            i += 1;
        }
    }
    cs
}

fn in_char_specifiers(specifiers: &[CharSpecifier], c: char, options: MatchOptions) -> bool {
    for &specifier in specifiers.iter() {
        match specifier {
            SingleChar(sc) => {
                if chars_eq(c, sc, options.case_sensitive) {
                    return true;
                }
            }
            CharRange(start, end) => {
                // FIXME: work with non-ascii chars properly (issue #1347)
                if !options.case_sensitive && c.is_ascii() && start.is_ascii() && end.is_ascii() {
                    let start = start.to_ascii_lowercase();
                    let end = end.to_ascii_lowercase();

                    let start_up = start.to_uppercase().next().unwrap();
                    let end_up = end.to_uppercase().next().unwrap();

                    // only allow case insensitive matching when
                    // both start and end are within a-z or A-Z
                    if start != start_up && end != end_up {
                        let c = c.to_ascii_lowercase();
                        if c >= start && c <= end {
                            return true;
                        }
                    }
                }

                if c >= start && c <= end {
                    return true;
                }
            }
        }
    }

    false
}

/// A helper function to determine if two chars are (possibly case-insensitively) equal.
fn chars_eq(a: char, b: char, case_sensitive: bool) -> bool {
    if cfg!(windows) && path::is_separator(a) && path::is_separator(b) {
        true
    } else if !case_sensitive && a.is_ascii() && b.is_ascii() {
        // FIXME: work with non-ascii chars properly (issue #9084)
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

/// Configuration options to modify the behaviour of `Pattern::matches_with(..)`.
#[allow(missing_copy_implementations)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct MatchOptions {
    /// Whether or not patterns should be matched in a case-sensitive manner.
    /// This currently only considers upper/lower case relationships between
    /// ASCII characters, but in future this might be extended to work with
    /// Unicode.
    pub case_sensitive: bool,

    /// Whether or not path-component separator characters (e.g. `/` on
    /// Posix) must be matched by a literal `/`, rather than by `*` or `?` or
    /// `[...]`.
    pub require_literal_separator: bool,

    /// Whether or not paths that contain components that start with a `.`
    /// will require that `.` appears literally in the pattern; `*`, `?`, `**`,
    /// or `[...]` will not match. This is useful because such files are
    /// conventionally considered hidden on Unix systems and it might be
    /// desirable to skip them when listing files.
    pub require_literal_leading_dot: bool,
}

impl MatchOptions {
    /// Constructs a new `MatchOptions` with default field values. This is used
    /// when calling functions that do not take an explicit `MatchOptions`
    /// parameter.
    ///
    /// This function always returns this value:
    ///
    /// ```rust,ignore
    /// MatchOptions {
    ///     case_sensitive: true,
    ///     require_literal_separator: false,
    ///     require_literal_leading_dot: false
    /// }
    /// ```
    ///
    /// # Note
    /// The behavior of this method doesn't match `default()`'s. This returns
    /// `case_sensitive` as `true` while `default()` does it as `false`.
    // FIXME: Consider unity the behavior with `default()` in a next major release.
    pub fn new() -> Self {
        Self {
            case_sensitive: true,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        }
    }
}

#[cfg(test)]
mod test {
    use super::{glob, MatchOptions, Pattern, SegmentKind};
    use std::path::Path;

    #[test]
    fn test_pattern_from_str() {
        assert!("a*b".parse::<Pattern>().unwrap().matches("a_b"));
        assert!("a/**b".parse::<Pattern>().unwrap_err().pos == 4);
    }

    #[test]
    fn test_wildcard_errors() {
        assert!(Pattern::new("a/**b").unwrap_err().pos == 4);
        assert!(Pattern::new("a/bc**").unwrap_err().pos == 3);
        assert!(Pattern::new("a/*****").unwrap_err().pos == 4);
        assert!(Pattern::new("a/b**c**d").unwrap_err().pos == 2);
        assert!(Pattern::new("a**b").unwrap_err().pos == 0);
    }

    #[test]
    fn test_unclosed_bracket_errors() {
        assert!(Pattern::new("abc[def").unwrap_err().pos == 3);
        assert!(Pattern::new("abc[!def").unwrap_err().pos == 3);
        assert!(Pattern::new("abc[").unwrap_err().pos == 3);
        assert!(Pattern::new("abc[!").unwrap_err().pos == 3);
        assert!(Pattern::new("abc[d").unwrap_err().pos == 3);
        assert!(Pattern::new("abc[!d").unwrap_err().pos == 3);
        assert!(Pattern::new("abc[]").unwrap_err().pos == 3);
        assert!(Pattern::new("abc[!]").unwrap_err().pos == 3);
    }

    #[test]
    fn test_glob_errors() {
        assert!(glob("a/**b").err().unwrap().pos == 4);
        assert!(glob("abc[def").err().unwrap().pos == 3);
    }

    // this test assumes that there is a /root directory and that
    // the user running this test is not root or otherwise doesn't
    // have permission to read its contents
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn test_iteration_errors() {
        use std::io;
        let mut iter = glob("/root/*").unwrap();

        // GlobErrors shouldn't halt iteration
        let next = iter.next();
        assert!(next.is_some());

        let err = next.unwrap();
        assert!(err.is_err());

        let err = err.err().unwrap();
        assert!(err.path() == Path::new("/root"));
        assert!(err.error().kind() == io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn test_absolute_pattern() {
        assert!(glob("/").unwrap().next().is_some());
        assert!(glob("//").unwrap().next().is_some());

        // assume that the filesystem is not empty!
        assert!(glob("/*").unwrap().next().is_some());

        #[cfg(not(windows))]
        fn win() {}

        #[cfg(windows)]
        fn win() {
            use std::env::current_dir;
            use std::path::Component;

            // check windows absolute paths with host/device components
            let root_with_device = current_dir()
                .ok()
                .and_then(|p| match p.components().next().unwrap() {
                    Component::Prefix(prefix_component) => {
                        let path = Path::new(prefix_component.as_os_str()).join("*");
                        Some(path.to_path_buf())
                    }
                    _ => panic!("no prefix in this path"),
                })
                .unwrap();
            // FIXME (#9639): This needs to handle non-utf8 paths
            assert!(glob(root_with_device.as_os_str().to_str().unwrap())
                .unwrap()
                .next()
                .is_some());
        }
        win()
    }

    #[test]
    fn test_wildcards() {
        assert!(Pattern::new("a*b").unwrap().matches("a_b"));
        assert!(Pattern::new("a*b*c").unwrap().matches("abc"));
        assert!(!Pattern::new("a*b*c").unwrap().matches("abcd"));
        assert!(Pattern::new("a*b*c").unwrap().matches("a_b_c"));
        assert!(Pattern::new("a*b*c").unwrap().matches("a___b___c"));
        assert!(Pattern::new("abc*abc*abc")
            .unwrap()
            .matches("abcabcabcabcabcabcabc"));
        assert!(!Pattern::new("abc*abc*abc")
            .unwrap()
            .matches("abcabcabcabcabcabcabca"));
        assert!(Pattern::new("a*a*a*a*a*a*a*a*a")
            .unwrap()
            .matches("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(Pattern::new("a*b[xyz]c*d").unwrap().matches("abxcdbxcddd"));
    }

    #[test]
    fn test_recursive_wildcards() {
        let pat = Pattern::new("some/**/needle.txt").unwrap();
        assert!(pat.matches("some/needle.txt"));
        assert!(pat.matches("some/one/needle.txt"));
        assert!(pat.matches("some/one/two/needle.txt"));
        assert!(pat.matches("some/other/needle.txt"));
        assert!(!pat.matches("some/other/notthis.txt"));

        // a single ** should be valid, for globs
        // Should accept anything
        let pat = Pattern::new("**").unwrap();
        assert!(pat.is_recursive);
        assert!(pat.matches("abcde"));
        assert!(pat.matches(""));
        assert!(pat.matches(".asdf"));
        assert!(pat.matches("/x/.asdf"));

        // collapse consecutive wildcards
        let pat = Pattern::new("some/**/**/needle.txt").unwrap();
        assert!(pat.matches("some/needle.txt"));
        assert!(pat.matches("some/one/needle.txt"));
        assert!(pat.matches("some/one/two/needle.txt"));
        assert!(pat.matches("some/other/needle.txt"));
        assert!(!pat.matches("some/other/notthis.txt"));

        // ** can begin the pattern
        let pat = Pattern::new("**/test").unwrap();
        assert!(pat.matches("one/two/test"));
        assert!(pat.matches("one/test"));
        assert!(pat.matches("test"));

        // /** can begin the pattern
        let pat = Pattern::new("/**/test").unwrap();
        assert!(pat.matches("/one/two/test"));
        assert!(pat.matches("/one/test"));
        assert!(pat.matches("/test"));
        assert!(!pat.matches("/one/notthis"));
        assert!(!pat.matches("/notthis"));

        // Only start sub-patterns on start of path segment.
        let pat = Pattern::new("**/.*").unwrap();
        assert!(pat.matches(".abc"));
        assert!(pat.matches("abc/.abc"));
        assert!(!pat.matches("ab.c"));
        assert!(!pat.matches("abc/ab.c"));
    }

    #[test]
    fn test_lots_of_files() {
        // this is a good test because it touches lots of differently named files
        glob("/*/*/*/*").unwrap().skip(10000).next();
    }

    #[test]
    fn test_range_pattern() {
        let pat = Pattern::new("a[0-9]b").unwrap();
        for i in 0..10 {
            assert!(pat.matches(&format!("a{}b", i)));
        }
        assert!(!pat.matches("a_b"));

        let pat = Pattern::new("a[!0-9]b").unwrap();
        for i in 0..10 {
            assert!(!pat.matches(&format!("a{}b", i)));
        }
        assert!(pat.matches("a_b"));

        let pats = ["[a-z123]", "[1a-z23]", "[123a-z]"];
        for &p in pats.iter() {
            let pat = Pattern::new(p).unwrap();
            for c in "abcdefghijklmnopqrstuvwxyz".chars() {
                assert!(pat.matches(&c.to_string()));
            }
            for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ".chars() {
                let options = MatchOptions {
                    case_sensitive: false,
                    ..MatchOptions::new()
                };
                assert!(pat.matches_with(&c.to_string(), options));
            }
            assert!(pat.matches("1"));
            assert!(pat.matches("2"));
            assert!(pat.matches("3"));
        }

        let pats = ["[abc-]", "[-abc]", "[a-c-]"];
        for &p in pats.iter() {
            let pat = Pattern::new(p).unwrap();
            assert!(pat.matches("a"));
            assert!(pat.matches("b"));
            assert!(pat.matches("c"));
            assert!(pat.matches("-"));
            assert!(!pat.matches("d"));
        }

        let pat = Pattern::new("[2-1]").unwrap();
        assert!(!pat.matches("1"));
        assert!(!pat.matches("2"));

        assert!(Pattern::new("[-]").unwrap().matches("-"));
        assert!(!Pattern::new("[!-]").unwrap().matches("-"));
    }

    #[test]
    fn test_pattern_matches() {
        let txt_pat = Pattern::new("*hello.txt").unwrap();
        assert!(txt_pat.matches("hello.txt"));
        assert!(txt_pat.matches("gareth_says_hello.txt"));
        assert!(txt_pat.matches("some/path/to/hello.txt"));
        assert!(txt_pat.matches("some\\path\\to\\hello.txt"));
        assert!(txt_pat.matches("/an/absolute/path/to/hello.txt"));
        assert!(!txt_pat.matches("hello.txt-and-then-some"));
        assert!(!txt_pat.matches("goodbye.txt"));

        let dir_pat = Pattern::new("*some/path/to/hello.txt").unwrap();
        assert!(dir_pat.matches("some/path/to/hello.txt"));
        assert!(dir_pat.matches("a/bigger/some/path/to/hello.txt"));
        assert!(!dir_pat.matches("some/path/to/hello.txt-and-then-some"));
        assert!(!dir_pat.matches("some/other/path/to/hello.txt"));
    }

    #[test]
    fn test_pattern_escape() {
        let s = "_[_]_?_*_!_";
        assert_eq!(Pattern::escape(s), "_[[]_[]]_[?]_[*]_!_".to_string());
        assert!(Pattern::new(&Pattern::escape(s)).unwrap().matches(s));
    }

    #[test]
    fn test_pattern_matches_case_insensitive() {
        let pat = Pattern::new("aBcDeFg").unwrap();
        let options = MatchOptions {
            case_sensitive: false,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        };

        assert!(pat.matches_with("aBcDeFg", options));
        assert!(pat.matches_with("abcdefg", options));
        assert!(pat.matches_with("ABCDEFG", options));
        assert!(pat.matches_with("AbCdEfG", options));
    }

    #[test]
    fn test_pattern_matches_case_insensitive_range() {
        let pat_within = Pattern::new("[a]").unwrap();
        let pat_except = Pattern::new("[!a]").unwrap();

        let options_case_insensitive = MatchOptions {
            case_sensitive: false,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        };
        let options_case_sensitive = MatchOptions {
            case_sensitive: true,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        };

        assert!(pat_within.matches_with("a", options_case_insensitive));
        assert!(pat_within.matches_with("A", options_case_insensitive));
        assert!(!pat_within.matches_with("A", options_case_sensitive));

        assert!(!pat_except.matches_with("a", options_case_insensitive));
        assert!(!pat_except.matches_with("A", options_case_insensitive));
        assert!(pat_except.matches_with("A", options_case_sensitive));
    }

    #[test]
    fn test_pattern_matches_require_literal_separator() {
        let options_require_literal = MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        };
        let options_not_require_literal = MatchOptions {
            case_sensitive: true,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        };

        assert!(Pattern::new("abc/def")
            .unwrap()
            .matches_with("abc/def", options_require_literal));
        assert!(!Pattern::new("abc?def")
            .unwrap()
            .matches_with("abc/def", options_require_literal));
        assert!(!Pattern::new("abc*def")
            .unwrap()
            .matches_with("abc/def", options_require_literal));
        assert!(!Pattern::new("abc[/]def")
            .unwrap()
            .matches_with("abc/def", options_require_literal));

        assert!(Pattern::new("abc/def")
            .unwrap()
            .matches_with("abc/def", options_not_require_literal));
        assert!(Pattern::new("abc?def")
            .unwrap()
            .matches_with("abc/def", options_not_require_literal));
        assert!(Pattern::new("abc*def")
            .unwrap()
            .matches_with("abc/def", options_not_require_literal));
        assert!(Pattern::new("abc[/]def")
            .unwrap()
            .matches_with("abc/def", options_not_require_literal));
    }

    #[test]
    fn test_pattern_matches_require_literal_leading_dot() {
        let options_require_literal_leading_dot = MatchOptions {
            case_sensitive: true,
            require_literal_separator: false,
            require_literal_leading_dot: true,
        };
        let options_not_require_literal_leading_dot = MatchOptions {
            case_sensitive: true,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        };

        let f = |options| {
            Pattern::new("*.txt")
                .unwrap()
                .matches_with(".hello.txt", options)
        };
        assert!(f(options_not_require_literal_leading_dot));
        assert!(!f(options_require_literal_leading_dot));

        let f = |options| {
            Pattern::new(".*.*")
                .unwrap()
                .matches_with(".hello.txt", options)
        };
        assert!(f(options_not_require_literal_leading_dot));
        assert!(f(options_require_literal_leading_dot));

        let f = |options| {
            Pattern::new("aaa/bbb/*")
                .unwrap()
                .matches_with("aaa/bbb/.ccc", options)
        };
        assert!(f(options_not_require_literal_leading_dot));
        assert!(!f(options_require_literal_leading_dot));

        let f = |options| {
            Pattern::new("aaa/bbb/*")
                .unwrap()
                .matches_with("aaa/bbb/c.c.c.", options)
        };
        assert!(f(options_not_require_literal_leading_dot));
        assert!(f(options_require_literal_leading_dot));

        let f = |options| {
            Pattern::new("aaa/bbb/.*")
                .unwrap()
                .matches_with("aaa/bbb/.ccc", options)
        };
        assert!(f(options_not_require_literal_leading_dot));
        assert!(f(options_require_literal_leading_dot));

        let f = |options| {
            Pattern::new("aaa/?bbb")
                .unwrap()
                .matches_with("aaa/.bbb", options)
        };
        assert!(f(options_not_require_literal_leading_dot));
        assert!(!f(options_require_literal_leading_dot));

        let f = |options| {
            Pattern::new("aaa/[.]bbb")
                .unwrap()
                .matches_with("aaa/.bbb", options)
        };
        assert!(f(options_not_require_literal_leading_dot));
        assert!(!f(options_require_literal_leading_dot));

        let f = |options| Pattern::new("**/*").unwrap().matches_with(".bbb", options);
        assert!(f(options_not_require_literal_leading_dot));
        assert!(!f(options_require_literal_leading_dot));
    }

    #[test]
    fn test_matches_path() {
        // on windows, (Path::new("a/b").as_str().unwrap() == "a\\b"), so this
        // tests that / and \ are considered equivalent on windows
        assert!(Pattern::new("a/b").unwrap().matches_path(Path::new("a/b")));
    }

    #[test]
    fn test_path_join() {
        let pattern = Path::new("one").join(Path::new("**/*.rs"));
        assert!(Pattern::new(pattern.to_str().unwrap()).is_ok());
    }

    // --- Pattern structure index tests ---

    #[test]
    fn test_pattern_index_classification() {
        // Pattern: a\*/[x*]/b?
        // Expected segments (the char class [x*] sits between two literal
        // runs, so "/" and "b" do NOT merge across it):
        //   Literal("a")  [0..1]
        //   Escape("*")   [1..3]
        //   Literal("/")  [3..4]
        //   CharClass     [4..8]  decoded "[x*]"
        //   Literal("/b") [8..10]
        //   Wildcard("?") [10..11]
        let pattern = Pattern::new("a\\*/[x*]/b?").unwrap();
        let index = pattern.index();

        assert_eq!(index.baseline(), "a\\*/[x*]/b?");
        assert_eq!(index.len(), 6);

        let segs = index.segments();
        assert_eq!(segs[0].kind(), SegmentKind::Literal);
        assert_eq!(segs[0].start(), 0);
        assert_eq!(segs[0].end(), 1);
        assert_eq!(segs[0].decoded(), "a");

        assert_eq!(segs[1].kind(), SegmentKind::Escape);
        assert_eq!(segs[1].start(), 1);
        assert_eq!(segs[1].end(), 3);
        assert_eq!(segs[1].decoded(), "*");

        assert_eq!(segs[2].kind(), SegmentKind::Literal);
        assert_eq!(segs[2].start(), 3);
        assert_eq!(segs[2].end(), 4);
        assert_eq!(segs[2].decoded(), "/");

        assert_eq!(segs[3].kind(), SegmentKind::CharClass);
        assert_eq!(segs[3].start(), 4);
        assert_eq!(segs[3].end(), 8);
        assert_eq!(segs[3].decoded(), "[x*]");

        assert_eq!(segs[4].kind(), SegmentKind::Literal);
        assert_eq!(segs[4].start(), 8);
        assert_eq!(segs[4].end(), 10);
        assert_eq!(segs[4].decoded(), "/b");

        assert_eq!(segs[5].kind(), SegmentKind::Wildcard);
        assert_eq!(segs[5].start(), 10);
        assert_eq!(segs[5].end(), 11);
        assert_eq!(segs[5].decoded(), "?");
    }

    #[test]
    fn test_pattern_index_edit_literal() {
        let pattern = Pattern::new("a\\*/[x*]/b?").unwrap();
        let index = pattern.index();

        // Edit the first literal segment ("a") to "a2".
        let edited = index.edit_literal(0, "a2").unwrap();
        assert_eq!(edited.as_str(), "a2\\*/[x*]/b?");

        // The escaped asterisk is still a literal after the edit.
        assert!(edited.matches("a2*/x/b?"));
        assert!(!edited.matches("a2X/x/b?"));

        // Re-index the edited pattern and verify non-target segments kept
        // their structure (kind and decoded value).
        let edited_index = edited.index();
        let edited_segs = edited_index.segments();

        // Segment 0 changed from "a" to "a2".
        assert_eq!(edited_segs[0].kind(), SegmentKind::Literal);
        assert_eq!(edited_segs[0].decoded(), "a2");

        // Segment 1 (escape) is unchanged in kind and decoded value.
        assert_eq!(edited_segs[1].kind(), SegmentKind::Escape);
        assert_eq!(edited_segs[1].decoded(), "*");

        // Segment 3 (char class) is unchanged.
        assert_eq!(edited_segs[3].kind(), SegmentKind::CharClass);
        assert_eq!(edited_segs[3].decoded(), "[x*]");

        // Segment 5 (wildcard "?") is unchanged.
        assert_eq!(edited_segs[5].kind(), SegmentKind::Wildcard);
        assert_eq!(edited_segs[5].decoded(), "?");
    }

    #[test]
    fn test_pattern_index_edit_literal_escapes_metachars() {
        let pattern = Pattern::new("a\\*/[x*]/b?").unwrap();
        let index = pattern.index();

        // Edit "a" to a value containing metacharacters; they must be escaped
        // so the segment remains a literal.
        let edited = index.edit_literal(0, "a*").unwrap();
        assert_eq!(edited.as_str(), "a[*]\\*/[x*]/b?");
        assert!(edited.matches("a**/x/b?"));
        assert!(!edited.matches("aXX/x/b?"));
    }

    #[test]
    fn test_pattern_index_verify_rejects_changed_baseline() {
        let pattern = Pattern::new("a\\*/[x*]/b?").unwrap();
        let index = pattern.index();
        assert!(index.verify(&pattern));

        // Change the baseline; the old index must be rejected.
        let changed = Pattern::new("x\\*/[x*]/b?").unwrap();
        assert!(!index.verify(&changed));

        // The index still verifies against the original pattern.
        assert!(index.verify(&pattern));
    }

    #[test]
    fn test_pattern_index_unicode_byte_ranges() {
        // "é" is 2 bytes in UTF-8.
        let pattern = Pattern::new("é\\*/b?").unwrap();
        let index = pattern.index();
        let segs = index.segments();

        assert_eq!(segs.len(), 4);

        assert_eq!(segs[0].kind(), SegmentKind::Literal);
        assert_eq!(segs[0].start(), 0);
        assert_eq!(segs[0].end(), 2);
        assert_eq!(segs[0].decoded(), "é");

        assert_eq!(segs[1].kind(), SegmentKind::Escape);
        assert_eq!(segs[1].start(), 2);
        assert_eq!(segs[1].end(), 4);
        assert_eq!(segs[1].decoded(), "*");

        assert_eq!(segs[2].kind(), SegmentKind::Literal);
        assert_eq!(segs[2].start(), 4);
        assert_eq!(segs[2].end(), 6);
        assert_eq!(segs[2].decoded(), "/b");

        assert_eq!(segs[3].kind(), SegmentKind::Wildcard);
        assert_eq!(segs[3].start(), 6);
        assert_eq!(segs[3].end(), 7);
        assert_eq!(segs[3].decoded(), "?");
    }

    #[test]
    fn test_pattern_index_charclass_star_not_wildcard() {
        // The `*` inside `[x*]` is part of the character class, not a wildcard.
        let pattern = Pattern::new("a[x*]b?").unwrap();
        let index = pattern.index();
        let segs = index.segments();

        assert_eq!(segs.len(), 4);
        assert_eq!(segs[0].kind(), SegmentKind::Literal);
        assert_eq!(segs[0].decoded(), "a");

        assert_eq!(segs[1].kind(), SegmentKind::CharClass);
        assert_eq!(segs[1].decoded(), "[x*]");

        assert_eq!(segs[2].kind(), SegmentKind::Literal);
        assert_eq!(segs[2].decoded(), "b");

        assert_eq!(segs[3].kind(), SegmentKind::Wildcard);
        assert_eq!(segs[3].decoded(), "?");

        // The pattern matches "a*b?" (the class matches the literal `*`).
        assert!(pattern.matches("a*b?"));
        assert!(pattern.matches("axb?"));
        assert!(!pattern.matches("aXb?"));
    }

    #[test]
    fn test_pattern_index_negated_charclass() {
        let pattern = Pattern::new("a[!x]b").unwrap();
        let index = pattern.index();
        let segs = index.segments();

        assert_eq!(segs.len(), 3);
        assert_eq!(segs[1].kind(), SegmentKind::CharClass);
        assert_eq!(segs[1].decoded(), "[!x]");
    }

    #[test]
    fn test_pattern_index_escape_variants() {
        let pattern = Pattern::new("a\\*b\\?c\\[d\\]e\\\\f").unwrap();
        let index = pattern.index();
        let segs = index.segments();

        // a, \*, b, \?, c, \[, d, \], e, \\, f
        assert_eq!(segs.len(), 11);
        assert_eq!(segs[0].kind(), SegmentKind::Literal);
        assert_eq!(segs[0].decoded(), "a");
        assert_eq!(segs[1].kind(), SegmentKind::Escape);
        assert_eq!(segs[1].decoded(), "*");
        assert_eq!(segs[2].kind(), SegmentKind::Literal);
        assert_eq!(segs[2].decoded(), "b");
        assert_eq!(segs[3].kind(), SegmentKind::Escape);
        assert_eq!(segs[3].decoded(), "?");
        assert_eq!(segs[4].kind(), SegmentKind::Literal);
        assert_eq!(segs[4].decoded(), "c");
        assert_eq!(segs[5].kind(), SegmentKind::Escape);
        assert_eq!(segs[5].decoded(), "[");
        assert_eq!(segs[6].kind(), SegmentKind::Literal);
        assert_eq!(segs[6].decoded(), "d");
        assert_eq!(segs[7].kind(), SegmentKind::Escape);
        assert_eq!(segs[7].decoded(), "]");
        assert_eq!(segs[8].kind(), SegmentKind::Literal);
        assert_eq!(segs[8].decoded(), "e");
        assert_eq!(segs[9].kind(), SegmentKind::Escape);
        assert_eq!(segs[9].decoded(), "\\");
        assert_eq!(segs[10].kind(), SegmentKind::Literal);
        assert_eq!(segs[10].decoded(), "f");

        // The pattern matches "a*b?c[d]e\f" where each metachar is literal.
        assert!(pattern.matches("a*b?c[d]e\\f"));
    }

    #[test]
    fn test_pattern_index_wildcards() {
        // `**` must form a single path component, so use `a/**/c?d`.
        let pattern = Pattern::new("a/**/c?d").unwrap();
        let index = pattern.index();
        let segs = index.segments();

        // a/, **, c, ?, d
        assert_eq!(segs.len(), 5);
        assert_eq!(segs[0].kind(), SegmentKind::Literal);
        assert_eq!(segs[0].decoded(), "a/");
        assert_eq!(segs[1].kind(), SegmentKind::Wildcard);
        assert_eq!(segs[1].decoded(), "**");
        assert_eq!(segs[2].kind(), SegmentKind::Literal);
        assert_eq!(segs[2].decoded(), "c");
        assert_eq!(segs[3].kind(), SegmentKind::Wildcard);
        assert_eq!(segs[3].decoded(), "?");
        assert_eq!(segs[4].kind(), SegmentKind::Literal);
        assert_eq!(segs[4].decoded(), "d");
    }

    #[test]
    fn test_pattern_index_literal_run_grouping() {
        // Consecutive literal characters are grouped into a single segment.
        let pattern = Pattern::new("abc").unwrap();
        let index = pattern.index();
        let segs = index.segments();

        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].kind(), SegmentKind::Literal);
        assert_eq!(segs[0].start(), 0);
        assert_eq!(segs[0].end(), 3);
        assert_eq!(segs[0].decoded(), "abc");
    }

    #[test]
    fn test_pattern_index_edit_non_literal_fails() {
        let pattern = Pattern::new("a\\*/[x*]/b?").unwrap();
        let index = pattern.index();

        // Segment 1 is an escape, not a literal; editing it must fail.
        assert!(index.edit_literal(1, "x").is_err());

        // Segment 3 is a char class, not a literal; editing it must fail.
        assert!(index.edit_literal(3, "x").is_err());

        // Segment 5 is a wildcard, not a literal; editing it must fail.
        assert!(index.edit_literal(5, "x").is_err());

        // Out-of-bounds segment index must fail.
        assert!(index.edit_literal(100, "x").is_err());
    }

    #[test]
    fn test_pattern_index_empty() {
        let pattern = Pattern::new("").unwrap();
        let index = pattern.index();
        assert_eq!(index.baseline(), "");
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
        assert!(index.verify(&pattern));
    }
}

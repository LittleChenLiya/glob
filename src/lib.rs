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
        let mut tokens = Vec::new();
        let mut is_recursive = false;
        let mut has_metachars = false;
        let mut i = 0;

        while i < chars.len() {
            match chars[i] {
                '?' => {
                    has_metachars = true;
                    tokens.push(AnyChar);
                    i += 1;
                }
                '*' => {
                    has_metachars = true;

                    let old = i;

                    while i < chars.len() && chars[i] == '*' {
                        i += 1;
                    }

                    let count = i - old;

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
                            let is_valid = if i == 2 || path::is_separator(chars[i - count - 1]) {
                                // it ends in a '/'
                                if i < chars.len() && path::is_separator(chars[i]) {
                                    i += 1;
                                    true
                                // or the pattern ends here
                                // this enables the existing globbing mechanism
                                } else if i == chars.len() {
                                    true
                                // `**` ends in non-separator
                                } else {
                                    return Err(PatternError {
                                        pos: i,
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
                            }
                        }
                        Ordering::Less => tokens.push(AnySequence),
                    }
                }
                '[' => {
                    has_metachars = true;

                    if i + 4 <= chars.len() && chars[i + 1] == '!' {
                        match chars[i + 3..].iter().position(|x| *x == ']') {
                            None => (),
                            Some(j) => {
                                let chars = &chars[i + 2..i + 3 + j];
                                let cs = parse_char_specifiers(chars);
                                tokens.push(AnyExcept(cs));
                                i += j + 4;
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
                c => {
                    tokens.push(Char(c));
                    i += 1;
                }
            }
        }

        Ok(Self {
            tokens,
            original: pattern.to_string(),
            is_recursive,
            has_metachars,
        })
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

/// The kind of a wildcard token within a `Pattern`.
///
/// This describes which sort of wildcard produced a particular
/// `WildcardMatch` when a match is explained via `Pattern::explain_with`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WildcardKind {
    /// A `*` token, matching any (possibly empty) sequence of characters.
    Star,
    /// A `**` token, matching the current directory and arbitrary
    /// subdirectories.
    RecursiveStar,
    /// A `?` token, matching any single character.
    Question,
    /// A `[...]` token, matching any single character inside the brackets.
    CharClass,
    /// A `[!...]` token, matching any single character *not* inside the
    /// brackets.
    NegatedCharClass,
}

/// The slice of an input string that a single wildcard token of a `Pattern`
/// consumed during a successful match, as reported by
/// `Pattern::explain_with`.
///
/// The byte offsets always fall on UTF-8 character boundaries of the input,
/// so they can be used to slice the input string directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WildcardMatch {
    /// The index of the wildcard within the pattern's token sequence
    /// (literal characters also count as tokens).
    pub token_index: usize,
    /// The kind of the wildcard token.
    pub kind: WildcardKind,
    /// The byte offset (inclusive) of the start of the consumed slice.
    pub start: usize,
    /// The byte offset (exclusive) of the end of the consumed slice.
    pub end: usize,
}

impl WildcardMatch {
    /// Return the slice of `input` that this wildcard consumed.
    ///
    /// `input` must be the same string that was passed to
    /// `Pattern::explain`/`Pattern::explain_with`.
    pub fn text<'a>(&self, input: &'a str) -> &'a str {
        &input[self.start..self.end]
    }
}

/// The result of explaining how a `Pattern` matches (or fails to match) an
/// input string, as produced by `Pattern::explain` and
/// `Pattern::explain_with`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchExplanation {
    /// Whether the pattern matched the input. This always agrees with
    /// `Pattern::matches_with` for the same input and options.
    pub matched: bool,
    /// The slices consumed by each wildcard token (`*`, `**`, `?` and
    /// `[...]`/`[!...]`), in left-to-right order. This is empty when
    /// `matched` is `false`.
    ///
    /// When several matches are possible, the reported explanation is the
    /// one where each wildcard, considered from left to right, consumes as
    /// few characters as possible.
    pub wildcards: Vec<WildcardMatch>,
    /// When `matched` is `false`, the earliest byte offset in the input at
    /// which a mismatch was detected while searching for a match. The offset
    /// always falls on a UTF-8 character boundary; it may be one past the
    /// end of the input when the input was exhausted before the pattern was.
    /// This is `None` when `matched` is `true`.
    pub failure_offset: Option<usize>,
}

impl fmt::Display for MatchExplanation {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.matched {
            if self.wildcards.is_empty() {
                write!(f, "matched (no wildcards)")
            } else {
                writeln!(f, "matched with {} wildcard(s):", self.wildcards.len())?;
                for (i, w) in self.wildcards.iter().enumerate() {
                    writeln!(
                        f,
                        "  #{} {:?} consumed bytes {}..{}",
                        i, w.kind, w.start, w.end
                    )?;
                }
                Ok(())
            }
        } else {
            match self.failure_offset {
                Some(offset) => write!(
                    f,
                    "no match (earliest failure determined at byte {})",
                    offset
                ),
                None => write!(f, "no match"),
            }
        }
    }
}

impl Pattern {
    /// Explain how this pattern matches the given `str` using the default
    /// match options (i.e. `MatchOptions::new()`).
    ///
    /// Unlike `Pattern::matches`, this reports *how* the pattern matched:
    /// which slice of the input each wildcard (`*`, `**`, `?`, `[...]`)
    /// consumed, or, when the pattern does not match, the earliest position
    /// at which the mismatch was determined. No filesystem is accessed.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use glob::Pattern;
    ///
    /// let pattern = Pattern::new("src/**/test_?.rs").unwrap();
    /// let input = "src/模块一/test_q.rs";
    /// let explanation = pattern.explain(input);
    /// assert!(explanation.matched);
    /// assert_eq!(explanation.wildcards[0].text(input), "模块一/");
    /// assert_eq!(explanation.wildcards[1].text(input), "q");
    /// ```
    pub fn explain(&self, str: &str) -> MatchExplanation {
        self.explain_with(str, MatchOptions::new())
    }

    /// Explain how this pattern matches the given `str` using the specified
    /// match options.
    ///
    /// The matching semantics are identical to `Pattern::matches_with`: the
    /// `matched` field of the result always agrees with it. See
    /// `Pattern::explain` and `MatchExplanation` for details on what is
    /// reported.
    ///
    /// The search is memoized, so explaining patterns with many consecutive
    /// wildcards takes polynomial (not exponential) time in the length of
    /// the input.
    pub fn explain_with(&self, str: &str, options: MatchOptions) -> MatchExplanation {
        let chars: Vec<char> = str.chars().collect();
        let mut offsets: Vec<usize> = str.char_indices().map(|(i, _)| i).collect();
        offsets.push(str.len());
        let state_count = (self.tokens.len() + 1) * (chars.len() + 1) * 2;
        let mut explainer = Explainer {
            tokens: &self.tokens,
            chars,
            offsets,
            options,
            memo: vec![None; state_count],
            failure_min: None,
        };
        if explainer.can(0, 0, true) {
            let wildcards = explainer.walk();
            MatchExplanation {
                matched: true,
                wildcards,
                failure_offset: None,
            }
        } else {
            MatchExplanation {
                matched: false,
                wildcards: Vec::new(),
                failure_offset: explainer.failure_min,
            }
        }
    }
}

/// A memoizing matcher used to implement `Pattern::explain_with`.
///
/// It mirrors the semantics of `Pattern::matches_from` exactly, but works
/// on pre-collected character and byte-offset vectors so that consumed
/// ranges can be reported, and it memoizes intermediate results so that
/// patterns with many wildcards cannot cause exponential backtracking.
struct Explainer<'a> {
    tokens: &'a [PatternToken],
    chars: Vec<char>,
    /// `offsets[i]` is the byte offset of `chars[i]` in the input;
    /// `offsets[chars.len()]` is the input length.
    offsets: Vec<usize>,
    options: MatchOptions,
    memo: Vec<Option<bool>>,
    /// The earliest byte offset at which a mismatch was detected.
    failure_min: Option<usize>,
}

impl<'a> Explainer<'a> {
    fn memo_index(&self, ti: usize, ci: usize, follows_separator: bool) -> usize {
        ((ti * (self.chars.len() + 1)) + ci) * 2 + follows_separator as usize
    }

    fn record_failure(&mut self, ci: usize) {
        let offset = self.offsets[ci];
        self.failure_min = Some(match self.failure_min {
            Some(min) => cmp::min(min, offset),
            None => offset,
        });
    }

    /// Compute the attempts a star token at input position `ci` may make,
    /// in increasing order of consumption.
    ///
    /// Returns the list of `(consumed_chars, follows_separator)` pairs to
    /// try, plus the input position at which consumption was forced to stop
    /// early (a `require_literal_leading_dot` or `require_literal_separator`
    /// violation), if any.
    fn star_attempts(
        &self,
        ci: usize,
        follows_separator: bool,
        recursive: bool,
    ) -> (Vec<(usize, bool)>, Option<usize>) {
        let n = self.chars.len();
        let mut attempts = Vec::new();
        let mut failure = None;
        let mut cur_follows_separator = follows_separator;
        let mut k = 0;
        loop {
            // `**` only attempts a sub-match right after a separator, except
            // that consuming the rest of the input always counts (this
            // mirrors `matches_from` falling out of its consumption loop).
            if k == 0 || !recursive || cur_follows_separator || ci + k == n {
                attempts.push((k, cur_follows_separator));
            }
            if ci + k == n {
                break;
            }
            let c = self.chars[ci + k];
            if cur_follows_separator && self.options.require_literal_leading_dot && c == '.' {
                failure = Some(ci + k);
                break;
            }
            cur_follows_separator = path::is_separator(c);
            k += 1;
            if !recursive && self.options.require_literal_separator && cur_follows_separator {
                failure = Some(ci + k - 1);
                break;
            }
        }
        (attempts, failure)
    }

    /// Return whether `tokens[ti..]` can match `chars[ci..]`, with
    /// `follows_separator` indicating whether `chars[ci]` directly follows
    /// a path separator. Memoized; equivalent to `Pattern::matches_from`.
    fn can(&mut self, ti0: usize, ci0: usize, fs0: bool) -> bool {
        let tokens = self.tokens;
        let n = self.chars.len();
        let mut ti = ti0;
        let mut ci = ci0;
        let mut follows_separator = fs0;
        let mut visited = Vec::new();
        let result = loop {
            let idx = self.memo_index(ti, ci, follows_separator);
            if let Some(cached) = self.memo[idx] {
                break cached;
            }
            visited.push(idx);
            if ti == tokens.len() {
                if ci == n {
                    break true;
                }
                self.record_failure(ci);
                break false;
            }
            match tokens[ti] {
                AnySequence | AnyRecursiveSequence => {
                    let recursive = tokens[ti] == AnyRecursiveSequence;
                    let (attempts, failure) = self.star_attempts(ci, follows_separator, recursive);
                    if let Some(failure) = failure {
                        self.record_failure(failure);
                    }
                    let mut matched = false;
                    for (k, next_follows_separator) in attempts {
                        if self.can(ti + 1, ci + k, next_follows_separator) {
                            matched = true;
                            break;
                        }
                    }
                    break matched;
                }
                _ => {
                    if ci == n {
                        self.record_failure(n);
                        break false;
                    }
                    let c = self.chars[ci];
                    let is_sep = path::is_separator(c);
                    if !token_matches(&tokens[ti], c, is_sep, follows_separator, self.options) {
                        self.record_failure(ci);
                        break false;
                    }
                    ti += 1;
                    ci += 1;
                    follows_separator = is_sep;
                }
            }
        };
        for idx in visited {
            self.memo[idx] = Some(result);
        }
        result
    }

    /// Reconstruct the left-to-right minimal-consumption explanation of a
    /// successful match. Must only be called after `can(0, 0, true)`
    /// returned `true`.
    fn walk(&mut self) -> Vec<WildcardMatch> {
        let tokens = self.tokens;
        let mut wildcards = Vec::new();
        let mut ti = 0;
        let mut ci = 0;
        let mut follows_separator = true;
        while ti < tokens.len() {
            match tokens[ti] {
                AnySequence | AnyRecursiveSequence => {
                    let recursive = tokens[ti] == AnyRecursiveSequence;
                    let (attempts, _) = self.star_attempts(ci, follows_separator, recursive);
                    let mut chosen = None;
                    for (k, next_follows_separator) in attempts {
                        if self.can(ti + 1, ci + k, next_follows_separator) {
                            chosen = Some((k, next_follows_separator));
                            break;
                        }
                    }
                    let (k, next_follows_separator) = chosen.expect("explain: walk lost a match");
                    wildcards.push(WildcardMatch {
                        token_index: ti,
                        kind: if recursive {
                            WildcardKind::RecursiveStar
                        } else {
                            WildcardKind::Star
                        },
                        start: self.offsets[ci],
                        end: self.offsets[ci + k],
                    });
                    ci += k;
                    follows_separator = next_follows_separator;
                    ti += 1;
                }
                Char(_) => {
                    follows_separator = path::is_separator(self.chars[ci]);
                    ci += 1;
                    ti += 1;
                }
                _ => {
                    let kind = match tokens[ti] {
                        AnyChar => WildcardKind::Question,
                        AnyWithin(..) => WildcardKind::CharClass,
                        AnyExcept(..) => WildcardKind::NegatedCharClass,
                        _ => unreachable!(),
                    };
                    wildcards.push(WildcardMatch {
                        token_index: ti,
                        kind,
                        start: self.offsets[ci],
                        end: self.offsets[ci + 1],
                    });
                    follows_separator = path::is_separator(self.chars[ci]);
                    ci += 1;
                    ti += 1;
                }
            }
        }
        debug_assert_eq!(ci, self.chars.len());
        wildcards
    }
}

/// A helper to determine whether a single non-star token matches a character.
///
/// This mirrors the corresponding logic in `Pattern::matches_from`.
fn token_matches(
    token: &PatternToken,
    c: char,
    is_sep: bool,
    follows_separator: bool,
    options: MatchOptions,
) -> bool {
    match *token {
        AnyChar | AnyWithin(..) | AnyExcept(..)
            if (options.require_literal_separator && is_sep)
                || (follows_separator && options.require_literal_leading_dot && c == '.') =>
        {
            false
        }
        AnyChar => true,
        AnyWithin(ref specifiers) => in_char_specifiers(specifiers, c, options),
        AnyExcept(ref specifiers) => !in_char_specifiers(specifiers, c, options),
        Char(c2) => chars_eq(c, c2, options.case_sensitive),
        AnySequence | AnyRecursiveSequence => unreachable!(),
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
    use super::{glob, MatchOptions, Pattern};
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

    /// Naively enumerate every way `pattern` can match `s`, returning the
    /// consumption (in chars) of each wildcard token for each match.
    ///
    /// This is an exponential-time oracle used to cross-check the
    /// left-to-right minimal-consumption explanations produced by
    /// `Pattern::explain_with` on small inputs.
    fn brute_force_consumptions(
        pattern: &Pattern,
        s: &str,
        options: MatchOptions,
    ) -> Vec<Vec<usize>> {
        use super::PatternToken::*;
        #[allow(clippy::too_many_arguments)]
        fn rec(
            tokens: &[super::PatternToken],
            ti: usize,
            chars: &[char],
            ci: usize,
            follows_separator: bool,
            options: MatchOptions,
            acc: &mut Vec<usize>,
            out: &mut Vec<Vec<usize>>,
        ) {
            if ti == tokens.len() {
                if ci == chars.len() {
                    out.push(acc.clone());
                }
                return;
            }
            match tokens[ti] {
                AnySequence | AnyRecursiveSequence => {
                    let recursive = tokens[ti] == AnyRecursiveSequence;
                    let n = chars.len();
                    let mut cur_follows_separator = follows_separator;
                    let mut k = 0;
                    loop {
                        if k == 0 || !recursive || cur_follows_separator || ci + k == n {
                            acc.push(k);
                            rec(
                                tokens,
                                ti + 1,
                                chars,
                                ci + k,
                                cur_follows_separator,
                                options,
                                acc,
                                out,
                            );
                            acc.pop();
                        }
                        if ci + k == n {
                            break;
                        }
                        let c = chars[ci + k];
                        if cur_follows_separator && options.require_literal_leading_dot && c == '.'
                        {
                            break;
                        }
                        cur_follows_separator = std::path::is_separator(c);
                        k += 1;
                        if !recursive && options.require_literal_separator && cur_follows_separator
                        {
                            break;
                        }
                    }
                }
                Char(c2) => {
                    if ci < chars.len() {
                        let c = chars[ci];
                        if super::chars_eq(c, c2, options.case_sensitive) {
                            rec(
                                tokens,
                                ti + 1,
                                chars,
                                ci + 1,
                                std::path::is_separator(c),
                                options,
                                acc,
                                out,
                            );
                        }
                    }
                }
                _ => {
                    if ci < chars.len() {
                        let c = chars[ci];
                        let is_sep = std::path::is_separator(c);
                        if super::token_matches(&tokens[ti], c, is_sep, follows_separator, options)
                        {
                            acc.push(1);
                            rec(tokens, ti + 1, chars, ci + 1, is_sep, options, acc, out);
                            acc.pop();
                        }
                    }
                }
            }
        }
        let chars: Vec<char> = s.chars().collect();
        let mut out = Vec::new();
        let mut acc = Vec::new();
        rec(
            &pattern.tokens,
            0,
            &chars,
            0,
            true,
            options,
            &mut acc,
            &mut out,
        );
        out
    }

    #[test]
    fn test_explain_agrees_with_brute_force() {
        let patterns = [
            "", "a", "ab", "*", "**", "?", "[ab]", "[!ab]", "a*", "*a", "a*a", "*a*", "a*a*a",
            "*.*", "**/a", "a/**", "a/**/b", "**/*", "*/?", "a*b*c", "[ab]*c", "a?c", ".*",
            "**/.*",
        ];
        let inputs = [
            "", "a", "b", "aa", "ab", "ba", "aaa", "abab", "a.b", "a/b", "a/b/c", ".a", "a.",
            "abc", "xaybz",
        ];
        for case_sensitive in [true, false] {
            for require_literal_separator in [true, false] {
                for require_literal_leading_dot in [true, false] {
                    let options = MatchOptions {
                        case_sensitive,
                        require_literal_separator,
                        require_literal_leading_dot,
                    };
                    for &pat in patterns.iter() {
                        let pattern = Pattern::new(pat).unwrap();
                        for &input in inputs.iter() {
                            let explanation = pattern.explain_with(input, options);
                            assert_eq!(
                                explanation.matched,
                                pattern.matches_with(input, options),
                                "match disagreement for {:?} vs {:?} with {:?}",
                                pat,
                                input,
                                options
                            );
                            let mut all = brute_force_consumptions(&pattern, input, options);
                            all.sort();
                            all.dedup();
                            if explanation.matched {
                                let got: Vec<usize> = explanation
                                    .wildcards
                                    .iter()
                                    .map(|w| input[w.start..w.end].chars().count())
                                    .collect();
                                assert_eq!(
                                    Some(&got),
                                    all.first(),
                                    "explanation is not left-to-right minimal for {:?} vs {:?} with {:?}",
                                    pat,
                                    input,
                                    options
                                );
                            } else {
                                assert!(
                                    all.is_empty(),
                                    "brute force found a match for {:?} vs {:?} with {:?}",
                                    pat,
                                    input,
                                    options
                                );
                                assert!(explanation.failure_offset.is_some());
                            }
                        }
                    }
                }
            }
        }
    }
}

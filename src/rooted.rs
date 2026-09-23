// Copyright 2014 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Root-bounded recursive glob matching with limited symlink following.
//!
//! Unlike [`glob`](crate::glob), which interprets the pattern relative to the
//! current working directory, [`glob_rooted`] treats a caller-supplied
//! directory as the traversal boundary and reports every match as the
//! relative path the caller sees underneath that root.
//!
//! The in-root scope check is a *static* check against the file system tree
//! as it exists while scanning. It is performed component-by-component on
//! canonicalized paths, but it cannot defend against the file system being
//! replaced (e.g. a symlink being swapped) after the check has been made.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{MatchOptions, Pattern, PatternError};

/// How [`glob_rooted`] treats symbolic links encountered during traversal.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum FollowLinks {
    /// Never follow symbolic links. A link is reported as a plain entry when
    /// it matches the pattern, but the walk never descends through it and
    /// never resolves its target.
    Never,
    /// Follow a symbolic link only when its canonicalized target stays inside
    /// the canonicalized root directory. The boundary is compared per path
    /// component, so a sibling directory whose name merely shares a string
    /// prefix with the root (e.g. `root-external` next to `root`) is
    /// treated as outside. Links escaping the root are not followed; broken
    /// links are reported as [`RootedErrorKind::BrokenLink`] errors.
    WithinRoot,
}

/// Configuration options for [`glob_rooted`].
#[derive(Debug, Copy, Clone)]
pub struct RootedOptions {
    /// The symbolic link policy to apply while walking. Defaults to
    /// [`FollowLinks::Never`].
    pub follow_links: FollowLinks,
    /// Maximum depth of returned entries, counted in components of the
    /// logical (root-relative) path. Direct children of the root have depth
    /// 1. Entries deeper than the limit are neither returned nor descended
    /// into. `None` means no limit.
    pub max_depth: Option<usize>,
    /// Options forwarded to the pattern matcher, with the same semantics as
    /// [`glob_with`](crate::glob_with): path separators, hidden (dot) files
    /// and case sensitivity behave exactly as they do for the existing
    /// matcher. As with `glob_with`, `require_literal_separator` is always
    /// forced to `true`.
    pub match_options: MatchOptions,
}

impl Default for RootedOptions {
    fn default() -> Self {
        RootedOptions {
            follow_links: FollowLinks::Never,
            max_depth: None,
            match_options: MatchOptions::new(),
        }
    }
}

/// The category of a [`RootedError`], so callers can tell a missing root
/// from a broken link, a permission failure or a truncated traversal loop.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum RootedErrorKind {
    /// The root directory itself does not exist (or its canonicalization
    /// failed with `NotFound`).
    RootNotFound,
    /// A symbolic link that the policy asked to follow could not be resolved
    /// because its target does not exist.
    BrokenLink,
    /// Reading a directory or resolving an entry failed with a permission
    /// error.
    PermissionDenied,
    /// Descending into this path would revisit a directory that is already on
    /// the current ancestor chain, so the descent was truncated. The error
    /// path names exactly the entry whose descent was cut off; sibling
    /// entries are still traversed.
    Loop,
    /// Any other I/O error.
    Other,
}

/// A glob iteration error produced by [`glob_rooted`].
///
/// The path is reported the way the caller sees it: relative to the scan
/// root for entries inside the tree, or the root path itself for errors
/// that concern the root.
#[derive(Debug)]
pub struct RootedError {
    path: PathBuf,
    kind: RootedErrorKind,
    error: Option<io::Error>,
}

impl RootedError {
    /// The path that the error corresponds to, as seen by the caller.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Which category this error falls into.
    pub fn kind(&self) -> RootedErrorKind {
        self.kind
    }

    /// The underlying I/O error, if there is one. Loop truncation carries a
    /// synthesized error describing the loop.
    pub fn error(&self) -> Option<&io::Error> {
        self.error.as_ref()
    }
}

impl fmt::Display for RootedError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let description = match self.kind {
            RootedErrorKind::RootNotFound => "root directory not found",
            RootedErrorKind::BrokenLink => "broken symbolic link",
            RootedErrorKind::PermissionDenied => "permission denied",
            RootedErrorKind::Loop => "directory loop truncated",
            RootedErrorKind::Other => "i/o error",
        };
        match &self.error {
            Some(error) => write!(f, "{} at `{}`: {}", description, self.path.display(), error),
            None => write!(f, "{} at `{}`", description, self.path.display()),
        }
    }
}

impl Error for RootedError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.error
            .as_ref()
            .map(|error| error as &(dyn Error + 'static))
    }
}

/// An alias for a rooted glob iteration result.
///
/// This represents either a matched path (relative to the scan root) or a
/// traversal error. Errors never end the iteration early: entries that come
/// after an error in traversal order are still produced.
pub type RootedResult = Result<PathBuf, RootedError>;

fn classify_io_error(path: PathBuf, error: io::Error, at_root: bool) -> RootedError {
    let kind = match error.kind() {
        io::ErrorKind::NotFound if at_root => RootedErrorKind::RootNotFound,
        io::ErrorKind::NotFound => RootedErrorKind::BrokenLink,
        io::ErrorKind::PermissionDenied => RootedErrorKind::PermissionDenied,
        _ => RootedErrorKind::Other,
    };
    RootedError {
        path,
        kind,
        error: Some(error),
    }
}

/// An iterator that yields the `RootedResult`s of a [`glob_rooted`] scan in
/// a stable, deterministic traversal order (depth-first, with each
/// directory's children visited in alphabetical order).
#[derive(Debug)]
pub struct RootedPaths {
    items: std::vec::IntoIter<RootedResult>,
}

impl Iterator for RootedPaths {
    type Item = RootedResult;

    fn next(&mut self) -> Option<RootedResult> {
        self.items.next()
    }
}

/// Return an iterator that produces all paths under `root` matching
/// `pattern`, expressed relative to `root` exactly as the caller sees
/// them.
///
/// The pattern is matched against root-relative paths with the same
/// semantics as [`glob_with`](crate::glob_with): `*`, `?`, `[...]` and
/// the recursive `**` token are supported, `require_literal_separator` is
/// forced on, and hidden-file / case-sensitivity behavior comes from
/// [`RootedOptions::match_options`]. A trailing separator in the pattern
/// restricts results to directories, mirroring `glob`.
///
/// Symbolic links are handled according to
/// [`RootedOptions::follow_links`]. Directory loops are detected against the
/// current ancestor chain only, so the same real directory reached through
/// two different aliases is reported once per alias, while a link pointing
/// back at one of its own ancestors produces a single
/// [`RootedErrorKind::Loop`] error naming the offending path and does not
/// abort the rest of the scan.
///
/// Note that the in-root check is a static scope check against the file
/// system tree observed during the scan; it is not a defense against entries
/// being replaced after they have been checked.
///
/// # Examples
///
/// ```no_run
/// use glob::{glob_rooted, FollowLinks, RootedOptions};
/// use std::path::Path;
///
/// let options = RootedOptions {
///     follow_links: FollowLinks::WithinRoot,
///     max_depth: None,
///     ..RootedOptions::default()
/// };
/// for entry in glob_rooted(Path::new("assets"), "**/*.png", options).unwrap() {
///     match entry {
///         Ok(path) => println!("{}", path.display()),
///         Err(e) => eprintln!("{}", e),
///     }
/// }
/// ```
pub fn glob_rooted(
    root: &Path,
    pattern: &str,
    options: RootedOptions,
) -> Result<RootedPaths, PatternError> {
    // Validate the pattern up front, mirroring glob_with.
    let _ = Pattern::new(pattern)?;

    let trimmed = pattern.trim_end_matches(std::path::is_separator);
    let require_dir = trimmed.len() != pattern.len();
    let matcher = Pattern::new(trimmed)?;

    let mut match_options = options.match_options;
    match_options.require_literal_separator = true;

    // Hidden entries are filtered during traversal when the match options
    // demand a literal leading dot, unless the pattern itself names a
    // dot-component literally (in which case hidden entries stay reachable,
    // matching the behavior of fill_todo's literal-component fast path).
    let allow_hidden = !match_options.require_literal_leading_dot
        || trimmed
            .split(std::path::is_separator)
            .any(|component| component.starts_with('.'));

    let mut walker = Walker {
        matcher,
        require_dir,
        match_options,
        follow_links: options.follow_links,
        max_depth: options.max_depth,
        allow_hidden,
        items: Vec::new(),
    };

    let items = match fs::canonicalize(root) {
        Ok(canonical_root) => {
            let mut ancestors = vec![canonical_root];
            walker.walk(Path::new(""), root, &mut ancestors);
            walker.items
        }
        Err(error) => vec![Err(classify_io_error(root.to_path_buf(), error, true))],
    };

    Ok(RootedPaths {
        items: items.into_iter(),
    })
}

struct Walker {
    matcher: Pattern,
    require_dir: bool,
    match_options: MatchOptions,
    follow_links: FollowLinks,
    max_depth: Option<usize>,
    allow_hidden: bool,
    items: Vec<RootedResult>,
}

impl Walker {
    fn walk(&mut self, logical: &Path, fs_path: &Path, ancestors: &mut Vec<PathBuf>) {
        let read_dir = match fs::read_dir(fs_path) {
            Ok(read_dir) => read_dir,
            Err(error) => {
                let at_root = logical.as_os_str().is_empty();
                let path = if at_root {
                    fs_path.to_path_buf()
                } else {
                    logical.to_path_buf()
                };
                self.items.push(Err(classify_io_error(path, error, at_root)));
                return;
            }
        };

        let mut children = Vec::new();
        for entry in read_dir {
            match entry {
                Ok(entry) => children.push(entry),
                Err(error) => self.items.push(Err(classify_io_error(
                    logical.to_path_buf(),
                    error,
                    false,
                ))),
            }
        }
        children.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

        for child in children {
            let name = child.file_name();
            if !self.allow_hidden {
                if let Some(text) = name.to_str() {
                    if text.starts_with('.') {
                        continue;
                    }
                }
            }

            let child_logical = logical.join(&name);
            let depth = child_logical.components().count();
            if self.max_depth.map_or(false, |max| depth > max) {
                continue;
            }

            let child_fs = child.path();
            let file_type = match child.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    self.items
                        .push(Err(classify_io_error(child_logical, error, false)));
                    continue;
                }
            };

            // Whether this entry may be descended into, and the canonical
            // path to use for loop detection when it is.
            let mut descend = file_type.is_dir();
            let mut canonical = None;

            if file_type.is_symlink() {
                descend = false;
                if self.follow_links == FollowLinks::WithinRoot {
                    match fs::canonicalize(&child_fs) {
                        Ok(target) => {
                            // Component-wise boundary check: Path::starts_with
                            // compares whole components, so a sibling whose
                            // name merely shares a string prefix with the
                            // root is correctly treated as outside.
                            if target.starts_with(&ancestors[0]) {
                                canonical = Some(target);
                                descend =
                                    fs::metadata(&child_fs).map_or(false, |meta| meta.is_dir());
                            }
                        }
                        Err(error) => {
                            self.items
                                .push(Err(classify_io_error(child_logical, error, false)));
                            continue;
                        }
                    }
                }
            }

            // A directory for matching purposes follows links (like the
            // existing PathWrapper), independent of the follow policy.
            let is_dir = file_type.is_dir()
                || (file_type.is_symlink()
                    && fs::metadata(&child_fs).map_or(false, |meta| meta.is_dir()));

            if self
                .matcher
                .matches_path_with(&child_logical, self.match_options)
                && (!self.require_dir || is_dir)
            {
                self.items.push(Ok(child_logical.clone()));
            }

            if !descend || self.max_depth.map_or(false, |max| depth >= max) {
                continue;
            }

            let canonical = match canonical {
                Some(canonical) => canonical,
                None => match fs::canonicalize(&child_fs) {
                    Ok(canonical) => canonical,
                    Err(error) => {
                        self.items
                            .push(Err(classify_io_error(child_logical, error, false)));
                        continue;
                    }
                },
            };

            if ancestors.contains(&canonical) {
                self.items.push(Err(RootedError {
                    path: child_logical.clone(),
                    kind: RootedErrorKind::Loop,
                    error: Some(io::Error::new(
                        io::ErrorKind::Other,
                        format!(
                            "descending into `{}` would revisit an ancestor directory",
                            child_logical.display()
                        ),
                    )),
                }));
                continue;
            }

            ancestors.push(canonical);
            self.walk(&child_logical, &child_fs, ancestors);
            ancestors.pop();
        }
    }
}

// Copyright 2026 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Explicit, batched glob scanning with resumable, persistent cursors.
//!
//! [`BatchScanner`] walks a directory tree with the same per-component matching
//! rules as [`crate::glob_with`], but instead of an `Iterator` it returns one
//! [`Batch`] at a time through [`BatchScanner::next_batch`]. Every batch is
//! capped at a fixed number of *matched* paths (`batch_size`); entries that do
//! not match the pattern are still consumed from the traversal and advance the
//! resume position, so the scan always makes progress.
//!
//! Matched paths within and across batches follow a fixed order: pre-order
//! depth-first traversal, with each directory's entries ordered by comparable
//! relative path (byte order on Unix, UTF-16 code-unit order on Windows). A
//! path returned once is never delivered again when resuming from a cursor
//! produced by the same scan.
//!
//! # Persistence and resumption
//!
//! [`BatchScanner::cursor`] and [`Batch::cursor`] render an opaque, versioned
//! [`String`] capturing the exact traversal frontier. It can be stored in a
//! file or database and later passed to [`BatchScanner::resume`], possibly in a
//! different process.
//!
//! The cursor is bound to:
//!
//! * the pattern,
//! * the [`crate::MatchOptions`], and
//! * the identity of the scan root.
//!
//! Root identity is the root as the OS resolves it (`canonicalize`). Resuming
//! with a different pattern, different options, or a different root is rejected
//! with [`CursorError`]; cursors cannot be reused after changing configuration.
//!
//! # Directory changes across resumes
//!
//! The walk is a live walk, not a snapshot. If directories still on the
//! frontier gain or lose entries while the scan is paused, this is reported,
//! never hidden: the first resumed batch carries [`DirectoryChange`] reports
//! and [`Batch::is_snapshot_consistent`] returns `false`. Files inserted ahead
//! of the cursor into directories that were already fully consumed are not seen
//! (they sit behind the resume position); this is the standard resume boundary
//! and is not reported, because that directory is never observed again. An
//! entry that was already delivered is never re-delivered.
//!
//! Because a live walk cannot prove that nothing changed, callers must not
//! claim the result set is identical to a fresh scan when consistency is
//! reported as false.
//!
//! # Example: five files, page size two, resume later
//!
//! ```no_run
//! use glob::batch::BatchScanner;
//! use glob::MatchOptions;
//!
//! let root = "/srv/incoming";
//!
//! // First run: read one batch, then persist the cursor.
//! let mut scanner = BatchScanner::new("*.txt", MatchOptions::new(), root, 2).unwrap();
//! let batch = scanner.next_batch();
//! assert_eq!(batch.matches.len(), 2);
//! std::fs::write("scan.cursor", batch.cursor()).unwrap();
//!
//! // ... later, possibly in another process ...
//!
//! // Resume: pattern, options and root must be identical, otherwise resume
//! // returns a CursorError.
//! let saved = std::fs::read_to_string("scan.cursor").unwrap();
//! let mut scanner =
//!     BatchScanner::resume(&saved, "*.txt", MatchOptions::new(), root).unwrap();
//! let rest = scanner.next_batch();
//! assert_eq!(rest.matches.len(), 3); // the remaining three files
//! assert!(rest.is_finished());
//! ```
//!
//! # Consistency boundaries
//!
//! * Within one uninterrupted scan, traversal order is deterministic and every
//!   path is delivered at most once.
//! * Resuming never re-delivers a path the previous run already returned.
//! * A resume can only observe (and report) changes in directories still on the
//!   frontier. Directories fully consumed before the pause are never revisited,
//!   so files inserted into them afterwards are missed silently rather than
//!   reported.
//! * When [`Batch::is_snapshot_consistent`] is false, inspect
//!   [`Batch::changes`] and treat the combined result as best-effort, never as
//!   an exact, stable snapshot.

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::{GlobError, MatchOptions, Pattern};

const FORMAT_ID: &str = "GBC1";

/// An explicit batched glob scanner over a directory tree.
///
/// See the [module documentation](self) for batching, cursor, resumption and
/// change-reporting semantics.
pub struct BatchScanner {
    pattern: String,
    components: Vec<Pattern>,
    options: MatchOptions,
    require_dir: bool,
    root: PathBuf,
    root_identity: PathBuf,
    batch_size: usize,
    stack: Vec<Frame>,
    finished: bool,
    /// Set for the rest of this scan once a resumed directory is observed to
    /// have gained or lost entries.
    tainted: bool,
}

struct Frame {
    /// Root-relative path of this directory; the empty path means the root.
    rel: PathBuf,
    /// Pattern component index this directory expands against.
    pat_idx: usize,
    /// Whether pat_idx points at a recursive component.
    recursive: bool,
    /// Remaining work queue in descending order (popped from the end), so
    /// iteration is ascending. `loaded` distinguishes never-read from
    /// read-and-empty.
    entries: Vec<EntrySnapshot>,
    loaded: bool,
    /// Full directory listing observed when this frame was (re)read, in
    /// ascending (display) order. Together with `consumed` it is the resume
    /// watermark: the first `consumed` entries were already handed out and
    /// must never be delivered again.
    observed: Vec<EntrySnapshot>,
    /// Number of `observed` entries already consumed.
    consumed: usize,
    /// Present only for frames rebuilt from a cursor until their first
    /// (re)read. Holds the cursor listing and consumed count so the fresh
    /// listing can be diffed and the queue resumed past the watermark.
    resume: Option<ResumeSnapshot>,
    /// When true, emit this directory itself before its entries (trailing **).
    emit_self: bool,
    /// Fingerprint of the listing last observed.
    fingerprint: u64,
    fingerprint_valid: bool,
}

/// Snapshot carried by a cursor for a partially or fully read directory.
struct ResumeSnapshot {
    /// Entries recorded in the cursor, ascending.
    observed: Vec<EntrySnapshot>,
    /// Entries already consumed at pause time.
    consumed: usize,
}

#[derive(Clone)]
struct EntrySnapshot {
    name: OsString,
    is_dir: bool,
}

/// One page of results from [`BatchScanner::next_batch`].
#[derive(Debug)]
pub struct Batch {
    /// Matched paths in fixed (sorted relative-path) order; at most
    /// `batch_size` entries.
    pub matches: Vec<PathBuf>,
    /// I/O errors encountered while filling this batch (unreadable
    /// directories). Errors do not consume match capacity.
    pub errors: Vec<GlobError>,
    /// Directories that gained or lost entries between the paused scan and
    /// this batch. Only populated on the first batch after a resume that
    /// observed a change.
    pub changes: Vec<DirectoryChange>,
    cursor: String,
    finished: bool,
    snapshot_consistent: bool,
}

impl Batch {
    /// The persistent, opaque resume cursor. Store it verbatim and pass it to
    /// [`BatchScanner::resume`] later.
    pub fn cursor(&self) -> &str {
        &self.cursor
    }

    /// True when the traversal frontier is exhausted. Further `next_batch`
    /// calls keep returning empty, finished batches.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// True when no observed directory changed across a resume boundary. A
    /// false value means the combined result must not be presented as a
    /// complete, unchanged snapshot; inspect [`Batch::changes`].
    pub fn is_snapshot_consistent(&self) -> bool {
        self.snapshot_consistent
    }
}

/// A reported difference between a directory listing captured in a cursor and
/// the listing observed on resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryChange {
    /// Root-relative path of the directory whose listing changed.
    pub directory: PathBuf,
    /// Entry names present now but absent from the cursor listing (percent
    /// encoded; see [`encode_component`]).
    pub added: Vec<String>,
    /// Entry names captured by the cursor that no longer exist (percent
    /// encoded).
    pub removed: Vec<String>,
    /// Fingerprint of the previous (cursor) listing.
    pub previous_fingerprint: u64,
    /// Fingerprint of the listing observed on resume.
    pub current_fingerprint: u64,
}

/// Errors from [`BatchScanner::new`] and [`BatchScanner::resume`].
#[derive(Debug)]
pub enum ScanError {
    /// The pattern is not a valid glob pattern (or was empty/unsupported).
    Pattern(crate::PatternError),
    /// The root could not be read or was not a directory.
    Io(GlobError),
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::Pattern(e) => write!(f, "invalid batch scan pattern: {e}"),
            ScanError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl Error for ScanError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ScanError::Pattern(e) => Some(e),
            ScanError::Io(e) => Some(e),
        }
    }
}

/// A stored cursor could not resume a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorError {
    /// The cursor text is truncated, garbled or not produced by this crate.
    Corrupt(String),
    /// The cursor was produced for another pattern.
    PatternMismatch {
        /// Pattern embedded in the cursor.
        stored: String,
        /// Pattern passed to resume.
        given: String,
    },
    /// The cursor was produced with different [`crate::MatchOptions`].
    OptionsMismatch,
    /// The cursor's bound root identity differs from the given root.
    RootMismatch {
        /// Root identity embedded in the cursor.
        stored: String,
        /// Root identity passed to resume.
        given: String,
    },
}

impl fmt::Display for CursorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CursorError::Corrupt(msg) => write!(f, "corrupt batch scan cursor: {msg}"),
            CursorError::PatternMismatch { stored, given } => write!(
                f,
                "cursor is bound to pattern {stored:?}, cannot resume with {given:?}"
            ),
            CursorError::OptionsMismatch => f.write_str(
                "cursor is bound to different MatchOptions; changing scan options is rejected",
            ),
            CursorError::RootMismatch { stored, given } => write!(
                f,
                "cursor root identity {stored:?} does not match resume root {given:?}"
            ),
        }
    }
}

impl Error for CursorError {}

impl BatchScanner {
    /// Create a scanner matching `pattern` (interpreted relative to `root`)
    /// with `options`, returning at most `batch_size` matched paths per batch.
    ///
    /// `pattern` uses the same syntax and per-component matching rules as
    /// [`crate::glob_with`], but is always relative to `root` (no leading
    /// separator and no `..` components). Returned paths are joined under
    /// `root`.
    ///
    /// # Errors
    ///
    /// * [`ScanError::Pattern`] if the pattern is invalid, empty, absolute or
    ///   contains `..`.
    /// * [`ScanError::Io`] if `batch_size` is zero or `root` is not an
    ///   existing directory.
    pub fn new(
        pattern: &str,
        options: MatchOptions,
        root: impl AsRef<Path>,
        batch_size: usize,
    ) -> Result<BatchScanner, ScanError> {
        if batch_size == 0 {
            return Err(ScanError::Io(GlobError::new(
                root.as_ref().to_path_buf(),
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "batch_size must be greater than zero",
                ),
            )));
        }
        let (components, require_dir) = compile_pattern(pattern).map_err(ScanError::Pattern)?;
        let root = root.as_ref().to_path_buf();
        let root_identity = root_identity(&root);
        ensure_root_dir(&root_identity)?;

        let first_recursive = components[0].is_recursive_pattern();
        // The root directory itself is never delivered as a match (glob only
        // yields entries under the scope), so the initial frame does not
        // self-emit even for a bare `**` pattern.
        let first_emit_self = false;

        Ok(BatchScanner {
            pattern: pattern.to_string(),
            components,
            options,
            require_dir,
            root,
            root_identity,
            batch_size,
            stack: vec![new_frame(
                PathBuf::new(),
                0,
                first_recursive,
                first_emit_self,
            )],
            finished: false,
            tainted: false,
        })
    }

    /// Resume a scan from a cursor previously produced by [`Batch::cursor`] or
    /// [`BatchScanner::cursor`].
    ///
    /// `pattern`, `options` and `root` must match the scan the cursor was
    /// produced with. Any mismatch is rejected with [`CursorError`] — a cursor
    /// cannot be reused after changing configuration. The batch size is read
    /// from the cursor.
    pub fn resume(
        cursor: &str,
        pattern: &str,
        options: MatchOptions,
        root: impl AsRef<Path>,
    ) -> Result<BatchScanner, CursorError> {
        let parsed = parse_cursor(cursor)?;
        if parsed.pattern != pattern {
            return Err(CursorError::PatternMismatch {
                stored: parsed.pattern,
                given: pattern.to_string(),
            });
        }
        if parsed.options != options {
            return Err(CursorError::OptionsMismatch);
        }
        let root = root.as_ref().to_path_buf();
        let root_identity = root_identity(&root);
        if parsed.root_identity != root_identity {
            return Err(CursorError::RootMismatch {
                stored: parsed.root_identity.display().to_string(),
                given: root_identity.display().to_string(),
            });
        }
        ensure_root_dir(&root_identity)
            .map_err(|e| CursorError::Corrupt(format!("scan root unavailable: {e}")))?;

        Ok(BatchScanner {
            pattern: parsed.pattern,
            components: parsed.components,
            options,
            require_dir: parsed.require_dir,
            root,
            root_identity,
            batch_size: parsed.batch_size,
            stack: parsed.frames,
            finished: parsed.finished,
            tainted: false,
        })
    }

    /// The pattern this scanner is bound to.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// The match options this scanner is bound to.
    pub fn options(&self) -> MatchOptions {
        self.options
    }

    /// The root directory matches are joined under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Maximum number of matched paths returned per batch.
    pub fn batch_size(&self) -> usize {
        self.batch_size
    }

    /// Render the current resume cursor without advancing the scan.
    pub fn cursor(&self) -> String {
        self.render_cursor()
    }

    /// Advance the scan and return the next batch, containing at most
    /// `batch_size` matched paths.
    ///
    /// Entries that do not match the pattern still advance the traversal, so a
    /// batch containing zero `matches` can still move the frontier (and its
    /// cursor) forward. I/O errors from unreadable directories are collected
    /// in [`Batch::errors`] and do not consume match capacity.
    pub fn next_batch(&mut self) -> Batch {
        let mut paths: Vec<PathBuf> = Vec::new();
        let mut errors: Vec<GlobError> = Vec::new();
        let mut changes: Vec<DirectoryChange> = Vec::new();

        while paths.len() < self.batch_size {
            match self.step(&mut changes) {
                Some(Ok(path)) => paths.push(path),
                Some(Err(err)) => errors.push(err),
                None => break,
            }
        }

        let finished = self.finished;
        let snapshot_consistent = !self.tainted && changes.is_empty();
        let cursor = self.render_cursor();
        Batch {
            matches: paths,
            errors,
            changes,
            cursor,
            finished,
            snapshot_consistent,
        }
    }

    /// Pop the next matched path. `changes` accumulates directory-change
    /// reports for resumed frames as they are loaded.
    fn step(&mut self, changes: &mut Vec<DirectoryChange>) -> Option<Result<PathBuf, GlobError>> {
        loop {
            // Locate the deepest frame with work.
            let idx = self.stack.len().checked_sub(1)?;
            let frame = &mut self.stack[idx];

            if !frame.loaded {
                if let Err(err) = self.load_frame(idx, changes) {
                    return Some(Err(err));
                }
                continue;
            }

            if frame.emit_self {
                frame.emit_self = false;
                let rel = frame.rel.clone();
                return Some(Ok(join_rel(&self.root, &rel)));
            }

            match frame.entries.pop() {
                Some(entry) => {
                    // One more ascending entry of `observed` is handed out.
                    frame.consumed += 1;
                    // Drop the mutable borrow before process_entry re-borrows
                    // self.stack.
                    let work = EntryWork {
                        frame_idx: idx,
                        name: entry.name,
                        is_dir: entry.is_dir,
                    };
                    if let Some(result) = self.process_entry(work) {
                        return Some(result);
                    }
                }
                None => {
                    self.stack.pop();
                    if self.stack.is_empty() {
                        self.finished = true;
                        return None;
                    }
                }
            }
        }
    }

    /// Read a frame's directory, sort entries and (on resume) continue past
    /// the cursor watermark while reporting additions and removals.
    fn load_frame(
        &mut self,
        idx: usize,
        changes: &mut Vec<DirectoryChange>,
    ) -> Result<(), GlobError> {
        let (dir_path, resume) = {
            let frame = &mut self.stack[idx];
            (join_rel(&self.root, &frame.rel), frame.resume.take())
        };

        let mut live: Vec<EntrySnapshot> = match read_entries_sorted(&dir_path, self.options) {
            Ok(entries) => entries,
            Err(e) => {
                self.stack.pop();
                return Err(GlobError::new(dir_path, e));
            }
        };
        // Ascending display order; reversed below for end-popping.
        live.sort_by(|a, b| a.name.cmp(&b.name));

        let mut queue: Vec<EntrySnapshot>;
        let observed: Vec<EntrySnapshot>;
        let consumed: usize;

        match resume {
            None => {
                // Fresh directory: visit everything.
                observed = live.clone();
                consumed = 0;
                queue = live;
            }
            Some(snap) => {
                let old_observed = snap.observed;
                let old_enc: BTreeSet<String> = old_observed
                    .iter()
                    .map(|e| encode_component(&e.name))
                    .collect();
                let watermark: Option<&OsString> = if snap.consumed == 0 {
                    None
                } else {
                    old_observed.get(snap.consumed - 1).map(|e| &e.name)
                };

                // The whole current listing becomes the new observed state.
                observed = live.clone();

                // The resume position is every live entry at or before the
                // last delivered name.
                consumed = match watermark {
                    Some(mark) => live
                        .iter()
                        .filter(|e| e.name.as_os_str() <= mark.as_os_str())
                        .count(),
                    None => 0,
                };

                // Continue the live walk forward: only live entries strictly
                // after the watermark are visited. Entries at or before it
                // stay delivered; a newly inserted entry there is reported in
                // `added` but cannot be visited without breaking the fixed
                // order (see the module-level consistency boundaries).
                queue = live
                    .into_iter()
                    .filter(|e| {
                        watermark.map_or(true, |mark| e.name.as_os_str() > mark.as_os_str())
                    })
                    .collect();

                let live_enc: BTreeSet<String> =
                    observed.iter().map(|e| encode_component(&e.name)).collect();
                let new_fingerprint = fingerprint_encoded(&live_enc);
                let old_fingerprint = fingerprint_encoded(&old_enc);
                if old_fingerprint != new_fingerprint {
                    self.tainted = true;
                    changes.push(DirectoryChange {
                        directory: self.stack[idx].rel.clone(),
                        added: live_enc.difference(&old_enc).cloned().collect(),
                        removed: old_enc.difference(&live_enc).cloned().collect(),
                        previous_fingerprint: old_fingerprint,
                        current_fingerprint: new_fingerprint,
                    });
                }
            }
        }

        // Popping is from the end; store descending.
        queue.reverse();
        let fingerprint =
            fingerprint_encoded(&observed.iter().map(|e| encode_component(&e.name)).collect());

        let frame = &mut self.stack[idx];
        frame.entries = queue;
        frame.loaded = true;
        frame.observed = observed;
        frame.consumed = consumed;
        frame.fingerprint = fingerprint;
        frame.fingerprint_valid = true;
        Ok(())
    }

    /// Consider one candidate directory entry against the current frame.
    /// Returns a matched path or pushes descent frames and returns `None`.
    fn process_entry(&mut self, work: EntryWork) -> Option<Result<PathBuf, GlobError>> {
        let (rel, pat_idx, recursive) = {
            let frame = &self.stack[work.frame_idx];
            (frame.rel.clone(), frame.pat_idx, frame.recursive)
        };
        let child_rel = join_rel_components(&rel, &work.name);
        let last = self.components.len() - 1;

        if recursive {
            // Stack frames are visited top-first (popped from the end), so the
            // order of pushes below arranges a depth-first walk where a name
            // matched via the zero-depth route (branch A) is considered before
            // descending further (branch B), matching `glob` ordering.

            // Branch B: continue descending with the same recursive index.
            // Pushed first so it sits beneath branch A.
            if work.is_dir {
                self.stack
                    .push(new_frame(child_rel.clone(), pat_idx, true, pat_idx == last));
            }

            // Branch A: the recursive sequence consumes zero+ directories, so
            // try matching this entry with the next pattern component. Pushed
            // last so it is processed first.
            if pat_idx < last {
                let next_pat = &self.components[pat_idx + 1];
                if name_matches(next_pat, &work.name, self.options) {
                    if pat_idx + 1 == last {
                        if !self.require_dir || work.is_dir {
                            return Some(Ok(join_rel(&self.root, &child_rel)));
                        }
                    } else if work.is_dir {
                        let next_recursive = self.components[pat_idx + 2].is_recursive_pattern();
                        let next_emit = next_recursive && pat_idx + 2 == last;
                        self.stack.push(new_frame(
                            child_rel,
                            pat_idx + 2,
                            next_recursive,
                            next_emit,
                        ));
                    }
                }
            }
            return None;
        }

        // Non-recursive component.
        if !name_matches(&self.components[pat_idx], &work.name, self.options) {
            return None;
        }
        if pat_idx == last {
            if self.require_dir && !work.is_dir {
                return None;
            }
            Some(Ok(join_rel(&self.root, &child_rel)))
        } else if work.is_dir {
            let next_recursive = self.components[pat_idx + 1].is_recursive_pattern();
            self.stack
                .push(new_frame(child_rel, pat_idx + 1, next_recursive, false));
            None
        } else {
            None
        }
    }
}

struct EntryWork {
    frame_idx: usize,
    name: OsString,
    is_dir: bool,
}

fn new_frame(rel: PathBuf, pat_idx: usize, recursive: bool, emit_self: bool) -> Frame {
    Frame {
        rel,
        pat_idx,
        recursive,
        entries: Vec::new(),
        loaded: false,
        observed: Vec::new(),
        consumed: 0,
        resume: None,
        emit_self,
        fingerprint: 0,
        fingerprint_valid: false,
    }
}

fn read_entries_sorted(dir: &Path, options: MatchOptions) -> io::Result<Vec<EntrySnapshot>> {
    let mut out = Vec::new();
    for de in fs::read_dir(dir)? {
        let de = de?;
        let name = de.file_name();
        // Match the glob crate's fill_todo: when a literal leading dot is
        // required, dot entries never participate in matching or descent.
        if options.require_literal_leading_dot
            && name.to_str().map_or(false, |s| s.starts_with('.'))
        {
            continue;
        }
        let is_dir = is_directory_entry(&de, &dir.join(&name));
        out.push(EntrySnapshot { name, is_dir });
    }
    Ok(out)
}

fn is_directory_entry(de: &fs::DirEntry, full: &Path) -> bool {
    match de.file_type() {
        Ok(ft) => {
            if ft.is_symlink() {
                fs::metadata(full).map(|m| m.is_dir()).unwrap_or(false)
            } else {
                ft.is_dir()
            }
        }
        Err(_) => fs::metadata(full).map(|m| m.is_dir()).unwrap_or(false),
    }
}

fn name_matches(pattern: &Pattern, name: &OsString, options: MatchOptions) -> bool {
    match name.to_str() {
        Some(s) => pattern.matches_with(s, options),
        // The glob crate itself skips non-UTF-8 names for wildcard matching.
        None => false,
    }
}

fn join_rel(root: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

fn join_rel_components(rel: &Path, name: &OsString) -> PathBuf {
    if rel.as_os_str().is_empty() {
        PathBuf::from(name)
    } else {
        rel.join(name)
    }
}

fn root_identity(root: &Path) -> PathBuf {
    fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

fn ensure_root_dir(identity: &Path) -> Result<(), ScanError> {
    match fs::metadata(identity) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(ScanError::Io(GlobError::new(
            identity.to_path_buf(),
            io::Error::new(io::ErrorKind::InvalidInput, "scan root is not a directory"),
        ))),
        Err(e) => Err(ScanError::Io(GlobError::new(identity.to_path_buf(), e))),
    }
}

fn compile_pattern(pattern: &str) -> Result<(Vec<Pattern>, bool), crate::PatternError> {
    if pattern.is_empty() {
        return Err(crate::PatternError::new(
            0,
            "batch pattern must not be empty",
        ));
    }
    // Validate the whole pattern first (malformed **, ranges, ...).
    let _whole = Pattern::new(pattern)?;
    let require_dir = pattern
        .chars()
        .next_back()
        .map_or(false, |c| c == '/' || c == '\\');

    let mut components: Vec<Pattern> = Vec::new();
    for comp in Path::new(pattern).components() {
        match comp {
            Component::Normal(os) => {
                let s = os.to_str().ok_or_else(|| {
                    crate::PatternError::new(0, "pattern components must be valid UTF-8")
                })?;
                let p = Pattern::new(s)?;
                if p.is_recursive_pattern()
                    && components
                        .last()
                        .map_or(false, |c| c.is_recursive_pattern())
                {
                    // Collapse consecutive recursive components (a/**/**/b).
                    continue;
                }
                components.push(p);
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => {
                return Err(crate::PatternError::new(
                    0,
                    "batch patterns must be relative to the scan root (no leading / or ..)",
                ));
            }
        }
    }
    if components.is_empty() {
        return Err(crate::PatternError::new(
            0,
            "batch pattern must contain at least one path component",
        ));
    }
    Ok((components, require_dir))
}

// ---------------------------------------------------------------------------
// Fingerprint
// ---------------------------------------------------------------------------

/// FNV-1a 64-bit over the encoded, sorted entry-name set. Directories with the
/// same entry-name multiset share a fingerprint; any add or remove changes it.
fn fingerprint_encoded(names: &BTreeSet<String>) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for name in names {
        for byte in name.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash ^= 0x0a;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

// ---------------------------------------------------------------------------
// Path/entry-name encoding (also used by DirectoryChange reports)
// ---------------------------------------------------------------------------

/// Percent-encode one path component (a directory entry name).
///
/// Unreserved characters (`A-Za-z0-9-._~`) are kept verbatim; every other
/// byte, including `/`, spaces and non-ASCII or non-UTF-8 data, is written as
/// `%XX`. This makes entry names unambiguous both inside the cursor text and
/// inside [`DirectoryChange`] reports.
///
/// On Unix the encoded bytes are the raw OS bytes; on Windows they are the
/// UTF-16 code units of the name (big endian), matching the platform ordering
/// basis used to sort directory entries.
pub fn encode_component(name: &OsStr) -> String {
    let bytes = os_to_bytes(name);
    let mut out = String::with_capacity(bytes.len());
    for b in bytes {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~');
        if unreserved {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(hex_nibble(b >> 4));
            out.push(hex_nibble(b & 0x0f));
        }
    }
    out
}

/// Decode a value produced by [`encode_component`].
pub fn decode_component(encoded: &str) -> Result<OsString, CursorError> {
    bytes_to_os(decode_percent(encoded)?)
}

/// Percent-encode a relative path, keeping `/` between components.
pub fn encode_path(path: &Path) -> String {
    let mut out = String::new();
    for (i, comp) in path.components().enumerate() {
        if i > 0 {
            out.push('/');
        }
        match comp {
            Component::Normal(os) => out.push_str(&encode_component(os)),
            Component::CurDir => out.push('.'),
            Component::ParentDir => out.push_str(".."),
            other => out.push_str(&encode_component(other.as_os_str())),
        }
    }
    out
}

/// Decode a relative path produced by [`encode_path`].
pub fn decode_path(encoded: &str) -> Result<PathBuf, CursorError> {
    if encoded.is_empty() {
        return Ok(PathBuf::new());
    }
    let mut path = PathBuf::new();
    for part in encoded.split('/') {
        path.push(decode_component(part)?);
    }
    Ok(path)
}

fn hex_nibble(n: u8) -> char {
    char::from_digit(u32::from(n), 16).expect("nibble is 0..16")
}

fn decode_percent(encoded: &str) -> Result<Vec<u8>, CursorError> {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(CursorError::Corrupt("truncated % escape".to_string()));
            }
            let hi = hex_value(bytes[i + 1])?;
            let lo = hex_value(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

fn hex_value(b: u8) -> Result<u8, CursorError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(CursorError::Corrupt("bad hex digit in cursor".to_string())),
    }
}

fn encode_text(s: &str) -> String {
    encode_component(OsStr::new(s))
}

fn decode_text(encoded: &str) -> Result<String, CursorError> {
    decode_component(encoded)?
        .into_string()
        .map_err(|_| CursorError::Corrupt("expected UTF-8 text field in cursor".to_string()))
}

#[cfg(unix)]
fn os_to_bytes(os: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    os.as_bytes().to_vec()
}

#[cfg(unix)]
fn bytes_to_os(bytes: Vec<u8>) -> Result<OsString, CursorError> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(bytes))
}

#[cfg(windows)]
fn os_to_bytes(os: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    // WTF-8: identical to UTF-8 for normal text but also represents unpaired
    // UTF-16 surrogates losslessly, so any Windows file name round-trips while
    // ordinary names stay human-readable.
    wtf8_encode(os.encode_wide())
}

#[cfg(windows)]
fn bytes_to_os(bytes: Vec<u8>) -> Result<OsString, CursorError> {
    use std::os::windows::ffi::OsStringExt;
    let units = wtf8_decode(&bytes)?;
    Ok(OsString::from_wide(&units))
}

/// Encode a sequence of UTF-16 code units as WTF-8.
#[cfg(windows)]
fn wtf8_encode<I: Iterator<Item = u16>>(units: I) -> Vec<u8> {
    let mut iter = units.peekable();
    let mut out = Vec::new();
    while let Some(u) = iter.next() {
        if !(0xD800..=0xDBFF).contains(&u) {
            emit_scalar(u32::from(u), &mut out);
            continue;
        }
        // High surrogate: pair it with a following low surrogate when present.
        match iter.peek() {
            Some(&low) if (0xDC00..=0xDFFF).contains(&low) => {
                iter.next();
                let c = 0x1_0000 + (u32::from(u - 0xD800) << 10) + u32::from(low - 0xDC00);
                emit_scalar(c, &mut out);
            }
            _ => {
                // Unpaired high surrogate (CESU-8 style encoding).
                let v = u - 0xD800;
                out.push(0xED);
                out.push(0xA0 + ((v >> 6) as u8));
                out.push(0x80 + ((v & 0x3F) as u8));
            }
        }
    }
    out
}

#[cfg(windows)]
fn emit_scalar(c: u32, out: &mut Vec<u8>) {
    if c < 0x80 {
        out.push(c as u8);
    } else if c < 0x800 {
        out.push(0xC0 | (c >> 6) as u8);
        out.push(0x80 | (c & 0x3F) as u8);
    } else if c < 0x1_0000 {
        if (0xD800..=0xDFFF).contains(&c) {
            // Unpaired low surrogate passed straight through.
            let v = (c - 0xDC00) as u16;
            out.push(0xED);
            out.push(0xB0 + ((v >> 6) as u8));
            out.push(0x80 + ((v & 0x3F) as u8));
        } else {
            out.push(0xE0 | (c >> 12) as u8);
            out.push(0x80 | ((c >> 6) & 0x3F) as u8);
            out.push(0x80 | (c & 0x3F) as u8);
        }
    } else {
        out.push(0xF0 | (c >> 18) as u8);
        out.push(0x80 | ((c >> 12) & 0x3F) as u8);
        out.push(0x80 | ((c >> 6) & 0x3F) as u8);
        out.push(0x80 | (c & 0x3F) as u8);
    }
}

/// Decode WTF-8 bytes back into UTF-16 code units.
#[cfg(windows)]
fn wtf8_decode(bytes: &[u8]) -> Result<Vec<u16>, CursorError> {
    fn at(bytes: &[u8], i: usize) -> Result<u8, CursorError> {
        let b = *bytes
            .get(i)
            .ok_or_else(|| CursorError::Corrupt("truncated WTF-8 sequence".into()))?;
        if b & 0xC0 != 0x80 {
            return Err(CursorError::Corrupt("bad WTF-8 continuation byte".into()));
        }
        Ok(b & 0x3F)
    }

    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        if b0 < 0x80 {
            out.push(u16::from(b0));
            i += 1;
        } else if b0 & 0xE0 == 0xC0 {
            let c = (u32::from(b0 & 0x1F) << 6) | u32::from(at(bytes, i + 1)?);
            out.push(
                u16::try_from(c)
                    .map_err(|_| CursorError::Corrupt("overlong or invalid 2-byte WTF-8".into()))?,
            );
            i += 2;
        } else if b0 & 0xF0 == 0xE0 {
            let c = (u32::from(b0 & 0x0F) << 12)
                | (u32::from(at(bytes, i + 1)?) << 6)
                | u32::from(at(bytes, i + 2)?);
            if (0xD800..=0xDFFF).contains(&c) {
                // Encoded unpaired surrogate: emit the lone code unit.
                out.push(c as u16);
            } else {
                out.push(
                    u16::try_from(c)
                        .map_err(|_| CursorError::Corrupt("invalid 3-byte WTF-8 scalar".into()))?,
                );
            }
            i += 3;
        } else if b0 & 0xF8 == 0xF0 {
            let c = (u32::from(b0 & 0x07) << 18)
                | (u32::from(at(bytes, i + 1)?) << 12)
                | (u32::from(at(bytes, i + 2)?) << 6)
                | u32::from(at(bytes, i + 3)?);
            if !(0x1_0000..=0x10_FFFF).contains(&c) {
                return Err(CursorError::Corrupt(
                    "out of range 4-byte WTF-8 sequence".into(),
                ));
            }
            let v = c - 0x1_0000;
            out.push(0xD800 + (v >> 10) as u16);
            out.push(0xDC00 + (v & 0x3FF) as u16);
            i += 4;
        } else {
            return Err(CursorError::Corrupt("bad WTF-8 lead byte".into()));
        }
    }
    Ok(out)
}

#[cfg(not(any(unix, windows)))]
fn os_to_bytes(os: &OsStr) -> Vec<u8> {
    os.to_string_lossy().as_bytes().to_vec()
}

#[cfg(not(any(unix, windows)))]
fn bytes_to_os(bytes: Vec<u8>) -> Result<OsString, CursorError> {
    String::from_utf8(bytes)
        .map(OsString::from)
        .map_err(|_| CursorError::Corrupt("invalid UTF-8 payload in cursor".to_string()))
}

// ---------------------------------------------------------------------------
// Cursor serialization
// ---------------------------------------------------------------------------

fn options_bits(o: MatchOptions) -> u8 {
    (u8::from(o.case_sensitive))
        | (u8::from(o.require_literal_separator) << 1)
        | (u8::from(o.require_literal_leading_dot) << 2)
}

fn bits_options(bits: u8) -> MatchOptions {
    MatchOptions {
        case_sensitive: bits & 1 != 0,
        require_literal_separator: bits & 2 != 0,
        require_literal_leading_dot: bits & 4 != 0,
    }
}

fn u64_to_hex(v: u64) -> String {
    format!("{v:016x}")
}

fn hex_to_u64(s: &str) -> Result<u64, CursorError> {
    u64::from_str_radix(s, 16)
        .map_err(|_| CursorError::Corrupt(format!("expected hex integer, found {s:?}")))
}

impl BatchScanner {
    fn render_cursor(&self) -> String {
        let mut out = String::new();
        out.push_str(FORMAT_ID);
        out.push('\n');
        out.push_str(&encode_text(&self.pattern));
        out.push('\n');
        out.push_str(&encode_text(&self.root_identity.to_string_lossy()));
        out.push('\n');
        out.push(char::from_digit(u32::from(options_bits(self.options)), 16).unwrap());
        out.push('\n');
        out.push_str(&u64_to_hex(self.batch_size as u64));
        out.push('\n');
        out.push('\n');
        for frame in &self.stack {
            out.push_str("F ");
            out.push_str(&encode_path(&frame.rel));
            out.push(' ');
            out.push_str(&u64_to_hex(frame.pat_idx as u64));
            out.push(' ');
            out.push(if frame.recursive { '1' } else { '0' });
            out.push(' ');
            out.push(if frame.emit_self { '1' } else { '0' });
            out.push(' ');
            if frame.fingerprint_valid {
                out.push_str(&u64_to_hex(frame.fingerprint));
            } else {
                out.push('-');
            }
            out.push('\n');
            if frame.fingerprint_valid {
                // A frame parsed from a cursor but not re-read this run keeps
                // its listing in `resume`; render from there so the watermark
                // survives a batch that did not touch the frame.
                let (observed, consumed): (&[EntrySnapshot], usize) = match &frame.resume {
                    Some(snap) if !frame.loaded => (&snap.observed, snap.consumed),
                    _ => (&frame.observed, frame.consumed),
                };
                // Resume watermark: consumed prefix length of the observed
                // ascending listing.
                out.push_str("N ");
                out.push_str(&u64_to_hex(consumed as u64));
                out.push('\n');
                // Full observed listing, ascending so the text is stable.
                for entry in observed {
                    out.push_str("E ");
                    out.push_str(&encode_component(&entry.name));
                    out.push(' ');
                    out.push(if entry.is_dir { 'd' } else { 'f' });
                    out.push('\n');
                }
            }
        }
        out.push_str("END\n");
        out
    }
}

struct ParsedCursor {
    pattern: String,
    root_identity: PathBuf,
    options: MatchOptions,
    batch_size: usize,
    require_dir: bool,
    components: Vec<Pattern>,
    frames: Vec<Frame>,
    finished: bool,
}

fn parse_cursor(text: &str) -> Result<ParsedCursor, CursorError> {
    let mut lines = text.lines();
    let first = lines
        .next()
        .ok_or_else(|| CursorError::Corrupt("empty cursor".to_string()))?;
    if first != FORMAT_ID {
        return Err(CursorError::Corrupt(format!(
            "unexpected format id {first:?}"
        )));
    }
    let pattern = decode_text(
        lines
            .next()
            .ok_or_else(|| CursorError::Corrupt("missing pattern".to_string()))?,
    )?;
    let root_identity = PathBuf::from(decode_text(
        lines
            .next()
            .ok_or_else(|| CursorError::Corrupt("missing root".to_string()))?,
    )?);
    let options_line = lines
        .next()
        .ok_or_else(|| CursorError::Corrupt("missing options".to_string()))?;
    if options_line.len() != 1 {
        return Err(CursorError::Corrupt("bad options field".to_string()));
    }
    let options = bits_options(
        hex_value(options_line.as_bytes()[0])
            .map_err(|_| CursorError::Corrupt("bad options nibble".to_string()))?,
    );
    let batch_size = hex_to_u64(
        lines
            .next()
            .ok_or_else(|| CursorError::Corrupt("missing batch size".to_string()))?,
    )? as usize;
    if batch_size == 0 {
        return Err(CursorError::Corrupt("zero batch size".to_string()));
    }
    let blank = lines
        .next()
        .ok_or_else(|| CursorError::Corrupt("missing header terminator".to_string()))?;
    if !blank.is_empty() {
        return Err(CursorError::Corrupt(
            "expected blank line after header".to_string(),
        ));
    }

    let (components, require_dir) = compile_pattern(&pattern)
        .map_err(|e| CursorError::Corrupt(format!("stored pattern no longer valid: {e}")))?;

    let mut frames: Vec<Frame> = Vec::new();
    loop {
        let line = match lines.next() {
            Some(l) => l,
            None => return Err(CursorError::Corrupt("cursor truncated before END".into())),
        };
        if line == "END" {
            if lines.next().is_some() {
                return Err(CursorError::Corrupt("trailing data after END".into()));
            }
            break;
        }
        if let Some(rest) = line.strip_prefix("F ") {
            let mut fields = rest.split(' ');
            let rel = decode_path(
                fields
                    .next()
                    .ok_or_else(|| CursorError::Corrupt("frame missing rel".into()))?,
            )?;
            let pat_idx = hex_to_u64(
                fields
                    .next()
                    .ok_or_else(|| CursorError::Corrupt("frame missing index".into()))?,
            )? as usize;
            let recursive = parse_bit(
                fields
                    .next()
                    .ok_or_else(|| CursorError::Corrupt("frame missing recursive".into()))?,
            )?;
            let emit_self = parse_bit(
                fields
                    .next()
                    .ok_or_else(|| CursorError::Corrupt("frame missing emit flag".into()))?,
            )?;
            let fp_field = fields
                .next()
                .ok_or_else(|| CursorError::Corrupt("frame missing fingerprint".into()))?;
            if fields.next().is_some() {
                return Err(CursorError::Corrupt("too many frame fields".into()));
            }
            let (fingerprint, valid) = if fp_field == "-" {
                (0u64, false)
            } else {
                (hex_to_u64(fp_field)?, true)
            };
            frames.push(Frame {
                rel,
                pat_idx,
                recursive,
                entries: Vec::new(),
                loaded: false,
                observed: Vec::new(),
                consumed: 0,
                resume: None,
                emit_self,
                fingerprint,
                fingerprint_valid: valid,
            });
        } else if let Some(rest) = line.strip_prefix("N ") {
            let frame = frames
                .last_mut()
                .ok_or_else(|| CursorError::Corrupt("watermark without frame".into()))?;
            if !frame.fingerprint_valid || frame.resume.is_some() {
                return Err(CursorError::Corrupt(
                    "watermark record in invalid position".into(),
                ));
            }
            let consumed = hex_to_u64(rest)? as usize;
            frame.resume = Some(ResumeSnapshot {
                observed: Vec::new(),
                consumed,
            });
        } else if let Some(rest) = line.strip_prefix("E ") {
            let frame = frames
                .last_mut()
                .ok_or_else(|| CursorError::Corrupt("entry without frame".into()))?;
            let mut fields = rest.split(' ');
            let name_enc = fields
                .next()
                .ok_or_else(|| CursorError::Corrupt("entry missing name".into()))?;
            let kind = fields
                .next()
                .ok_or_else(|| CursorError::Corrupt("entry missing kind".into()))?;
            if fields.next().is_some() {
                return Err(CursorError::Corrupt("too many entry fields".into()));
            }
            let is_dir = match kind {
                "d" => true,
                "f" => false,
                _ => return Err(CursorError::Corrupt(format!("bad entry kind {kind:?}"))),
            };
            let snapshot = frame
                .resume
                .as_mut()
                .ok_or_else(|| CursorError::Corrupt("entry record before watermark".into()))?;
            snapshot.observed.push(EntrySnapshot {
                name: decode_component(name_enc)?,
                is_dir,
            });
        } else {
            return Err(CursorError::Corrupt(format!("unknown record {line:?}")));
        }
    }

    for frame in &mut frames {
        if frame.fingerprint_valid {
            let snap = frame
                .resume
                .take()
                .ok_or_else(|| CursorError::Corrupt("read frame missing watermark".into()))?;
            // E records are persisted in ascending order.
            if snap.consumed > snap.observed.len() {
                return Err(CursorError::Corrupt(
                    "consumed count exceeds observed entries".into(),
                ));
            }
            // The directory is re-read lazily on the next step; the snapshot
            // supplies the watermark and change baseline.
            frame.resume = Some(snap);
            frame.loaded = false;
        }
    }

    let finished = frames.is_empty();

    Ok(ParsedCursor {
        pattern,
        root_identity,
        options,
        batch_size,
        require_dir,
        components,
        frames,
        finished,
    })
}

fn parse_bit(s: &str) -> Result<bool, CursorError> {
    match s {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(CursorError::Corrupt(format!(
            "expected 0/1 bit, found {s:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_roundtrips_special_names() {
        for name in [
            "a b.txt",
            "100% done",
            "x/y",
            "caf\u{e9}.log",
            "dot.dot",
            "- leading dash",
        ] {
            let os = OsString::from(name);
            let enc = encode_component(&os);
            assert!(!enc.contains('/'), "encoded {name} must not contain /");
            let back = decode_component(&enc).unwrap();
            assert_eq!(back, os, "roundtrip for {name} ({enc})");
        }
        // Paths keep a real slash separator only between components.
        let p = PathBuf::from("dir name/a b.txt");
        let enc = encode_path(&p);
        assert_eq!(enc, "dir%20name/a%20b.txt");
        assert_eq!(decode_path(&enc).unwrap(), p);
    }

    #[test]
    fn options_bits_roundtrip() {
        for bits in 0u8..8 {
            let opts = bits_options(bits);
            assert_eq!(options_bits(opts), bits);
        }
    }
}

//! Explicit, resumable, batched directory scanning.
//!
//! This module provides [`BatchScanner`], an alternative to the [`glob`](crate::glob)
//! iterator that delivers matched paths in explicit batches of a bounded size,
//! fixes the order within each directory to ascending comparable file names, and
//! produces a persistent, resumable [`Cursor`] after every batch.
//!
//! # Consistency boundaries (read before use)
//!
//! A batch scan is **not** a transactional snapshot of the filesystem:
//!
//! * Traversal is deterministic and depth-first, one directory at a time. Once a
//!   directory has been scanned and the cursor has moved past it, files added to
//!   it are not guaranteed to be found, and files removed from it are not
//!   guaranteed to be reflected in later batches.
//! * When a scan is resumed, every directory that was observed before is
//!   re-fingerprinted (device/inode/modification time on Unix, file attributes
//!   and timestamps on Windows). A different fingerprint is reported as
//!   [`FsChange::DirChanged`]; it is a change signal, not a proof of identity.
//! * Paths that were delivered in earlier batches and no longer exist when the
//!   scan resumes are reported as [`FsChange::Deleted`].
//! * Renames may appear as a [`FsChange::Deleted`] plus an addition; additions in
//!   directories the cursor already passed are not picked up.
//! * The cursor binds the pattern, the match options and the canonical root
//!   path. A resume with a different pattern or options, or against a root that
//!   was replaced, is rejected with a [`ResumeError`] instead of silently
//!   starting a different scan.
//! * Unreadable directories are reported as [`GlobError`](crate::GlobError)
//!   items and traversal continues, exactly like [`glob`](crate::glob).
//!
//! In short: the scanner guarantees not to repeat delivered paths and to flag
//! observed changes, but it does **not** guarantee that the result set is
//! identical to a fresh, complete scan of the current filesystem.
//!
//! # Example
//!
//! ```rust,no_run
//! use glob::batch::BatchScanner;
//! use glob::MatchOptions;
//!
//! # fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let mut scanner = BatchScanner::new("data/**/*.csv", MatchOptions::new(), 100)?;
//! let batch = scanner.next_batch();
//! for path in batch.items.iter().filter_map(Result::ok) {
//!     println!("matched: {}", path.display());
//! }
//! let saved = batch.cursor.to_base64(); // persist for a later run
//! # Ok(())
//! # }
//! ```

use std::cmp;
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{self, Component, Path, PathBuf};

use crate::{GlobError, GlobResult, MatchOptions, Pattern, PatternError, PatternToken};

/// The result of a single batch: the matched paths (and read errors), the
/// advanced cursor, any changes observed while resuming, and whether the whole
/// scan is exhausted.
#[derive(Debug)]
pub struct Batch {
    /// The items delivered in this batch. Each is either a matched path or a
    /// directory-read error. The number of `Ok` items never exceeds the batch
    /// limit the scanner was created with.
    pub items: Vec<GlobResult>,

    /// The cursor state at the end of this batch. It can be persisted and used
    /// to [`resume`](BatchScanner::resume) the scan later.
    pub cursor: Cursor,

    /// Filesystem changes observed while resuming. These are drained once: the
    /// first batch of a resumed scanner carries the changes observed while
    /// re-establishing the scan.
    pub changes: Vec<FsChange>,

    /// `true` if the entire scan has been traversed and no further batches will
    /// yield items.
    pub exhausted: bool,
}

/// A change observed in the filesystem while a batch scan was (re)established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsChange {
    /// A directory that was scanned before now has a different fingerprint, or
    /// it could no longer be stat'ed.
    DirChanged {
        /// The directory path, in the same output style as the matched paths.
        path: PathBuf,
    },

    /// A path that was delivered in an earlier batch no longer exists.
    Deleted {
        /// The path that was delivered before and is now missing.
        path: PathBuf,
    },
}

/// An error returned when a [`Cursor`] cannot be resumed.
#[derive(Debug)]
pub enum ResumeError {
    /// The pattern passed to [`BatchScanner::resume`] does not match the
    /// pattern the cursor was created with.
    PatternMismatch {
        /// The pattern the cursor was created with.
        expected: String,
        /// The pattern that was passed to `resume`.
        found: String,
    },

    /// The match options passed to [`BatchScanner::resume`] do not match the
    /// options the cursor was created with.
    OptionsMismatch {
        /// The options the cursor was created with.
        expected: MatchOptions,
        /// The options that were passed to `resume`.
        found: MatchOptions,
    },

    /// The canonical root path the cursor is bound to cannot be found anymore
    /// or differs from the stored root identity.
    RootMismatch {
        /// The root path the cursor is bound to.
        expected: PathBuf,
        /// The root path that was found (empty if it could not be stat'ed).
        found: PathBuf,
    },

    /// The cursor bytes are corrupt, truncated, from an incompatible version,
    /// or were produced on a different platform.
    InvalidCursor(String),

    /// The pattern passed to `resume` is not a valid glob pattern.
    InvalidPattern(PatternError),
}

impl fmt::Display for ResumeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResumeError::PatternMismatch { expected, found } => write!(
                f,
                "cursor was created for pattern {:?}, cannot resume with {:?}",
                expected, found
            ),
            ResumeError::OptionsMismatch { expected, found } => write!(
                f,
                "cursor was created with options {:?}, cannot resume with {:?}",
                expected, found
            ),
            ResumeError::RootMismatch { expected, found } => write!(
                f,
                "cursor is bound to root {}, which no longer matches the current root {}",
                expected.display(),
                found.display()
            ),
            ResumeError::InvalidCursor(msg) => write!(f, "invalid cursor: {}", msg),
            ResumeError::InvalidPattern(e) => write!(f, "invalid pattern: {}", e),
        }
    }
}

impl Error for ResumeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ResumeError::InvalidPattern(e) => Some(e),
            _ => None,
        }
    }
}

/// An error returned when starting a [`BatchScanner`].
#[derive(Debug)]
pub enum ScanError {
    /// The pattern is not a valid glob pattern.
    Pattern(PatternError),
    /// The root directory could not be determined or canonicalized.
    Io(io::Error),
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::Pattern(e) => write!(f, "invalid pattern: {}", e),
            ScanError::Io(e) => write!(f, "I/O error: {}", e),
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

/// A persistent, resumable cursor for a [`BatchScanner`].
///
/// The cursor is an opaque snapshot of the scan state: the pattern, the match
/// options, the canonical root identity, the traversal stack with per-directory
/// positions, the fingerprints of scanned directories, the set of paths already
/// delivered, and whether the scan is exhausted.
///
/// It can be serialized with [`to_bytes`](Cursor::to_bytes)/
/// [`from_bytes`](Cursor::from_bytes) or with the compact base64 string
/// [`to_base64`](Cursor::to_base64)/[`from_base64`](Cursor::from_base64). Paths
/// inside the cursor are encoded as raw OS bytes (Unix) or UTF-16 (Windows), so
/// non-UTF-8 paths are preserved.
///
/// Cursors are bound to the platform they were produced on: a cursor created on
/// Windows cannot be decoded on Unix and vice versa.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    version: u32,
    pattern: String,
    options: MatchOptions,
    root: PathBuf,
    root_is_absolute: bool,
    root_fingerprint: Option<DirFingerprint>,
    frames: Vec<CursorFrame>,
    completed: Vec<CursorCompleted>,
    delivered: Vec<PathBuf>,
    exhausted: bool,
}

impl Cursor {
    /// Returns `true` if the scan that produced this cursor was fully
    /// traversed. Resuming an exhausted cursor yields no further items.
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Serializes the cursor to a compact binary representation.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Vec::new();
        w.extend_from_slice(CURSOR_MAGIC);
        w.extend_from_slice(&CURSOR_VERSION.to_le_bytes());
        put_bytes(&mut w, self.pattern.as_bytes());
        w.push(self.options.case_sensitive as u8);
        w.push(self.options.require_literal_separator as u8);
        w.push(self.options.require_literal_leading_dot as u8);
        w.push(self.root_is_absolute as u8);
        put_bytes(&mut w, &encode_os(self.root.as_os_str()));
        put_fingerprint(&mut w, self.root_fingerprint.as_ref());

        w.extend_from_slice(&(self.frames.len() as u32).to_le_bytes());
        for f in &self.frames {
            put_bytes(&mut w, &encode_os(f.dir.as_os_str()));
            put_bytes(&mut w, &encode_os(f.rel.as_os_str()));
            w.push(f.curdir as u8);
            w.extend_from_slice(&(f.pattern_idx as u32).to_le_bytes());
            put_bytes(&mut w, &encode_os(f.last_name.as_os_str()));
            put_fingerprint(&mut w, f.fingerprint.as_ref());
        }

        w.extend_from_slice(&(self.completed.len() as u32).to_le_bytes());
        for c in &self.completed {
            put_bytes(&mut w, &encode_os(c.rel.as_os_str()));
            put_fingerprint(&mut w, c.fingerprint.as_ref());
        }

        w.extend_from_slice(&(self.delivered.len() as u32).to_le_bytes());
        for p in &self.delivered {
            put_bytes(&mut w, &encode_os(p.as_os_str()));
        }

        w.push(self.exhausted as u8);
        w
    }

    /// Decodes a cursor previously produced by [`to_bytes`](Cursor::to_bytes).
    pub fn from_bytes(bytes: &[u8]) -> Result<Cursor, ResumeError> {
        let mut r = Reader::new(bytes);
        let magic = r.take(4)?;
        if magic != CURSOR_MAGIC {
            return Err(ResumeError::InvalidCursor(
                "bad magic (not a glob batch cursor)".to_string(),
            ));
        }
        let version = r.u32()?;
        if version != CURSOR_VERSION {
            return Err(ResumeError::InvalidCursor(format!(
                "unsupported cursor version {} (expected {})",
                version, CURSOR_VERSION
            )));
        }
        let pattern = String::from_utf8(r.bytes()?.to_vec())
            .map_err(|_| ResumeError::InvalidCursor("pattern is not utf-8".to_string()))?;
        let case_sensitive = r.u8()? != 0;
        let require_literal_separator = r.u8()? != 0;
        let require_literal_leading_dot = r.u8()? != 0;
        let options = MatchOptions {
            case_sensitive,
            require_literal_separator,
            require_literal_leading_dot,
        };
        let root_is_absolute = r.u8()? != 0;
        let root = decode_os(r.bytes()?)?;
        let root_fingerprint = r.fingerprint()?;

        let frame_count = r.u32()? as usize;
        let mut frames = Vec::with_capacity(frame_count);
        for _ in 0..frame_count {
            let dir = decode_os(r.bytes()?)?;
            let rel = decode_os(r.bytes()?)?;
            let curdir = r.u8()? != 0;
            let pattern_idx = r.u32()? as usize;
            let last_name = decode_os(r.bytes()?)?.into_os_string();
            let fingerprint = r.fingerprint()?;
            frames.push(CursorFrame {
                dir,
                rel,
                curdir,
                pattern_idx,
                last_name,
                fingerprint,
            });
        }

        let completed_count = r.u32()? as usize;
        let mut completed = Vec::with_capacity(completed_count);
        for _ in 0..completed_count {
            let rel = decode_os(r.bytes()?)?;
            let fingerprint = r.fingerprint()?;
            completed.push(CursorCompleted { rel, fingerprint });
        }

        let delivered_count = r.u32()? as usize;
        let mut delivered = Vec::with_capacity(delivered_count);
        for _ in 0..delivered_count {
            delivered.push(decode_os(r.bytes()?)?);
        }

        let exhausted = r.u8()? != 0;
        if r.remaining() != 0 {
            return Err(ResumeError::InvalidCursor(
                "trailing bytes after cursor".to_string(),
            ));
        }

        Ok(Cursor {
            version,
            pattern,
            options,
            root,
            root_is_absolute,
            root_fingerprint,
            frames,
            completed,
            delivered,
            exhausted,
        })
    }

    /// Serializes the cursor to a compact base64 string.
    pub fn to_base64(&self) -> String {
        base64_encode(&self.to_bytes())
    }

    /// Decodes a cursor previously produced by [`to_base64`](Cursor::to_base64).
    pub fn from_base64(s: &str) -> Result<Cursor, ResumeError> {
        let bytes = base64_decode(s)?;
        Cursor::from_bytes(&bytes)
    }
}

/// An explicit, resumable, batched directory scanner.
///
/// The scanner follows the same matching rules as [`glob`](crate::glob) (it
/// compiles the pattern into [`Pattern`]s and matches path components against
/// them), but it traverses directories in fixed ascending order and hands out
/// matched paths in batches of a bounded size. Call [`next_batch`](Self::next_batch)
/// repeatedly until the batch is reported as [`exhausted`](Batch::exhausted).
///
/// After every batch, the returned [`Cursor`] can be persisted and the scan
/// resumed later with [`resume`](Self::resume).
#[derive(Debug)]
pub struct BatchScanner {
    pattern: String,
    options: MatchOptions,
    root: PathBuf,
    root_is_absolute: bool,
    dir_patterns: Vec<Pattern>,
    require_dir: bool,
    frames: Vec<Frame>,
    completed: Vec<(PathBuf, Option<DirFingerprint>)>,
    delivered: Vec<PathBuf>,
    exhausted: bool,
    batch_limit: usize,
    pending_changes: Vec<FsChange>,
}

impl BatchScanner {
    /// Starts a new batched scan for `pattern` using the given match options.
    ///
    /// `batch_limit` is the maximum number of matched paths delivered per
    /// batch; it must be greater than zero. Entries that do not match still
    /// advance the traversal but do not count toward the limit.
    ///
    /// # Errors
    ///
    /// Returns [`ScanError::Pattern`] if the pattern is invalid and
    /// [`ScanError::Io`] if the scan root cannot be determined.
    pub fn new(
        pattern: &str,
        options: MatchOptions,
        batch_limit: usize,
    ) -> Result<BatchScanner, ScanError> {
        assert!(batch_limit > 0, "batch_limit must be greater than zero");

        // Validate the pattern first.
        let _ = Pattern::new(pattern).map_err(ScanError::Pattern)?;

        let (root_len, root_is_absolute) = root_info(pattern);
        let root = if root_is_absolute {
            PathBuf::from(&pattern[..root_len])
        } else {
            env::current_dir().map_err(ScanError::Io)?
        };
        let root = fs::canonicalize(root).map_err(ScanError::Io)?;

        let dir_patterns = build_dir_patterns(pattern, root_len).map_err(ScanError::Pattern)?;
        let require_dir = pattern.chars().next_back().map(path::is_separator) == Some(true);

        let frames = if dir_patterns.is_empty() {
            Vec::new()
        } else {
            vec![Frame {
                dir: root.clone(),
                rel: PathBuf::new(),
                curdir: !root_is_absolute,
                pattern_idx: 0,
                entries: Vec::new(),
                pos: 0,
                fingerprint: None,
                loaded: false,
            }]
        };

        Ok(BatchScanner {
            pattern: pattern.to_string(),
            options,
            root,
            root_is_absolute,
            dir_patterns,
            require_dir,
            frames,
            completed: Vec::new(),
            delivered: Vec::new(),
            exhausted: false,
            batch_limit,
            pending_changes: Vec::new(),
        })
    }

    /// Resumes a scan from a previously persisted [`Cursor`].
    ///
    /// The `pattern` and `options` must be identical to those the cursor was
    /// created with; otherwise the resume is rejected with a
    /// [`ResumeError`]. The cursor also binds the canonical root identity:
    /// a root that was replaced causes a rejection, while changed directory
    /// fingerprints are reported as [`FsChange`]s in the first batch.
    ///
    /// `batch_limit` must be greater than zero.
    pub fn resume(
        pattern: &str,
        options: MatchOptions,
        cursor: Cursor,
        batch_limit: usize,
    ) -> Result<BatchScanner, ResumeError> {
        assert!(batch_limit > 0, "batch_limit must be greater than zero");

        let _ = Pattern::new(pattern).map_err(ResumeError::InvalidPattern)?;
        if pattern != cursor.pattern {
            return Err(ResumeError::PatternMismatch {
                expected: cursor.pattern.clone(),
                found: pattern.to_string(),
            });
        }
        if options != cursor.options {
            return Err(ResumeError::OptionsMismatch {
                expected: cursor.options,
                found: options,
            });
        }

        // Root identity: the stored canonical root must still exist and match.
        let root = fs::canonicalize(&cursor.root).map_err(|_| ResumeError::RootMismatch {
            expected: cursor.root.clone(),
            found: PathBuf::new(),
        })?;
        if root != cursor.root {
            return Err(ResumeError::RootMismatch {
                expected: cursor.root.clone(),
                found: root,
            });
        }

        let dir_patterns = build_dir_patterns(&cursor.pattern, root_info(&cursor.pattern).0)
            .map_err(|e| ResumeError::InvalidCursor(format!("pattern recompiled: {}", e)))?;
        let require_dir = cursor.pattern.chars().next_back().map(path::is_separator) == Some(true);

        let mut pending_changes = Vec::new();

        // Fingerprint of the root.
        if let Some(expected) = &cursor.root_fingerprint {
            if fingerprint(&root).as_ref() != Some(expected) {
                pending_changes.push(FsChange::DirChanged {
                    path: output_path(&root, cursor.root_is_absolute, &root),
                });
            }
        }

        // Fingerprints of fully scanned directories.
        for c in &cursor.completed {
            let target = if c.rel.as_os_str().is_empty() {
                root.clone()
            } else {
                root.join(&c.rel)
            };
            if let Some(expected) = &c.fingerprint {
                if fingerprint(&target).as_ref() != Some(expected) {
                    pending_changes.push(FsChange::DirChanged {
                        path: output_path(&root, cursor.root_is_absolute, &target),
                    });
                }
            }
        }

        // Previously delivered paths that no longer exist.
        for abs in &cursor.delivered {
            let exists = fs::metadata(abs).is_ok() || fs::symlink_metadata(abs).is_ok();
            if !exists {
                pending_changes.push(FsChange::Deleted {
                    path: output_path(&root, cursor.root_is_absolute, abs),
                });
            }
        }

        // Rebuild the traversal stack, re-reading directories and seeking to
        // the saved position.
        let mut frames = Vec::with_capacity(cursor.frames.len());
        for cf in &cursor.frames {
            let mut frame = Frame {
                dir: cf.dir.clone(),
                rel: cf.rel.clone(),
                curdir: cf.curdir,
                pattern_idx: cf.pattern_idx,
                entries: Vec::new(),
                pos: 0,
                fingerprint: None,
                loaded: false,
            };
            let loaded = load_frame(
                &mut frame,
                &dir_patterns,
                &options,
                cursor.root_is_absolute,
                &root,
            );
            match loaded {
                Ok(()) => {
                    if !cf.last_name.is_empty() {
                        let pos = frame.entries.partition_point(|e| e.name <= cf.last_name);
                        frame.pos = pos;
                    }
                    frame.loaded = true;
                }
                Err(_) => {
                    // The directory can no longer be read. Leave the frame
                    // empty; the scan will pop it and report the change.
                    frame.loaded = true;
                }
            }
            if let Some(expected) = &cf.fingerprint {
                if fingerprint(&frame.dir).as_ref() != Some(expected) {
                    pending_changes.push(FsChange::DirChanged {
                        path: output_path(&root, cursor.root_is_absolute, &frame.dir),
                    });
                }
            }
            frames.push(frame);
        }

        Ok(BatchScanner {
            pattern: cursor.pattern,
            options,
            root,
            root_is_absolute: cursor.root_is_absolute,
            dir_patterns,
            require_dir,
            frames,
            completed: cursor.completed.into_iter().map(|c| (c.rel, c.fingerprint)).collect(),
            delivered: cursor.delivered,
            exhausted: cursor.exhausted,
            batch_limit,
            pending_changes,
        })
    }

    /// Delivers the next batch of matched paths.
    ///
    /// Returns as soon as `batch_limit` matched paths have been delivered, or
    /// when the scan is exhausted. Unmatched entries and read errors still
    /// advance the traversal. The returned [`Batch`] carries the advanced
    /// cursor and any observed changes.
    pub fn next_batch(&mut self) -> Batch {
        let mut items = Vec::new();
        let changes = std::mem::take(&mut self.pending_changes);
        let mut ok_count = 0;

        while ok_count < self.batch_limit {
            match self.step() {
                Step::Emit(path) => {
                    items.push(Ok(path));
                    ok_count += 1;
                }
                Step::Error(e) => items.push(Err(e)),
                Step::Continue => {}
                Step::Done => {
                    self.exhausted = true;
                    break;
                }
            }
        }

        // The loop may have ended because the batch limit was reached while
        // completed frames were still waiting to be popped. Determine whether
        // any more items are possible by checking whether every frame is fully
        // processed (loaded and positioned at its end).
        if !self.exhausted {
            self.exhausted = self.frames.is_empty()
                || self
                    .frames
                    .iter()
                    .all(|f| f.loaded && f.pos >= f.entries.len());
        }

        let exhausted = self.exhausted;
        Batch {
            items,
            cursor: self.snapshot(),
            changes,
            exhausted,
        }
    }

    /// Returns the current cursor state without advancing the scan.
    pub fn cursor(&self) -> Cursor {
        self.snapshot()
    }

    /// Returns `true` if the scan has been fully traversed.
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    fn snapshot(&self) -> Cursor {
        Cursor {
            version: CURSOR_VERSION,
            pattern: self.pattern.clone(),
            options: self.options,
            root: self.root.clone(),
            root_is_absolute: self.root_is_absolute,
            root_fingerprint: fingerprint(&self.root),
            frames: self
                .frames
                .iter()
                .map(|f| CursorFrame {
                    dir: f.dir.clone(),
                    rel: f.rel.clone(),
                    curdir: f.curdir,
                    pattern_idx: f.pattern_idx,
                    last_name: if f.pos == 0 {
                        OsString::new()
                    } else {
                        f.entries[f.pos - 1].name.clone()
                    },
                    fingerprint: f.fingerprint,
                })
                .collect(),
            completed: self
                .completed
                .iter()
                .map(|(rel, fingerprint)| CursorCompleted {
                    rel: rel.clone(),
                    fingerprint: *fingerprint,
                })
                .collect(),
            delivered: self.delivered.clone(),
            exhausted: self.exhausted,
        }
    }

    fn step(&mut self) -> Step {
        if self.frames.is_empty() {
            return Step::Done;
        }
        let fi = self.frames.len() - 1;
        if !self.frames[fi].loaded {
            if let Err(e) = load_frame(
                &mut self.frames[fi],
                &self.dir_patterns,
                &self.options,
                self.root_is_absolute,
                &self.root,
            ) {
                let f = self.frames.pop().unwrap();
                self.completed.push((f.rel, f.fingerprint));
                if self.frames.is_empty() {
                    self.exhausted = true;
                }
                return Step::Error(e);
            }
        }
        if self.frames[fi].pos < self.frames[fi].entries.len() {
            let entry = self.frames[fi].entries[self.frames[fi].pos].clone();
            self.frames[fi].pos += 1;
            self.process_entry(fi, &entry)
        } else {
            let f = self.frames.pop().unwrap();
            self.completed.push((f.rel, f.fingerprint));
            if self.frames.is_empty() {
                self.exhausted = true;
                Step::Done
            } else {
                Step::Continue
            }
        }
    }

    fn process_entry(&mut self, fi: usize, entry: &Entry) -> Step {
        let idx = self.frames[fi].pattern_idx;
        let len = self.dir_patterns.len();
        if self.dir_patterns[idx].is_recursive {
            let mut next = idx;
            while next + 1 < len && self.dir_patterns[next + 1].is_recursive {
                next += 1;
            }
            if entry.is_dir {
                // Descend into the directory with the recursive pattern first
                // (it is pushed last so it is processed first, matching the
                // LIFO order of glob's todo stack).
                self.push_child_frame(fi, entry, next, false);
                if next == len - 1 {
                    // A recursive pattern at the end matches the directory.
                    self.emit_entry(fi, entry)
                } else {
                    self.match_against(fi, next + 1, entry)
                }
            } else if next == len - 1 {
                // Not a directory and the recursive pattern is the last
                // pattern: no match.
                Step::Continue
            } else {
                self.match_against(fi, next + 1, entry)
            }
        } else {
            self.match_against(fi, idx, entry)
        }
    }

    fn match_against(&mut self, fi: usize, idx: usize, entry: &Entry) -> Step {
        // glob skips non-UTF-8 names when matching.
        let name = match entry.name.to_str() {
            Some(s) => s,
            None => return Step::Continue,
        };
        if self.dir_patterns[idx].matches_with(name, self.options) {
            if idx == self.dir_patterns.len() - 1 {
                self.emit_entry(fi, entry)
            } else if entry.is_dir {
                self.push_child_frame(fi, entry, idx + 1, false);
                Step::Continue
            } else {
                Step::Continue
            }
        } else {
            Step::Continue
        }
    }

    fn emit_entry(&mut self, fi: usize, entry: &Entry) -> Step {
        if self.require_dir && !entry.is_dir {
            return Step::Continue;
        }
        let frame = &self.frames[fi];
        let abs = frame.dir.join(&entry.name);
        if self.delivered.iter().any(|p| p == &abs) {
            return Step::Continue;
        }
        self.delivered.push(abs);
        let out = entry_output(frame, entry, self.root_is_absolute);
        Step::Emit(out)
    }

    fn push_child_frame(&mut self, fi: usize, entry: &Entry, pattern_idx: usize, special: bool) {
        let parent = &self.frames[fi];
        let (dir, rel, curdir) = if special && entry.name == "." {
            (
                parent.dir.clone(),
                if parent.curdir {
                    PathBuf::new()
                } else {
                    parent.rel.join(".")
                },
                parent.curdir,
            )
        } else if special && entry.name == ".." {
            (
                parent.dir.join(".."),
                if parent.curdir {
                    PathBuf::from("..")
                } else {
                    parent.rel.join("..")
                },
                false,
            )
        } else {
            (
                parent.dir.join(&entry.name),
                if parent.curdir {
                    PathBuf::from(&entry.name)
                } else {
                    parent.rel.join(&entry.name)
                },
                false,
            )
        };
        self.frames.push(Frame {
            dir,
            rel,
            curdir,
            pattern_idx,
            entries: Vec::new(),
            pos: 0,
            fingerprint: None,
            loaded: false,
        });
    }
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Frame {
    /// Absolute path of the directory to read.
    dir: PathBuf,
    /// Output prefix for paths emitted from this frame (relative for relative
    /// roots, empty for absolute roots and for the curdir root).
    rel: PathBuf,
    /// `true` when this frame reads the current directory (`.`), so entries
    /// are emitted as bare file names, like glob does for a relative scope.
    curdir: bool,
    /// Index into the scanner's pattern list that this frame's entries must
    /// match.
    pattern_idx: usize,
    /// Sorted entries of the directory (plus the special `.`/`..` entries).
    entries: Vec<Entry>,
    /// Index of the next entry to process.
    pos: usize,
    /// Fingerprint of the directory (only for frames that read a directory).
    fingerprint: Option<DirFingerprint>,
    /// Whether `entries` has been loaded.
    loaded: bool,
}

#[derive(Debug, Clone)]
struct Entry {
    name: OsString,
    is_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CursorFrame {
    dir: PathBuf,
    rel: PathBuf,
    curdir: bool,
    pattern_idx: usize,
    last_name: OsString,
    fingerprint: Option<DirFingerprint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CursorCompleted {
    rel: PathBuf,
    fingerprint: Option<DirFingerprint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
struct DirFingerprint {
    dev: u64,
    ino: u64,
    mtime_secs: i64,
    mtime_nanos: u32,
    win_attrs: u32,
    win_creation: u64,
    win_write: u64,
}

enum Step {
    Emit(PathBuf),
    Error(GlobError),
    Continue,
    Done,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const CURSOR_MAGIC: &[u8; 4] = b"GBCU";
const CURSOR_VERSION: u32 = 1;

fn root_info(pattern: &str) -> (usize, bool) {
    let mut components = Path::new(pattern).components().peekable();
    loop {
        match components.peek() {
            Some(Component::Prefix(..)) | Some(Component::RootDir) => {
                components.next();
            }
            _ => break,
        }
    }
    let rest = components.map(|s| s.as_os_str()).collect::<PathBuf>();
    let normalized_pattern = Path::new(pattern).iter().collect::<PathBuf>();
    let root_len = normalized_pattern.to_str().unwrap().len() - rest.to_str().unwrap().len();
    (root_len, root_len > 0)
}

fn build_dir_patterns(pattern: &str, root_len: usize) -> Result<Vec<Pattern>, PatternError> {
    let mut dir_patterns = Vec::new();
    let components = pattern[cmp::min(root_len, pattern.len())..].split_terminator(path::is_separator);
    for component in components {
        dir_patterns.push(Pattern::new(component)?);
    }
    if root_len == pattern.len() && root_len > 0 {
        // A pattern that is only a root (e.g. "/") matches the root itself.
        dir_patterns.push(Pattern {
            original: String::new(),
            tokens: Vec::new(),
            is_recursive: false,
            has_metachars: false,
        });
    }
    Ok(dir_patterns)
}

fn entry_output(frame: &Frame, entry: &Entry, root_is_absolute: bool) -> PathBuf {
    if root_is_absolute {
        frame.dir.join(&entry.name)
    } else if frame.curdir {
        PathBuf::from(&entry.name)
    } else {
        frame.rel.join(&entry.name)
    }
}

fn output_path(root: &Path, root_is_absolute: bool, abs: &Path) -> PathBuf {
    if root_is_absolute {
        abs.to_path_buf()
    } else if let Ok(rel) = abs.strip_prefix(root) {
        if rel.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            rel.to_path_buf()
        }
    } else {
        pathdiff(root, abs)
    }
}

/// Computes a relative path from `from` to `to` (both absolute). Falls back to
/// `to` itself when no common component exists (e.g. different drives).
fn pathdiff(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = to.components().collect();
    let mut common = 0;
    for (a, b) in from.iter().zip(to.iter()) {
        if a == b {
            common += 1;
        } else {
            break;
        }
    }
    let mut result = PathBuf::new();
    for _ in common..from.len() {
        result.push("..");
    }
    for c in &to[common..] {
        result.push(c.as_os_str());
    }
    result
}

fn fingerprint(path: &Path) -> Option<DirFingerprint> {
    let meta = fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(DirFingerprint {
            dev: meta.dev(),
            ino: meta.ino(),
            mtime_secs: meta.mtime(),
            mtime_nanos: meta.mtime_nanos() as u32,
            win_attrs: 0,
            win_creation: 0,
            win_write: 0,
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        Some(DirFingerprint {
            dev: 0,
            ino: 0,
            mtime_secs: 0,
            mtime_nanos: 0,
            win_attrs: meta.file_attributes(),
            win_creation: meta.creation_time(),
            win_write: meta.last_write_time(),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        Some(DirFingerprint::default())
    }
}

/// Loads the entries of a frame's directory.
///
/// For literal pattern components this stats the single named entry directly
/// (mirroring glob's fast path); for patterns with metacharacters it reads the
/// directory, sorts entries by name and applies the leading-dot option.
fn load_frame(
    frame: &mut Frame,
    patterns: &[Pattern],
    options: &MatchOptions,
    root_is_absolute: bool,
    root: &Path,
) -> Result<(), GlobError> {
    let pattern = &patterns[frame.pattern_idx];
    if !pattern.has_metachars {
        let s = pattern.as_str();
        let special = s == "." || s == "..";
        if special {
            let target = if s == "." {
                frame.dir.clone()
            } else {
                frame.dir.join("..")
            };
            if fs::metadata(&target).map(|m| m.is_dir()).unwrap_or(false) {
                frame.entries = vec![Entry {
                    name: OsString::from(s),
                    is_dir: true,
                }];
            }
        } else {
            let target = frame.dir.join(s);
            let exists = fs::metadata(&target).is_ok() || fs::symlink_metadata(&target).is_ok();
            if exists {
                let is_dir = fs::metadata(&target).map(|m| m.is_dir()).unwrap_or(false);
                frame.entries = vec![Entry {
                    name: OsString::from(s),
                    is_dir,
                }];
            }
        }
        frame.fingerprint = fingerprint(&frame.dir);
        frame.loaded = true;
        return Ok(());
    }

    let read = fs::read_dir(&frame.dir).and_then(|d| {
        d.map(|e| {
            e.map(|e| {
                let path = e.path();
                let is_dir = e
                    .file_type()
                    .ok()
                    .and_then(|ft| {
                        if ft.is_symlink() {
                            None
                        } else {
                            Some(ft.is_dir())
                        }
                    })
                    .or_else(|| fs::metadata(&path).map(|m| m.is_dir()).ok())
                    .unwrap_or(false);
                Entry {
                    name: e.file_name(),
                    is_dir,
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()
    });

    match read {
        Ok(mut entries) => {
            if options.require_literal_leading_dot {
                entries.retain(|e| !e.name.to_str().map(|s| s.starts_with('.')).unwrap_or(false));
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            // glob matches the special . and .. entries when the pattern
            // starts with a literal dot; they are delivered before the
            // regular entries (LIFO order of glob's todo stack).
            if !pattern.tokens.is_empty() && pattern.tokens[0] == PatternToken::Char('.') {
                entries.insert(
                    0,
                    Entry {
                        name: OsString::from("."),
                        is_dir: true,
                    },
                );
                entries.insert(
                    0,
                    Entry {
                        name: OsString::from(".."),
                        is_dir: true,
                    },
                );
            }
            frame.entries = entries;
            frame.fingerprint = fingerprint(&frame.dir);
            frame.loaded = true;
            Ok(())
        }
        Err(e) => {
            frame.fingerprint = fingerprint(&frame.dir);
            frame.loaded = true;
            Err(GlobError {
                path: frame_error_path(frame, root_is_absolute, root),
                error: e,
            })
        }
    }
}

fn frame_error_path(frame: &Frame, root_is_absolute: bool, _root: &Path) -> PathBuf {
    if root_is_absolute {
        frame.dir.clone()
    } else if frame.curdir {
        PathBuf::from(".")
    } else {
        frame.rel.clone()
    }
}

// ---------------------------------------------------------------------------
// Path encoding (raw OS bytes, so non-UTF-8 paths survive)
// ---------------------------------------------------------------------------

fn encode_os(os: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = os.as_bytes();
        let mut out = Vec::with_capacity(5 + bytes.len());
        out.push(1);
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
        out
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = os.encode_wide().collect();
        let byte_len = wide.len() * 2;
        let mut out = Vec::with_capacity(5 + byte_len);
        out.push(2);
        out.extend_from_slice(&(byte_len as u32).to_le_bytes());
        for w in wide {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out
    }
}

fn decode_os(bytes: &[u8]) -> Result<PathBuf, ResumeError> {
    if bytes.len() < 5 {
        return Err(ResumeError::InvalidCursor("truncated path".to_string()));
    }
    let tag = bytes[0];
    let len = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
    if bytes.len() != 5 + len {
        return Err(ResumeError::InvalidCursor("bad path length".to_string()));
    }
    let data = &bytes[5..];
    match tag {
        1 => {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;
                Ok(PathBuf::from(OsString::from_vec(data.to_vec())))
            }
            #[cfg(not(unix))]
            {
                let _ = data;
                Err(ResumeError::InvalidCursor(
                    "unix-encoded path on a non-unix platform".to_string(),
                ))
            }
        }
        2 => {
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStringExt;
                if len % 2 != 0 {
                    return Err(ResumeError::InvalidCursor(
                        "utf-16 path has odd length".to_string(),
                    ));
                }
                let wide: Vec<u16> = data
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                Ok(PathBuf::from(OsString::from_wide(&wide)))
            }
            #[cfg(not(windows))]
            {
                let _ = data;
                Err(ResumeError::InvalidCursor(
                    "windows-encoded path on a non-windows platform".to_string(),
                ))
            }
        }
        _ => Err(ResumeError::InvalidCursor(format!(
            "unknown path encoding tag {}",
            tag
        ))),
    }
}

// ---------------------------------------------------------------------------
// Binary cursor writer/reader
// ---------------------------------------------------------------------------

fn put_bytes(w: &mut Vec<u8>, bytes: &[u8]) {
    w.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    w.extend_from_slice(bytes);
}

fn put_fingerprint(w: &mut Vec<u8>, fp: Option<&DirFingerprint>) {
    match fp {
        None => w.push(0),
        Some(fp) => {
            w.push(1);
            w.extend_from_slice(&fp.dev.to_le_bytes());
            w.extend_from_slice(&fp.ino.to_le_bytes());
            w.extend_from_slice(&fp.mtime_secs.to_le_bytes());
            w.extend_from_slice(&fp.mtime_nanos.to_le_bytes());
            w.extend_from_slice(&fp.win_attrs.to_le_bytes());
            w.extend_from_slice(&fp.win_creation.to_le_bytes());
            w.extend_from_slice(&fp.win_write.to_le_bytes());
        }
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ResumeError> {
        if self.pos + n > self.buf.len() {
            return Err(ResumeError::InvalidCursor("unexpected end of cursor".to_string()));
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, ResumeError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ResumeError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, ResumeError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn i64(&mut self) -> Result<i64, ResumeError> {
        let b = self.take(8)?;
        Ok(i64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn bytes(&mut self) -> Result<&'a [u8], ResumeError> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn fingerprint(&mut self) -> Result<Option<DirFingerprint>, ResumeError> {
        let present = self.u8()?;
        if present == 0 {
            return Ok(None);
        }
        if present != 1 {
            return Err(ResumeError::InvalidCursor(
                "invalid fingerprint presence byte".to_string(),
            ));
        }
        Ok(Some(DirFingerprint {
            dev: self.u64()?,
            ino: self.u64()?,
            mtime_secs: self.i64()?,
            mtime_nanos: self.u32()?,
            win_attrs: self.u32()?,
            win_creation: self.u64()?,
            win_write: self.u64()?,
        }))
    }
}

// ---------------------------------------------------------------------------
// Base64 (standard alphabet, padded)
// ---------------------------------------------------------------------------

const B64_TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().saturating_add(2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64_TABLE[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64_TABLE[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64_TABLE[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn base64_decode(input: &str) -> Result<Vec<u8>, ResumeError> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let bytes = input.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err(ResumeError::InvalidCursor(
            "base64 length is not a multiple of 4".to_string(),
        ));
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().filter(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && chunk[3] != b'=') || (pad == 2 && chunk[2] != b'=') {
            return Err(ResumeError::InvalidCursor("bad base64 padding".to_string()));
        }
        let mut vals = [0u8; 4];
        for i in 0..4 {
            if chunk[i] == b'=' {
                vals[i] = 0;
            } else {
                vals[i] = val(chunk[i]).ok_or_else(|| {
                    ResumeError::InvalidCursor("invalid base64 character".to_string())
                })?;
            }
        }
        let n = ((vals[0] as u32) << 18)
            | ((vals[1] as u32) << 12)
            | ((vals[2] as u32) << 6)
            | (vals[3] as u32);
        out.push(((n >> 16) & 0xff) as u8);
        if pad < 2 {
            out.push(((n >> 8) & 0xff) as u8);
        }
        if pad < 1 {
            out.push((n & 0xff) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        for input in [
            vec![],
            vec![b'a'],
            vec![b'a', b'b'],
            vec![b'a', b'b', b'c'],
            vec![0u8, 1, 2, 255, 254, 253, 128, 64, 32],
        ] {
            let encoded = base64_encode(&input);
            assert_eq!(base64_decode(&encoded).unwrap(), input);
        }
    }

    #[test]
    fn cursor_roundtrip_empty() {
        let cursor = BatchScanner::new("*", MatchOptions::new(), 2)
            .unwrap()
            .cursor();
        let bytes = cursor.to_bytes();
        let decoded = Cursor::from_bytes(&bytes).unwrap();
        assert_eq!(cursor, decoded);
        let s = cursor.to_base64();
        let decoded = Cursor::from_base64(&s).unwrap();
        assert_eq!(cursor, decoded);
    }

    #[test]
    fn cursor_rejects_garbage() {
        assert!(Cursor::from_bytes(b"not a cursor").is_err());
        assert!(Cursor::from_base64("!!!!").is_err());
    }

    #[test]
    fn path_encoding_preserves_bytes() {
        let p = PathBuf::from("foo/bar");
        let encoded = encode_os(p.as_os_str());
        let decoded = decode_os(&encoded).unwrap();
        assert_eq!(decoded, p.as_os_str());
    }
}

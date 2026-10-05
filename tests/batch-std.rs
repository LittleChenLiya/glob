// Copyright 2026 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use glob::batch::{decode_component, encode_component, BatchScanner, CursorError};
use glob::{glob_with, MatchOptions, PatternError};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn names(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

fn make_files(root: &Path, files: &[&str]) {
    for f in files {
        let p = root.join(f);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::File::create(p).unwrap();
    }
}

/// Acceptance: five files, batch size 2; persist cursor, rebuild scanner, get
/// the remaining three with no duplication and in sorted order.
#[test]
fn five_files_batches_of_two_resume_without_duplicates() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    make_files(root, &["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]);

    // Fresh scan, first batch of two.
    let mut scanner = BatchScanner::new("*.txt", MatchOptions::new(), root, 2).unwrap();
    let first = scanner.next_batch();
    assert!(!first.is_finished());
    assert!(first.is_snapshot_consistent());
    assert_eq!(names(&first.matches), vec!["a.txt", "b.txt"]);
    let cursor = first.cursor().to_string();

    // Rebuild the scanner from the persisted cursor (as a different process
    // would). Only the remaining three come back, nothing is repeated.
    let mut scanner = BatchScanner::resume(&cursor, "*.txt", MatchOptions::new(), root).unwrap();
    let second = scanner.next_batch();
    assert_eq!(names(&second.matches), vec!["c.txt", "d.txt"]);
    assert!(!second.is_finished());
    let cursor2 = second.cursor().to_string();

    let mut scanner = BatchScanner::resume(&cursor2, "*.txt", MatchOptions::new(), root).unwrap();
    let third = scanner.next_batch();
    assert_eq!(names(&third.matches), vec!["e.txt"]);
    assert!(third.is_finished());

    // The union is exactly the five files, each delivered once.
    let mut all = Vec::new();
    all.extend(names(&first.matches));
    all.extend(names(&second.matches));
    all.extend(names(&third.matches));
    all.sort();
    assert_eq!(all, vec!["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]);

    // A finished cursor resumes to an empty batch.
    let mut scanner =
        BatchScanner::resume(third.cursor(), "*.txt", MatchOptions::new(), root).unwrap();
    let again = scanner.next_batch();
    assert!(again.matches.is_empty());
    assert!(again.is_finished());
}

/// Acceptance: changing the pattern rejects the old cursor.
#[test]
fn changing_pattern_rejects_old_cursor() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    make_files(root, &["a.log", "b.log", "c.log"]);

    let mut scanner = BatchScanner::new("*.log", MatchOptions::new(), root, 1).unwrap();
    let cursor = scanner.next_batch().cursor().to_string();

    let err = BatchScanner::resume(&cursor, "*.txt", MatchOptions::new(), root)
        .err()
        .unwrap();
    assert!(matches!(err, CursorError::PatternMismatch { .. }));
}

/// Changing match options also rejects the cursor.
#[test]
fn changing_options_rejects_old_cursor() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    make_files(root, &["A.log", "b.log"]);

    let opts = MatchOptions::new(); // case sensitive
    let mut scanner = BatchScanner::new("*.log", opts, root, 1).unwrap();
    let cursor = scanner.next_batch().cursor().to_string();

    let different = MatchOptions {
        case_sensitive: false,
        ..MatchOptions::new()
    };
    let err = BatchScanner::resume(&cursor, "*.log", different, root)
        .err()
        .unwrap();
    assert_eq!(err, CursorError::OptionsMismatch);
}

/// A cursor bound to one root cannot be resumed against another root.
#[test]
fn changing_root_rejects_old_cursor() {
    let dir1 = TempDir::new().unwrap();
    let dir2 = TempDir::new().unwrap();
    make_files(dir1.path(), &["a"]);
    make_files(dir2.path(), &["a"]);

    let mut scanner = BatchScanner::new("*", MatchOptions::new(), dir1.path(), 1).unwrap();
    let cursor = scanner.next_batch().cursor().to_string();

    let err = BatchScanner::resume(&cursor, "*", MatchOptions::new(), dir2.path())
        .err()
        .unwrap();
    assert!(matches!(err, CursorError::RootMismatch { .. }));
}

/// Unmatched entries still advance traversal: a directory full of
/// non-matching names makes progress and eventually finishes.
#[test]
fn unmatched_entries_still_advance_the_scan() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    make_files(root, &["skip1", "skip2", "keep.match", "skip3"]);

    let mut scanner = BatchScanner::new("*.match", MatchOptions::new(), root, 10).unwrap();
    let batch = scanner.next_batch();
    assert_eq!(names(&batch.matches), vec!["keep.match"]);
    assert!(batch.is_finished());
    // The cursor advanced even though most entries did not match.
    assert!(batch.cursor().contains("END"));
}

/// A newly added entry in a pending directory is explicitly flagged as a
/// change on resume and the batch is marked not snapshot-consistent.
#[test]
fn added_entry_on_resume_is_explicitly_reported() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    make_files(root, &["a", "b", "c", "d"]);

    let mut scanner = BatchScanner::new("*", MatchOptions::new(), root, 2).unwrap();
    let first = scanner.next_batch();
    assert_eq!(names(&first.matches), vec!["a", "b"]);
    assert!(first.is_snapshot_consistent());
    let cursor = first.cursor().to_string();

    // Add two files and remove one while the scan is paused.
    fs::File::create(root.join("e")).unwrap();
    fs::File::create(root.join("f")).unwrap();
    fs::remove_file(root.join("d")).unwrap();

    let mut scanner = BatchScanner::resume(&cursor, "*", MatchOptions::new(), root).unwrap();
    let second = scanner.next_batch();
    assert!(
        !second.is_snapshot_consistent(),
        "change must invalidate snapshot consistency"
    );
    assert_eq!(second.changes.len(), 1);
    let change = &second.changes[0];
    assert!(change.directory.as_os_str().is_empty(), "root directory");
    let added: BTreeSet<_> = change.added.iter().cloned().collect();
    assert_eq!(
        added,
        ["e", "f"]
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(change.removed, vec!["d".to_string()]);
    assert_ne!(change.previous_fingerprint, change.current_fingerprint);

    // The traversal still continues over the live listing: c, then the new
    // entries e/f (d is gone), and no earlier path is re-delivered.
    let mut got = names(&second.matches);
    let cursor2 = second.cursor().to_string();
    let mut scanner = BatchScanner::resume(&cursor2, "*", MatchOptions::new(), root).unwrap();
    let third = scanner.next_batch();
    got.extend(names(&third.matches));
    assert_eq!(got, vec!["c", "e", "f"]);
    assert!(third.is_finished());
}

/// A subsequent resume with no further changes returns to consistency, and
/// never reports duplicate change events.
#[test]
fn unchanged_resume_is_consistent_and_has_no_changes() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    make_files(root, &["a", "b", "c"]);

    let mut scanner = BatchScanner::new("*", MatchOptions::new(), root, 1).unwrap();
    let cursor = scanner.next_batch().cursor().to_string();

    let mut scanner = BatchScanner::resume(&cursor, "*", MatchOptions::new(), root).unwrap();
    let batch = scanner.next_batch();
    assert!(batch.is_snapshot_consistent());
    assert!(batch.changes.is_empty());
    assert_eq!(names(&batch.matches), vec!["b"]);
}

/// Entries whose names need percent-encoding round-trip through both the
/// cursor and the change reports.
#[test]
fn encoded_entry_names_roundtrip_and_match() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // A space, a percent sign and a slash-like character cannot break the
    // line-oriented cursor format.
    let tricky = ["my file.txt", "100%.txt", "a-b_c~d.txt"];
    for name in tricky {
        fs::File::create(root.join(name)).unwrap();
    }

    // Raw encoding helpers.
    let os = OsString::from("100%.txt");
    let enc = encode_component(&os);
    assert_eq!(enc, "100%25.txt");
    assert_eq!(decode_component(&enc).unwrap(), os);

    let mut scanner = BatchScanner::new("*.txt", MatchOptions::new(), root, 1).unwrap();
    let first = scanner.next_batch();
    let mut seen = names(&first.matches);
    let mut cursor = first.cursor().to_string();
    loop {
        let mut s = BatchScanner::resume(&cursor, "*.txt", MatchOptions::new(), root).unwrap();
        let batch = s.next_batch();
        seen.extend(names(&batch.matches));
        cursor = batch.cursor().to_string();
        if batch.is_finished() {
            break;
        }
    }
    seen.sort();
    let mut want = tricky.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    want.sort();
    assert_eq!(seen, want);
}

/// Cursor text rejects garbled / foreign payloads.
#[test]
fn corrupt_cursor_is_rejected() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::File::create(root.join("a")).unwrap();

    for bad in ["", "GBC9\n", "GBC1\n", "not a cursor at all\n"] {
        let err = BatchScanner::resume(bad, "*", MatchOptions::new(), root);
        assert!(matches!(err, Err(CursorError::Corrupt(_))), "for {bad:?}");
    }
}

/// Recursive scan paged with resumes equals a single uninterrupted scan.
#[test]
fn recursive_paged_scan_matches_uninterrupted_scan() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let files = [
        "a.txt",
        "sub/b.txt",
        "sub/deep/c.txt",
        "other/d.txt",
        "zzz.txt",
    ];
    make_files(root, &files);

    let collect = |batch_size: usize, resume: bool| -> Vec<String> {
        if !resume {
            let mut scanner =
                BatchScanner::new("**/*.txt", MatchOptions::new(), root, batch_size).unwrap();
            let mut out = Vec::new();
            loop {
                let batch = scanner.next_batch();
                out.extend(batch.matches.iter().map(|p| {
                    p.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/")
                }));
                if batch.is_finished() {
                    break;
                }
            }
            out
        } else {
            let mut cursor = BatchScanner::new("**/*.txt", MatchOptions::new(), root, batch_size)
                .unwrap()
                .cursor();
            let mut out = Vec::new();
            loop {
                let mut scanner =
                    BatchScanner::resume(&cursor, "**/*.txt", MatchOptions::new(), root).unwrap();
                let batch = scanner.next_batch();
                out.extend(batch.matches.iter().map(|p| {
                    p.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/")
                }));
                cursor = batch.cursor().to_string();
                if batch.is_finished() {
                    break;
                }
            }
            out
        }
    };

    let uninterrupted = collect(100, false);
    let paged = collect(2, true);
    assert_eq!(uninterrupted, paged);
    // Deterministic sorted pre-order.
    let mut sorted = uninterrupted.clone();
    sorted.sort();
    assert_eq!(uninterrupted, sorted);
}

/// A bare trailing `**` yields directories (matching the glob crate) and the
/// root itself is not returned.
#[test]
fn trailing_recursive_yields_directories_not_root() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    make_files(root, &["dir/f.txt", "dir/sub/g.txt", "top.txt"]);

    let mut scanner = BatchScanner::new("**", MatchOptions::new(), root, 10).unwrap();
    let batch = scanner.next_batch();
    let mut dirs = batch
        .matches
        .iter()
        .map(|p| {
            p.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect::<Vec<_>>();
    dirs.sort();
    assert_eq!(dirs, vec!["dir".to_string(), "dir/sub".to_string()]);
}

/// Batch size zero is rejected at construction.
#[test]
fn zero_batch_size_rejected() {
    let dir = TempDir::new().unwrap();
    let err = BatchScanner::new("*", MatchOptions::new(), dir.path(), 0);
    assert!(err.is_err());
}

/// require_literal_leading_dot option bound into the cursor hides dotfiles
/// and survives a resume.
#[test]
fn leading_dot_option_is_bound_and_effective() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    make_files(root, &["visible", ".hidden"]);
    let opts = MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: true,
    };
    let mut scanner = BatchScanner::new("*", opts, root, 10).unwrap();
    let batch = scanner.next_batch();
    assert_eq!(names(&batch.matches), vec!["visible"]);

    // Default options cannot resume this cursor.
    let err = BatchScanner::resume(batch.cursor(), "*", MatchOptions::new(), root);
    assert_eq!(err.err(), Some(CursorError::OptionsMismatch));
}

/// Deleted pending entries are reported by encoded name and skipped.
#[test]
fn deleted_pending_entries_are_reported_and_skipped() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    make_files(root, &["a", "b", "gone1", "gone2"]);

    let mut scanner = BatchScanner::new("*", MatchOptions::new(), root, 2).unwrap();
    let first = scanner.next_batch();
    assert_eq!(names(&first.matches), vec!["a", "b"]);
    let cursor = first.cursor().to_string();

    fs::remove_file(root.join("gone1")).unwrap();
    fs::remove_file(root.join("gone2")).unwrap();

    let mut scanner = BatchScanner::resume(&cursor, "*", MatchOptions::new(), root).unwrap();
    let second = scanner.next_batch();
    assert!(!second.is_snapshot_consistent());
    assert_eq!(second.changes.len(), 1);
    let removed: BTreeSet<_> = second.changes[0].removed.iter().cloned().collect();
    assert_eq!(
        removed,
        ["gone1", "gone2"]
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>()
    );
    assert!(second.changes[0].added.is_empty());
    assert!(second.matches.is_empty(), "nothing left to deliver");
    assert!(second.is_finished());
}

/// The final batch is explicitly marked finished, and batches that only
/// consume non-matching entries still advance to the end.
#[test]
fn batch_end_is_explicitly_marked() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    make_files(root, &["x.nomatch", "y.nomatch"]);

    let mut scanner = BatchScanner::new("*.match", MatchOptions::new(), root, 5).unwrap();
    let batch = scanner.next_batch();
    assert!(batch.matches.is_empty());
    assert!(batch.is_finished(), "empty terminal batch is finished");
    assert!(batch.is_snapshot_consistent());

    // Repeated calls stay finished and stay empty.
    let again = BatchScanner::resume(batch.cursor(), "*.match", MatchOptions::new(), root)
        .unwrap()
        .next_batch();
    assert!(again.is_finished());
    assert!(again.matches.is_empty());
}

/// The batched scanner returns the same set, in the same order, as the
/// crate's regular `glob_with`, for several pattern shapes.
#[test]
fn batch_results_match_glob_with_ordering() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    make_files(
        root,
        &[
            "a.txt",
            "b.log",
            "sub/c.txt",
            "sub/d.log",
            "sub/deep/e.txt",
            "zzz.txt",
            "other/f.txt",
        ],
    );

    for pattern in ["*.txt", "**/*.txt", "**", "sub/**", "**/*.log"] {
        let mut want: Vec<PathBuf> =
            glob_with(&root.join(pattern).to_string_lossy(), MatchOptions::new())
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
        want.sort();

        let mut got = Vec::new();
        let mut scanner = BatchScanner::new(pattern, MatchOptions::new(), root, 2).unwrap();
        loop {
            let batch = scanner.next_batch();
            let finished = batch.is_finished();
            got.extend(batch.matches);
            if finished {
                break;
            }
        }
        got.sort();
        assert_eq!(got, want, "mismatch for pattern {pattern:?}");
    }
}

/// Invalid batch patterns surface a pattern error rather than panicking.
#[test]
fn invalid_batch_pattern_is_a_pattern_error() {
    let dir = TempDir::new().unwrap();
    let err = BatchScanner::new("a/**b", MatchOptions::new(), dir.path(), 2);
    match err {
        Err(e) => {
            let _: &PatternError = match &e {
                glob::batch::ScanError::Pattern(p) => p,
                glob::batch::ScanError::Io(_) => panic!("expected pattern error"),
            };
        }
        Ok(_) => panic!("expected error"),
    }
}

/// A resume after every single batch returns the complete unique set even
/// when the tree contains directories that do not match mid-way.
#[test]
fn resume_after_every_path_is_complete_and_unique() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    make_files(
        root,
        &["1.txt", "2.txt", "d/3.txt", "d/sub/4.txt", "e/5.txt"],
    );

    let mut all = BTreeSet::new();
    let mut scanner = BatchScanner::new("**/*.txt", MatchOptions::new(), root, 1).unwrap();
    let mut batch = scanner.next_batch();
    for p in &batch.matches {
        assert!(all.insert(p.to_path_buf()));
    }
    while !batch.is_finished() {
        scanner =
            BatchScanner::resume(batch.cursor(), "**/*.txt", MatchOptions::new(), root).unwrap();
        batch = scanner.next_batch();
        for p in &batch.matches {
            assert!(all.insert(p.to_path_buf()), "duplicate {p:?}");
        }
    }
    assert_eq!(all.len(), 5);
}

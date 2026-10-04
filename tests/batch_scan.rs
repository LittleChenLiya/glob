// Integration tests for the resumable batched scanner.

use glob::batch::{BatchScanner, Cursor, FsChange, ResumeError};
use glob::MatchOptions;
use std::fs;
use std::path::PathBuf;

fn file_names(batch: &glob::batch::Batch) -> Vec<String> {
    batch
        .items
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
        .collect()
}

fn make_files(dir: &std::path::Path, names: &[&str]) {
    for name in names {
        fs::File::create(dir.join(name)).unwrap();
    }
}

#[test]
fn batched_resume_yields_no_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    make_files(dir.path(), &["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]);
    let pattern = dir.path().join("*.txt").to_str().unwrap().to_string();
    let options = MatchOptions::new();

    let mut scanner = BatchScanner::new(&pattern, options, 2).unwrap();
    let b1 = scanner.next_batch();
    assert_eq!(file_names(&b1), vec!["a.txt", "b.txt"]);
    assert!(!b1.exhausted);

    // Persist the cursor, then rebuild the scanner from it.
    let saved = b1.cursor.to_base64();
    let cursor = Cursor::from_base64(&saved).unwrap();
    let mut scanner = BatchScanner::resume(&pattern, options, cursor, 2).unwrap();

    let b2 = scanner.next_batch();
    assert_eq!(file_names(&b2), vec!["c.txt", "d.txt"]);
    assert!(!b2.exhausted);

    let b3 = scanner.next_batch();
    assert_eq!(file_names(&b3), vec!["e.txt"]);
    assert!(b3.exhausted);

    // Collect everything and assert there are no duplicates.
    let mut scanner = BatchScanner::new(&pattern, options, 2).unwrap();
    let mut all = Vec::new();
    loop {
        let b = scanner.next_batch();
        all.extend(file_names(&b));
        if b.exhausted {
            break;
        }
    }
    assert_eq!(all, vec!["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]);
}

#[test]
fn unmatched_entries_still_advance() {
    let dir = tempfile::tempdir().unwrap();
    make_files(dir.path(), &["a.txt", "b.log", "c.txt", "d.log", "e.txt"]);
    let pattern = dir.path().join("*.txt").to_str().unwrap().to_string();
    let mut scanner = BatchScanner::new(&pattern, MatchOptions::new(), 2).unwrap();
    let b1 = scanner.next_batch();
    assert_eq!(file_names(&b1), vec!["a.txt", "c.txt"]);
    let b2 = scanner.next_batch();
    assert_eq!(file_names(&b2), vec!["e.txt"]);
    assert!(b2.exhausted);
}

#[test]
fn pattern_mismatch_rejects_old_cursor() {
    let dir = tempfile::tempdir().unwrap();
    make_files(dir.path(), &["a.txt", "b.txt"]);
    let pattern = dir.path().join("*.txt").to_str().unwrap().to_string();
    let mut scanner = BatchScanner::new(&pattern, MatchOptions::new(), 2).unwrap();
    let b1 = scanner.next_batch();
    let cursor = b1.cursor;

    let other = dir.path().join("*.log").to_str().unwrap().to_string();
    let err = BatchScanner::resume(&other, MatchOptions::new(), cursor, 2).unwrap_err();
    assert!(
        matches!(err, ResumeError::PatternMismatch { .. }),
        "expected PatternMismatch, got {:?}",
        err
    );
}

#[test]
fn options_mismatch_rejects_old_cursor() {
    let dir = tempfile::tempdir().unwrap();
    make_files(dir.path(), &["a.txt", "b.txt"]);
    let pattern = dir.path().join("*.txt").to_str().unwrap().to_string();
    let mut scanner = BatchScanner::new(&pattern, MatchOptions::new(), 2).unwrap();
    let b1 = scanner.next_batch();
    let cursor = b1.cursor;

    let other = MatchOptions {
        case_sensitive: false,
        ..MatchOptions::new()
    };
    let err = BatchScanner::resume(&pattern, other, cursor, 2).unwrap_err();
    assert!(
        matches!(err, ResumeError::OptionsMismatch { .. }),
        "expected OptionsMismatch, got {:?}",
        err
    );
}

#[test]
fn root_mismatch_rejects_old_cursor() {
    // A relative pattern binds the root to the current working directory, so
    // removing that directory invalidates the cursor. (An absolute pattern
    // binds to the drive root, which survives.)
    let dir = tempfile::tempdir().unwrap();
    make_files(dir.path(), &["a.txt", "b.txt"]);
    let cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    let result = std::panic::catch_unwind(|| {
        let mut scanner = BatchScanner::new("*.txt", MatchOptions::new(), 2).unwrap();
        let b1 = scanner.next_batch();
        let cursor = b1.cursor;
        (scanner, cursor)
    });
    std::env::set_current_dir(cwd).unwrap();
    let (scanner, cursor) = result.unwrap();

    // Remove the root directory, then try to resume.
    drop(scanner);
    drop(dir);

    let err = BatchScanner::resume("*.txt", MatchOptions::new(), cursor, 2).unwrap_err();
    assert!(
        matches!(err, ResumeError::RootMismatch { .. }),
        "expected RootMismatch, got {:?}",
        err
    );
}

#[test]
fn dir_change_and_deleted_delivered_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    make_files(dir.path(), &["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]);
    let pattern = dir.path().join("*.txt").to_str().unwrap().to_string();
    let mut scanner = BatchScanner::new(&pattern, MatchOptions::new(), 2).unwrap();
    let b1 = scanner.next_batch();
    assert_eq!(file_names(&b1), vec!["a.txt", "b.txt"]);
    let cursor = b1.cursor;

    // Delete a delivered file and add a new one.
    fs::remove_file(dir.path().join("a.txt")).unwrap();
    fs::File::create(dir.path().join("f.txt")).unwrap();

    let mut scanner = BatchScanner::resume(&pattern, MatchOptions::new(), cursor, 2).unwrap();
    let b2 = scanner.next_batch();

    // The directory fingerprint changed.
    assert!(
        b2.changes
            .iter()
            .any(|c| matches!(c, FsChange::DirChanged { .. })),
        "expected DirChanged, got {:?}",
        b2.changes
    );
    // The delivered file that was deleted is reported.
    assert!(
        b2.changes.iter().any(|c| matches!(
            c,
            FsChange::Deleted { path } if path.file_name().unwrap() == "a.txt"
        )),
        "expected Deleted(a.txt), got {:?}",
        b2.changes
    );

    // The remaining undelivered files are still found (f sorts after e).
    assert_eq!(file_names(&b2), vec!["c.txt", "d.txt"]);
    let b3 = scanner.next_batch();
    assert_eq!(file_names(&b3), vec!["e.txt", "f.txt"]);
    assert!(b3.exhausted);
}

#[test]
fn cursor_roundtrip_bytes_and_base64() {
    let dir = tempfile::tempdir().unwrap();
    make_files(dir.path(), &["a.txt", "b.txt", "c.txt"]);
    let pattern = dir.path().join("*.txt").to_str().unwrap().to_string();
    let mut scanner = BatchScanner::new(&pattern, MatchOptions::new(), 2).unwrap();
    let _ = scanner.next_batch();
    let cursor = scanner.cursor();

    let bytes = cursor.to_bytes();
    let decoded = Cursor::from_bytes(&bytes).unwrap();
    assert_eq!(cursor, decoded);

    let s = cursor.to_base64();
    let decoded = Cursor::from_base64(&s).unwrap();
    assert_eq!(cursor, decoded);

    // An exhausted cursor round-trips and resumes to nothing.
    let mut scanner = BatchScanner::new(&pattern, MatchOptions::new(), 2).unwrap();
    let mut last = scanner.next_batch();
    while !last.exhausted {
        last = scanner.next_batch();
    }
    assert!(last.cursor.is_exhausted());
    let decoded = Cursor::from_base64(&last.cursor.to_base64()).unwrap();
    let mut resumed = BatchScanner::resume(&pattern, MatchOptions::new(), decoded, 2).unwrap();
    let b = resumed.next_batch();
    assert!(b.items.is_empty());
    assert!(b.exhausted);
}

#[test]
fn batch_end_marker_advances() {
    let dir = tempfile::tempdir().unwrap();
    make_files(dir.path(), &["a.txt", "b.txt", "c.txt", "d.txt"]);
    let pattern = dir.path().join("*.txt").to_str().unwrap().to_string();
    let mut scanner = BatchScanner::new(&pattern, MatchOptions::new(), 3).unwrap();

    let b1 = scanner.next_batch();
    assert_eq!(file_names(&b1), vec!["a.txt", "b.txt", "c.txt"]);
    assert!(!b1.exhausted);
    assert!(!b1.cursor.is_exhausted());

    let b2 = scanner.next_batch();
    assert_eq!(file_names(&b2), vec!["d.txt"]);
    assert!(b2.exhausted);
    assert!(b2.cursor.is_exhausted());
}

#[test]
fn recursive_pattern_batches_and_resumes() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    make_files(dir.path(), &["a.md", "b.md", "c.md"]);
    make_files(&dir.path().join("sub"), &["d.md", "e.md"]);
    let pattern = dir.path().join("**/*.md").to_str().unwrap().to_string();
    let options = MatchOptions::new();

    let mut scanner = BatchScanner::new(&pattern, options, 2).unwrap();
    let mut all = Vec::new();
    loop {
        let b = scanner.next_batch();
        all.extend(file_names(&b));
        if b.exhausted {
            break;
        }
    }
    assert_eq!(all, vec!["a.md", "b.md", "c.md", "d.md", "e.md"]);

    // Resume from a mid-scan cursor and confirm no duplicates.
    let mut scanner = BatchScanner::new(&pattern, options, 2).unwrap();
    let b1 = scanner.next_batch();
    assert_eq!(file_names(&b1), vec!["a.md", "b.md"]);
    let saved = b1.cursor.to_base64();
    let mut scanner =
        BatchScanner::resume(&pattern, options, Cursor::from_base64(&saved).unwrap(), 2).unwrap();
    let mut rest = Vec::new();
    loop {
        let b = scanner.next_batch();
        rest.extend(file_names(&b));
        if b.exhausted {
            break;
        }
    }
    assert_eq!(rest, vec!["c.md", "d.md", "e.md"]);
}

#[cfg(unix)]
#[test]
fn non_utf8_path_is_encoded_and_not_redelivered() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join(OsStr::from_bytes(b"sub\xffdir"));
    fs::create_dir(&sub).unwrap();
    let pattern = dir.path().join("**").to_str().unwrap().to_string();
    let options = MatchOptions::new();

    let mut scanner = BatchScanner::new(&pattern, options, 1).unwrap();
    let b1 = scanner.next_batch();
    assert_eq!(b1.items.len(), 1);
    let delivered = b1.items[0].as_ref().unwrap();
    assert_eq!(delivered, &sub);

    // The cursor round-trips through base64 with the non-UTF-8 path intact.
    let saved = b1.cursor.to_base64();
    let decoded = Cursor::from_base64(&saved).unwrap();
    assert_eq!(b1.cursor, decoded);

    // Resuming does not redeliver the directory.
    let mut scanner = BatchScanner::resume(&pattern, options, decoded, 1).unwrap();
    let b2 = scanner.next_batch();
    assert!(b2.items.is_empty());
    assert!(b2.exhausted);
}

#[test]
fn empty_pattern_yields_nothing() {
    // An empty pattern matches nothing, like glob("").
    let mut scanner = BatchScanner::new("", MatchOptions::new(), 2).unwrap();
    let b = scanner.next_batch();
    assert!(b.items.is_empty());
    assert!(b.exhausted);
}

#[test]
fn relative_pattern_emits_relative_paths() {
    // Relative patterns produce output paths relative to the current root,
    // mirroring glob's behaviour.
    let dir = tempfile::tempdir().unwrap();
    make_files(dir.path(), &["a.txt", "b.txt"]);
    let cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    let result = std::panic::catch_unwind(|| {
        let mut scanner = BatchScanner::new("*.txt", MatchOptions::new(), 2).unwrap();
        let b = scanner.next_batch();
        let names: Vec<PathBuf> = b
            .items
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .cloned()
            .collect();
        assert_eq!(names, vec![PathBuf::from("a.txt"), PathBuf::from("b.txt")]);
    });
    std::env::set_current_dir(cwd).unwrap();
    result.unwrap();
}

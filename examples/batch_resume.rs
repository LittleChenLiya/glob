//! Demonstrates persisting a batch scan cursor and resuming it later.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example batch_resume
//! ```
//!
//! The example creates a temporary directory with a handful of files, scans
//! it in batches of two, persists the cursor after the first batch, and then
//! rebuilds the scanner from the persisted cursor to collect the remaining
//! paths. It also shows that changing the pattern rejects the old cursor.

use glob::batch::{BatchScanner, Cursor, ResumeError};
use glob::MatchOptions;
use std::fs;

fn main() {
    // Set up a small fixture directory.
    let dir = tempfile::tempdir().expect("create temp dir");
    for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"] {
        fs::File::create(dir.path().join(name)).expect("create file");
    }

    let pattern = dir
        .path()
        .join("*.txt")
        .to_str()
        .expect("utf-8 pattern")
        .to_string();
    let options = MatchOptions::new();

    // --- First scan: deliver the first batch, then persist the cursor. ---
    let mut scanner = BatchScanner::new(&pattern, options, 2).expect("start scan");
    let first = scanner.next_batch();
    println!("first batch: {:?}", paths(&first.items));
    assert_eq!(paths(&first.items), vec!["a.txt", "b.txt"]);

    // The cursor is a self-contained value; encode it as base64 for storage.
    let saved = first.cursor.to_base64();
    println!("persisted cursor ({} bytes base64)", saved.len());

    // The scanner can be dropped here; the cursor is all we need to resume.
    drop(scanner);

    // --- Resume: rebuild the scanner from the persisted cursor. ---
    let cursor = Cursor::from_base64(&saved).expect("decode cursor");
    let mut scanner = BatchScanner::resume(&pattern, options, cursor, 2).expect("resume scan");

    let mut remaining = Vec::new();
    loop {
        let batch = scanner.next_batch();
        remaining.extend(paths(&batch.items));
        if batch.exhausted {
            break;
        }
    }
    println!("remaining after resume: {:?}", remaining);
    assert_eq!(remaining, vec!["c.txt", "d.txt", "e.txt"]);

    // --- Consistency check: a changed pattern rejects the old cursor. ---
    let cursor = Cursor::from_base64(&saved).expect("decode cursor");
    let other = dir
        .path()
        .join("*.log")
        .to_str()
        .expect("utf-8 pattern")
        .to_string();
    match BatchScanner::resume(&other, options, cursor, 2) {
        Err(ResumeError::PatternMismatch { expected, found }) => {
            println!(
                "rejected resume: cursor was for {:?}, got {:?}",
                expected, found
            );
        }
        other => panic!("expected PatternMismatch, got {:?}", other.err()),
    }

    println!("ok: resume delivered exactly the remaining paths, no duplicates");
}

fn paths(items: &[glob::GlobResult]) -> Vec<String> {
    items
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
        .collect()
}

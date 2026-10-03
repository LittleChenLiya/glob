//! Short Cargo demonstration of the in-memory multi-pattern
//! [`glob::PatternFilter`]. Nothing here touches the filesystem.
//!
//! Run with: `cargo run --example filter_paths`

use std::convert::Infallible;

use glob::PatternFilter;

fn main() {
    // Multiple declared patterns; sources are reported in this order.
    let filter = PatternFilter::new(["src/*", "**/*.rs", "*.md"]).unwrap();

    // A batch of path strings gathered from anywhere (memory, logs, ...).
    let inputs = [
        "src/a.rs",
        "src/a.rs", // duplicate input: reported only once
        "README.md",
        "src/nested/b.rs",
        "notes.txt",
    ];

    println!("== default filter (case and separators significant) ==");
    for hit in filter.filter(inputs) {
        // `src/a.rs` matches both `src/*` and `**/*.rs`, so it lists both.
        println!("{} <- {:?}", hit.path(), hit.source_patterns(&filter));
    }

    println!("== A.rs vs a.rs stay separate by default ==");
    for hit in filter.filter(["A.rs", "a.rs"]) {
        println!("{} <- {:?}", hit.path(), hit.source_patterns(&filter));
    }

    println!("== case-folding normalizer merges distinct originals ==");
    // "FOObar" matches only "FOO*"; "foobar" matches only "*bar". Folding
    // them to one identity unions both sources on a single result.
    let merged = PatternFilter::new(["FOO*", "*bar"]).unwrap();
    let hits = merged
        .filter_normalized(["FOObar", "foobar"], |s| {
            Ok::<_, Infallible>(s.to_ascii_lowercase())
        })
        .unwrap();
    for hit in &hits {
        println!("{} <- {:?}", hit.path(), hit.source_patterns(&merged));
    }
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source_patterns(&merged), vec!["FOO*", "*bar"]);

    println!("== a normalizer failure rejects the whole batch ==");
    let outcome = filter.filter_normalized(["ok.rs", "no.rs"], |s| {
        if s.contains("no") {
            Err("refusing this path")
        } else {
            Ok(s.to_owned())
        }
    });
    match outcome {
        Ok(_) => println!("unexpectedly got results"),
        Err(error) => println!("batch rejected: {error}"),
    }
}


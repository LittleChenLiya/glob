//! Demo: print a pattern's feature report and compare it with real matching.
//!
//! The feature report is a read-only, purely syntactic description of a
//! compiled pattern: it lists recursive `**` wildcards, character classes,
//! and the longest literal directory prefix. It does **not** traverse the
//! filesystem and makes no claim about matching performance.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example features
//! ```
//!
//! An optional pattern can be passed as the first argument:
//!
//! ```sh
//! cargo run --example features -- 'src/**/test[0-9].rs'
//! ```

use glob::{glob, MatchOptions, Pattern};
use std::fs;
use std::path::PathBuf;

fn main() {
    let pattern = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "src/**/test[0-9].rs".to_string());

    println!("Pattern: {pattern}");
    println!("----------------------------------------");

    let compiled = match Pattern::new(&pattern) {
        Ok(p) => p,
        Err(e) => {
            println!("Invalid pattern: {e}");
            std::process::exit(1);
        }
    };

    // Print the read-only feature report.
    let features = compiled.features();
    println!("Feature report (default options):");
    match features.literal_prefix() {
        Some(prefix) => {
            println!(
                "  literal prefix: {:?} (bytes {}..{})",
                prefix.prefix, prefix.span.start, prefix.span.end
            );
            println!("  exact guarantee: {}", prefix.is_exact);
        }
        None => println!("  literal prefix: <none>"),
    }

    println!("  recursive features: {}", features.recursive().count());
    for f in features.recursive() {
        println!(
            "    - {:?} at bytes {}..{}",
            f.kind, f.span.start, f.span.end
        );
    }

    println!(
        "  character classes: {}",
        features.character_classes().count()
    );
    for f in features.character_classes() {
        println!(
            "    - {:?} at bytes {}..{}",
            f.kind, f.span.start, f.span.end
        );
    }

    // Show how case-insensitive options weaken the prefix guarantee.
    let insensitive = compiled.features_with(MatchOptions {
        case_sensitive: false,
        ..MatchOptions::new()
    });
    if let Some(prefix) = insensitive.literal_prefix() {
        println!(
            "  exact guarantee (case-insensitive): {}",
            prefix.is_exact
        );
    }

    println!("----------------------------------------");

    // Compare with real matching against a small, fixed fixture tree.
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path();
    let files = [
        "src/test1.rs",
        "src/sub/test2.rs",
        "src/test3.txt",
        "src/other/test4.rs",
        "src/TEST5.rs",
        "README.md",
    ];
    for f in files {
        let path = root.join(f);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::File::create(path).unwrap();
    }

    let full: PathBuf = root.join(&pattern);
    let matched: Vec<PathBuf> = glob(full.to_str().unwrap())
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    println!("Glob matches under {}:", root.display());
    if matched.is_empty() {
        println!("  <none>");
    } else {
        for m in &matched {
            println!("  {}", m.strip_prefix(root).unwrap().display());
        }
    }

    // Also show Pattern::matches on sample paths.
    println!("----------------------------------------");
    println!("Pattern::matches on sample paths:");
    for sample in [
        "src/test1.rs",
        "src/sub/test2.rs",
        "src/test3.txt",
        "src/TEST5.rs",
        "README.md",
    ] {
        println!("  {sample}: {}", compiled.matches(sample));
    }
}

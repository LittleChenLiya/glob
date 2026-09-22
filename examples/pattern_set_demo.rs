//! Demo for `PatternSet`: builds a temporary tree, prints it, applies an
//! include/exclude/re-include rule set, and shows both the selected files
//! (with the winning and overridden rule indices) and a compile error.
//!
//! Run with: `cargo run --example pattern_set_demo`

use glob::{PatternSet, PatternSetOptions, RuleKind};
use std::fs;
use std::path::Path;

fn print_tree(dir: &Path, prefix: &str) {
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(|e| e.file_name());
    for (i, entry) in entries.iter().enumerate() {
        let last = i + 1 == entries.len();
        let branch = if last { "+-- " } else { "|-- " };
        println!(
            "{}{}{}",
            prefix,
            branch,
            entry.file_name().to_string_lossy()
        );
        if entry.file_type().unwrap().is_dir() {
            let ext = if last { "    " } else { "|   " };
            print_tree(&entry.path(), &format!("{}{}", prefix, ext));
        }
    }
}

fn main() {
    let root = std::env::temp_dir().join(format!("glob-pattern-set-demo-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src/generated")).unwrap();
    fs::write(root.join("src/a.rs"), "a").unwrap();
    fs::write(root.join("src/generated/x.rs"), "x").unwrap();
    fs::write(root.join("src/generated/keep.rs"), "keep").unwrap();

    println!("File tree ({}):", root.display());
    print_tree(&root, "");

    let rules = [
        (RuleKind::Include, "src/**/*.rs"),
        (RuleKind::Exclude, "src/generated/**"),
        (RuleKind::Include, "src/generated/keep.rs"),
    ];
    println!();
    println!("Rules (last match wins):");
    for (i, (kind, pattern)) in rules.iter().enumerate() {
        println!("  [{}] {:?} {}", i, kind, pattern);
    }

    let set = PatternSet::compile(&rules, PatternSetOptions::default()).unwrap();
    let outcome = set.walk(&root).unwrap();
    println!();
    println!("Selected files:");
    for m in &outcome.matches {
        println!(
            "  {} (winner: rule {}, overridden: {:?})",
            m.relative.join("/"),
            m.winner,
            m.overridden
        );
    }

    let ex = set.explain(&["src", "generated", "keep.rs"]);
    println!();
    println!(
        "Explain src/generated/keep.rs: matched rules {:?}, winner {:?}, included {}",
        ex.matched, ex.winner, ex.included
    );

    println!();
    println!("Compile error demo:");
    match PatternSet::compile(
        &[
            (RuleKind::Include, "src/**/*.rs"),
            (RuleKind::Exclude, "src/***bad"),
        ],
        PatternSetOptions::default(),
    ) {
        Ok(_) => println!("  unexpected success"),
        Err(e) => println!("  {}", e),
    }

    let _ = fs::remove_dir_all(&root);
}

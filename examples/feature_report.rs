//! Prints the syntactic feature report of a glob pattern and contrasts it
//! with ordinary `Pattern` matching.
//!
//! Run with: `cargo run --example feature_report`

use glob::{MatchOptions, Pattern};

fn show(pattern: &str, candidates: &[&str]) {
    println!("pattern: {:?}", pattern);

    let pattern = match Pattern::new(pattern) {
        Ok(p) => p,
        Err(e) => {
            println!("  invalid pattern: {}", e);
            println!();
            return;
        }
    };

    // The read-only feature report, extracted from the parsed tokens.
    // This performs no directory traversal.
    let report = pattern.features();
    for line in report.to_string().lines() {
        println!("  {}", line);
    }

    // The match options decide whether the directory prefix is guaranteed
    // to appear verbatim in matched paths.
    let options = MatchOptions {
        case_sensitive: false,
        ..MatchOptions::new()
    };
    println!(
        "  prefix guarantee (case-sensitive): {:?}",
        report.prefix_guarantee(MatchOptions::new())
    );
    println!(
        "  prefix guarantee (case-insensitive): {:?}",
        report.prefix_guarantee(options)
    );

    // Ordinary matching, for comparison.
    println!("  matching:");
    for candidate in candidates {
        println!(
            "    {:?} => sensitive: {}, insensitive: {}",
            candidate,
            pattern.matches(candidate),
            pattern.matches_with(candidate, options),
        );
    }
    println!();
}

fn main() {
    show(
        "src/**/test[0-9].rs",
        &[
            "src/test0.rs",
            "src/deep/nested/test9.rs",
            "src/testX.rs",
            "SRC/test1.rs",
        ],
    );
    show("a[*]b", &["a*b", "axb"]);
    show("数据/**/测试[0-9].rs", &["数据/测试3.rs", "数据/x/测试7.rs"]);
    show("abc[def", &[]);
}

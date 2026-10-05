// Copyright 2026 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Read-only glob diff checker example.
//!
//! This example applies two explicit [`MatchOptions`] configurations to a
//! finite set of candidate paths and reports each witness: a path for which
//! one side matches and the other does not. It performs **no** filesystem
//! scanning; matching is purely in-memory.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example diff_checker
//! ```
//!
//! The diff is **finite**: it only inspects the supplied candidate paths.
//! A witness-free result does not prove that the two patterns are
//! equivalent for all possible paths.

use glob::diff::{diff, Difference};
use glob::MatchOptions;

fn describe(d: &Difference) -> &'static str {
    match d {
        Difference::CaseSensitive => "case-sensitive",
        Difference::LiteralSeparator => "literal-separator",
        Difference::LiteralLeadingDot => "literal-leading-dot",
        Difference::Multiple(_) => "multiple",
        Difference::NotAttributable => "not-attributable",
    }
}

fn run(
    label: &str,
    left_pattern: &str,
    left_options: MatchOptions,
    right_pattern: &str,
    right_options: MatchOptions,
    candidates: &[&str],
) {
    println!("== {}", label);
    println!("  left:  pattern={:?} options={:?}", left_pattern, left_options);
    println!("  right: pattern={:?} options={:?}", right_pattern, right_options);
    println!("  candidates: {:?}", candidates);

    let result = diff(left_pattern, left_options, right_pattern, right_options, candidates)
        .expect("both patterns are valid");

    println!("  candidate_count: {}", result.candidate_count);
    println!("  finite: {} (only the supplied candidates were inspected)", result.finite);
    println!("  witnesses: {}", result.witnesses.len());
    for w in &result.witnesses {
        println!(
            "    path={:?} index={} left={} right={} difference={}",
            w.candidate.path, w.candidate.index, w.left, w.right,
            describe(&w.difference)
        );
    }
    println!();
}

fn main() {
    // 1. Case difference: `A.txt` vs `a.txt` under case options.
    let candidates = ["A.txt", "a.txt", "b.txt"];
    run(
        "case difference (A.txt / a.txt)",
        "a.txt",
        MatchOptions { case_sensitive: true, ..MatchOptions::new() },
        "a.txt",
        MatchOptions { case_sensitive: false, ..MatchOptions::new() },
        &candidates,
    );

    // 2. Dot file rule change explained separately.
    let candidates = [".hello.txt", "hello.txt", "other.log"];
    run(
        "dot file rule change (leading dot)",
        "*.txt",
        MatchOptions { require_literal_leading_dot: true, ..MatchOptions::new() },
        "*.txt",
        MatchOptions::new(),
        &candidates,
    );

    // 3. Same-path duplicate identity preserved.
    let candidates = ["A.txt", "A.txt"];
    run(
        "duplicate identity preserved",
        "a.txt",
        MatchOptions { case_sensitive: true, ..MatchOptions::new() },
        "a.txt",
        MatchOptions { case_sensitive: false, ..MatchOptions::new() },
        &candidates,
    );
}

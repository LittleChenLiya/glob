glob
====

Support for matching file paths against Unix shell style patterns.

[![Continuous integration](https://github.com/rust-lang/glob/actions/workflows/rust.yml/badge.svg)](https://github.com/rust-lang/glob/actions/workflows/rust.yml)

[Documentation](https://docs.rs/glob)

## Usage

To use `glob`, add this to your `Cargo.toml`:

```toml
[dependencies]
glob = "0.3.2"
```

## Examples

Print all jpg files in /media/ and all of its subdirectories.

```rust
use glob::glob;

for entry in glob("/media/**/*.jpg").expect("Failed to read glob pattern") {
    match entry {
        Ok(path) => println!("{:?}", path.display()),
        Err(e) => println!("{:?}", e),
    }
}
```

## Read-only diffing of `MatchOptions` over finite candidates

`diff_match_options` / `diff_patterns` apply two explicit `MatchOptions` sets
to a caller-supplied finite list of candidate paths and report the witnesses
that match under one configuration but not the other. The checker is purely
read-only (no filesystem scanning), rejects invalid patterns uniformly before
checking anything (never half a diff), explains case / separator / dot-file
differences separately, keeps each candidate's original identity (duplicates
included), and records the finiteness guarantee in the result itself: an
empty witness list never proves two configurations equivalent in general.

Three-path example:

```rust
use glob::{diff_match_options, MatchOptions};

let case_on = MatchOptions { case_sensitive: true, ..MatchOptions::new() };
let case_off = MatchOptions { case_sensitive: false, ..MatchOptions::new() };

// Three candidate paths; "A.txt" appears twice on purpose.
let candidates = ["A.txt", "a.txt", "A.txt"];
let diff = diff_match_options("a.txt", case_on, case_off, &candidates).unwrap();

assert_eq!(diff.witnesses.len(), 2); // both "A.txt" occurrences, indices 0 and 2
println!("{}", diff.finiteness_note); // finite-check guarantee, in the result
```

Run the checks for this feature with:

```text
cargo test --test diff
cargo test --doc diff
```

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

## Batched, resumable scans

`glob::batch::BatchScanner` returns matches in fixed-size batches in
deterministic relative-path order, and each batch carries an opaque cursor you
can persist and resume later (even from another process). A returned path is
never delivered twice. The cursor is bound to the pattern, match options and
scan root, so changing any of them is rejected. When a directory observed by
the cursor has gained or lost entries, the resumed batch reports those changes
(percent-encoded names plus fingerprints) and is marked not
snapshot-consistent.

```rust,no_run
use glob::batch::BatchScanner;
use glob::MatchOptions;

let root = "/srv/incoming";
let mut scanner = BatchScanner::new("*.txt", MatchOptions::new(), root, 2).unwrap();
let batch = scanner.next_batch();
std::fs::write("scan.cursor", batch.cursor()).unwrap();

// later ...
let saved = std::fs::read_to_string("scan.cursor").unwrap();
let mut scanner = BatchScanner::resume(&saved, "*.txt", MatchOptions::new(), root).unwrap();
let rest = scanner.next_batch();
for path in &rest.matches {
    println!("{}", path.display());
}
if !rest.is_snapshot_consistent() {
    eprintln!("the directory changed during the pause: {:?}", rest.changes);
}
```

See the [`glob::batch`](https://docs.rs/glob/*/glob/batch/) module docs for the
full consistency boundaries.

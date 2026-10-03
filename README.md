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

## Filtering in-memory paths

`PatternFilter` matches a batch of path strings that you already have in
memory against several patterns at once, without touching the filesystem.
Each path is reported once and records every pattern it hit. Matching is case
sensitive and treats `/` as significant by default; only an explicit
normalizer changes the deduplication identity.

```rust
use glob::PatternFilter;

let filter = PatternFilter::new(["src/*", "**/*.rs"]).unwrap();
for hit in filter.filter(["src/a.rs", "src/a.rs", "README.md"]) {
    // `src/a.rs` matches both patterns; the duplicate is reported only once.
    println!("{} matched {:?}", hit.path(), hit.source_patterns(&filter));
}
```

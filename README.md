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

## Inspecting a pattern's features

Before running a glob over a list of paths, you can ask a compiled `Pattern`
for a read-only report of its static, syntactic features. The report is built
from the tokens the pattern was compiled into (not guessed from the original
string), so escaped metacharacters such as `[*]` are reported as literals. It
reports recursive `**` wildcards, character classes, and the longest literal
directory prefix — all as UTF-8 byte offsets. It does not traverse the
filesystem and makes no claim about matching performance.

```rust
use glob::Pattern;

let pattern = Pattern::new("src/**/test[0-9].rs").unwrap();
let features = pattern.features();

// The literal directory prefix that can be descended into directly.
let prefix = features.literal_prefix().unwrap();
assert_eq!(prefix.prefix, "src");
assert_eq!(prefix.span.start, 0);
assert_eq!(prefix.span.end, 3);

// Recursive `**` and character-class positions (UTF-8 byte offsets).
assert!(features.has_recursive());
assert!(features.has_character_class());
```

The literal prefix is only an exact, byte-for-byte guarantee when the pattern
is matched case-sensitively. With `case_sensitive: false`, a matched path may
use a different casing, so the prefix is a case-insensitive hint. See
[`Pattern::features_with`] for details.

A runnable demo that prints the report alongside real matching is available:

```sh
cargo run --example features
```

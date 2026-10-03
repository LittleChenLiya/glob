//! Short demo of stable, in-memory multi-pattern path filtering.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example filter_demo
//! ```

use glob::filter::{filter_paths, filter_paths_with};
use glob::Pattern;

fn main() {
    let patterns = [
        Pattern::new("src/*").unwrap(),
        Pattern::new("**/*.rs").unwrap(),
    ];
    let paths = ["src/a.rs", "src/a.rs", "src/b.txt", "A.rs", "a.rs"];

    println!("== default: exact-string identity (case/separator sensitive) ==");
    for hit in filter_paths(&patterns, paths.iter().copied()) {
        println!("  {:<10} matched patterns {:?}", hit.path(), hit.sources());
    }

    println!("== with a case-folding normalizer ==");
    for hit in filter_paths_with(&patterns, paths.iter().copied(), |s| {
        Ok::<_, std::convert::Infallible>(s.to_lowercase())
    })
    .unwrap()
    {
        println!("  {:<10} matched patterns {:?}", hit.path(), hit.sources());
    }
}

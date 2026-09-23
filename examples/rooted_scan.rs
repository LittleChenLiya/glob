//! Scan script for the root-bounded recursive glob entry point.
//!
//! Usage: `cargo run --example rooted_scan -- <root> [pattern] [never|within-root]`
//!
//! Prints the input root, then every returned path (relative to the root)
//! and every traversal error in scan order. The scan is abandoned with a
//! non-zero exit code if it does not finish within 90 seconds.

use std::env;
use std::path::Path;
use std::process;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use glob::{glob_rooted, FollowLinks, RootedOptions};

const TIMEOUT: Duration = Duration::from_secs(90);

fn main() {
    let mut args = env::args().skip(1);
    let root = match args.next() {
        Some(root) => root,
        None => {
            eprintln!("usage: rooted_scan <root> [pattern] [never|within-root]");
            process::exit(2);
        }
    };
    let pattern = args.next().unwrap_or_else(|| "**".to_string());
    let follow_links = match args.next().as_deref() {
        None | Some("within-root") => FollowLinks::WithinRoot,
        Some("never") => FollowLinks::Never,
        Some(other) => {
            eprintln!("unknown follow policy: {}", other);
            process::exit(2);
        }
    };

    println!("root: {}", root);
    println!("pattern: {}", pattern);
    println!("follow_links: {:?}", follow_links);

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let options = RootedOptions {
            follow_links,
            ..RootedOptions::default()
        };
        let mut count = 0usize;
        match glob_rooted(Path::new(&root), &pattern, options) {
            Ok(paths) => {
                for entry in paths {
                    match entry {
                        Ok(path) => println!("ok: {}", path.display()),
                        Err(error) => println!(
                            "error({:?}): {}: {}",
                            error.kind(),
                            error.path().display(),
                            error
                        ),
                    }
                    count += 1;
                }
            }
            Err(pattern_error) => println!("pattern error: {}", pattern_error),
        }
        let _ = tx.send(count);
    });

    match rx.recv_timeout(TIMEOUT) {
        Ok(count) => println!("done: {} item(s)", count),
        Err(_) => {
            eprintln!("scan timed out after {} seconds", TIMEOUT.as_secs());
            process::exit(1);
        }
    }
}

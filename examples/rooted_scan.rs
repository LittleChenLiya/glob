//! Scan a directory tree with `glob_rooted_with`, printing the input root
//! and every returned path or error. The scan is aborted if it runs longer
//! than a 90 second wall-clock budget.
//!
//! Usage: rooted_scan [ROOT] [PATTERN] [never|within-root] [MAX_DEPTH]

use glob::{glob_rooted_with, FollowLinks, MatchOptions, RootedOptions};
use std::env;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const BUDGET: Duration = Duration::from_secs(90);

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let root = args.next().unwrap_or_else(|| ".".to_string());
    let pattern = args.next().unwrap_or_else(|| "**".to_string());
    let follow_links = match args.next().as_deref() {
        Some("within-root") => FollowLinks::WithinRoot,
        _ => FollowLinks::Never,
    };
    let max_depth = args.next().and_then(|s| s.parse::<usize>().ok());

    println!("root:    {}", root);
    println!("pattern: {}", pattern);
    println!("follow:  {:?}", follow_links);
    println!("depth:   {:?}", max_depth);

    let options = RootedOptions {
        match_options: MatchOptions::new(),
        follow_links,
        max_depth,
    };
    let paths = match glob_rooted_with(Path::new(&root), &pattern, options) {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("invalid pattern: {}", e);
            return ExitCode::from(2);
        }
    };

    let start = Instant::now();
    for entry in paths {
        if start.elapsed() > BUDGET {
            eprintln!("scan aborted: exceeded the 90 second budget");
            return ExitCode::from(1);
        }
        match entry {
            Ok(path) => println!("{}", path.display()),
            Err(e) => println!("error: {} ({})", e.path().display(), e.error()),
        }
    }
    println!("done in {:?}", start.elapsed());
    ExitCode::SUCCESS
}

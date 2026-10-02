//! In-memory demonstration of `PatternTemplate`: compile a file-search
//! template once, then bind it to externally supplied `module`/`name`
//! segments many times. No filesystem access is involved; matching is done
//! purely in memory with `Pattern::matches`.
//!
//! Run with: `cargo run --example template`

use glob::PatternTemplate;

fn main() {
    let template = PatternTemplate::new("src/{module}/**/{name}.rs").unwrap();
    println!("template: {}", template.as_str());
    println!("slots:    {:?}", template.slots().collect::<Vec<_>>());

    // Binding 1: the file name comes from external input and happens to
    // contain glob metacharacters. They are matched literally.
    let pattern = template.bind(&[("module", "cli"), ("name", "a[1]*")]).unwrap();
    println!("\nbinding module=cli name=a[1]* -> pattern `{}`", pattern);
    for candidate in ["src/cli/commands/a[1]*.rs", "src/cli/commands/a1xyz.rs"] {
        println!(
            "  matches {:30} -> {}",
            candidate,
            pattern.matches(candidate)
        );
    }
    assert!(pattern.matches("src/cli/commands/a[1]*.rs"));
    assert!(!pattern.matches("src/cli/commands/a1xyz.rs"));

    // Binding 2: the same template is reused; nothing leaks from binding 1.
    let other = template.bind(&[("module", "web"), ("name", "index")]).unwrap();
    println!("\nbinding module=web name=index -> pattern `{}`", other);
    println!(
        "  matches src/web/ui/index.rs -> {}",
        other.matches("src/web/ui/index.rs")
    );
    println!(
        "  matches src/web/ui/a[1]*.rs -> {}",
        other.matches("src/web/ui/a[1]*.rs")
    );
    assert!(other.matches("src/web/ui/index.rs"));
    assert!(!other.matches("src/web/ui/a[1]*.rs"));

    // Values must be single path segments: separators and `..` are rejected.
    for bad in ["../etc", "a/b"] {
        match template.bind(&[("module", bad), ("name", "x")]) {
            Ok(_) => println!("\nmodule={:?} unexpectedly accepted", bad),
            Err(err) => println!("\nmodule={:?} rejected: {}", bad, err),
        }
    }
}

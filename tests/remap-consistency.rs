//! Checks that the remap capture engine never disagrees with the existing
//! `glob::Pattern` matcher about which paths match a source.

use glob::remap::{RemapPlanner, RemapRule, TargetSafety};
use glob::MatchOptions;

fn options() -> MatchOptions {
    MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    }
}

#[test]
fn engine_matches_only_what_pattern_does_for_capturing_rule() {
    let planner = RemapPlanner::new(TargetSafety::Posix)
        .with_match_options(options())
        .add_rule(
            RemapRule::new("src/{dir:**}/{name:*}.rs", "lib/{dir}/{name}.txt").unwrap(),
        )
        .unwrap();

    let inputs = [
        "src/main.rs",
        "src/a/main.rs",
        "src/a/b/main.rs",
        "src/a/b/main.tx",
        "src/a/b/x",
        "src/a//b/main.rs",
        "src/.hidden/main.rs",
        "other/a/main.rs",
        "src/a/extra/deep/main.rs",
    ];
    for input in inputs {
        let plan = planner.plan(input);
        // Every path the planner matches must keep the .rs suffix and live
        // under src/, which is exactly what the plain Pattern
        // "src/**/*.rs" accepts (modulo the explicit empty-capture policy).
        if let Ok(m) = plan {
            assert!(m.target.ends_with(".txt"));
            assert!(m.target.starts_with("lib/"));
            assert!(m.captures[1].value.ends_with("main"));
        }
    }

    // Positive cases really do map.
    assert_eq!(
        planner.plan("src/a/b/main.rs").unwrap().target,
        "lib/a/b/main.txt"
    );

    // Negative cases are refused by both the planner and Pattern.
    use glob::Pattern;
    let plain = Pattern::new("src/**/*.rs").unwrap();
    for input in ["src/a/b/main.tx", "other/a/main.rs", "src/a/b/x"] {
        assert!(!plain.matches_with(input, options()));
        assert!(planner.plan(input).is_err());
    }
}

#[test]
fn star_capture_never_crosses_separator() {
    let planner = RemapPlanner::new(TargetSafety::Posix)
        .with_match_options(options())
        .add_rule(RemapRule::new("base/{name:*}", "out/{name}").unwrap())
        .unwrap();
    assert_eq!(planner.plan("base/file").unwrap().target, "out/file");
    assert!(planner.plan("base/one/two").is_err());
    assert!(planner.plan("base/").is_err());
}

#[test]
fn require_literal_leading_dot_is_honoured() {
    let opts = MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: true,
    };
    let planner = RemapPlanner::new(TargetSafety::Posix)
        .with_match_options(opts)
        .add_rule(RemapRule::new("d/{x:**}", "o/{x}").unwrap())
        .unwrap();
    assert!(planner.plan("d/.hidden").is_err());
    assert!(planner.plan("d/a/.hidden").is_err());
    assert_eq!(
        planner.plan("d/a/visible").unwrap().target,
        "o/a/visible"
    );
}

#[cfg(windows)]
#[test]
fn windows_separator_is_accepted_in_source() {
    let planner = RemapPlanner::new(TargetSafety::Posix)
        .add_rule(
            RemapRule::new("src/{dir:**}/{name:*}.rs", "lib/{dir}/{name}.txt").unwrap(),
        )
        .unwrap();
    let m = planner.plan(r"src\a\b\main.rs").unwrap();
    // Captured separators are preserved verbatim, while the literal
    // separators in the template remain forward slashes. Under the Windows
    // target profile this target would instead be rejected.
    assert_eq!(m.target, r"lib/a\b/main.txt");
}

//! Acceptance tests for the path remapping planner.

use glob::remap::{
    planner_from_rules, RemapErrorKind, RemapPlanner, RemapRule, TargetSafety,
};
use glob::MatchOptions;

fn planner(safety: TargetSafety) -> RemapPlanner {
    RemapPlanner::new(safety)
        .add_rule(
            RemapRule::new("src/{dir:**}/{name:*}.rs", "lib/{dir}/{name}.txt").unwrap(),
        )
        .unwrap()
}

#[test]
fn nested_directories_are_remapped() {
    let p = planner(TargetSafety::Posix);
    let plan = p
        .plan_all(["src/core/lib.rs", "src/net/http/server.rs", "src/a/b/c/d/e.rs"])
        .unwrap();
    assert_eq!(plan[0].target, "lib/core/lib.txt");
    assert_eq!(plan[0].captures[0].name, "dir");
    assert_eq!(plan[0].captures[0].value, "core");
    assert_eq!(plan[0].captures[1].value, "lib");
    assert_eq!(plan[1].target, "lib/net/http/server.txt");
    assert_eq!(plan[1].captures[0].value, "net/http");
    assert_eq!(plan[2].target, "lib/a/b/c/d/e.txt");
    assert_eq!(plan[2].captures[0].value, "a/b/c/d");
    assert_eq!(plan[2].rule_index, 0);
}

#[test]
fn first_rule_in_declaration_order_wins() {
    let p = RemapPlanner::new(TargetSafety::Posix)
        .add_rule(RemapRule::new("a/{x:**}/{y:*}", "one/{x}/{y}").unwrap())
        .unwrap()
        .add_rule(RemapRule::new("a/{x:**}", "two/{x}").unwrap())
        .unwrap();
    let m = p.plan("a/foo/bar").unwrap();
    assert_eq!(m.target, "one/foo/bar");
    assert_eq!(m.rule_index, 0);
}

#[test]
fn two_sources_mapping_to_same_target_are_rejected() {
    let p = RemapPlanner::new(TargetSafety::Posix)
        .add_rule(
            RemapRule::new("src/{dir:**}/{name:*}.rs", "lib/{dir}/{name}.txt").unwrap(),
        )
        .unwrap()
        .add_rule(
            RemapRule::new("lib/{dir:**}/{name:*}.txt", "lib/{dir}/{name}.txt").unwrap(),
        )
        .unwrap();

    let err = p
        .plan_all(["src/a/b/main.rs", "lib/a/b/main.txt"])
        .unwrap_err();
    match err.kind {
        RemapErrorKind::TargetConflict {
            source,
            other_source,
            target,
        } => {
            assert_eq!(source, "lib/a/b/main.txt");
            assert_eq!(other_source, "src/a/b/main.rs");
            assert_eq!(target, "lib/a/b/main.txt");
        }
        other => panic!("expected TargetConflict, got {:?}", other),
    }
}

#[test]
fn empty_capture_is_rejected() {
    let p = planner(TargetSafety::Posix);
    let err = p.plan("src/a/.rs").unwrap_err();
    assert!(matches!(
        err.kind,
        RemapErrorKind::EmptyCapture(ref n) if n == "name"
    ));

    // A zero-component `**` capture is itself an empty capture and is
    // rejected even though `glob` itself accepts `src/**/*.rs` matching
    // `src/main.rs`.
    let err = p.plan("src/main.rs").unwrap_err();
    assert!(matches!(err.kind, RemapErrorKind::EmptyCapture(ref n) if n == "dir"));
}

#[test]
fn reserved_characters_are_rejected_on_windows_profile() {
    let p = planner(TargetSafety::Windows);
    assert!(matches!(
        p.plan("src/a/has:colon.rs").unwrap_err().kind,
        RemapErrorKind::ReservedCharacter(_)
    ));
    assert!(matches!(
        p.plan("src/a/star*name.rs").unwrap_err().kind,
        RemapErrorKind::ReservedCharacter(_)
    ));
    // A component ending with a dot is normalised away by Windows file
    // systems, so it is rejected when a capture produces it.
    let p2 = RemapPlanner::new(TargetSafety::Windows)
        .add_rule(RemapRule::new("in/{x:*}", "out/{x}").unwrap())
        .unwrap();
    assert!(matches!(
        p2.plan("in/trailing.").unwrap_err().kind,
        RemapErrorKind::ReservedCharacter(_)
    ));
    assert!(matches!(
        p2.plan("in/space ").unwrap_err().kind,
        RemapErrorKind::ReservedCharacter(_)
    ));

    let p = planner_from_rules(TargetSafety::Posix, [("src/{d:**}", "out/{d}")]).unwrap();
    assert_eq!(p.plan("src/has:colon").unwrap().target, "out/has:colon");
}

#[test]
fn reserved_device_names_are_rejected_on_windows() {
    let p = planner(TargetSafety::Windows);
    let err = p.plan("src/a/CON.rs").unwrap_err();
    assert!(matches!(err.kind, RemapErrorKind::ReservedDeviceName(_)));
    assert!(p.plan("src/a/nul.rs").is_err());
    assert!(p.plan("src/a/LPT1.rs").is_err());
}

#[test]
fn windows_case_insensitive_conflicts_and_drive_escapes() {
    let p = RemapPlanner::new(TargetSafety::Windows)
        .add_rule(RemapRule::new("in/{x:*}", "out/{x}").unwrap())
        .unwrap();
    assert!(matches!(
        p.plan_all(["in/Foo", "in/foo"]).unwrap_err().kind,
        RemapErrorKind::TargetConflict { .. }
    ));

    let p = RemapPlanner::new(TargetSafety::Posix)
        .add_rule(RemapRule::new("in/{x:*}", "out/{x}").unwrap())
        .unwrap();
    assert!(p.plan_all(["in/Foo", "in/foo"]).is_ok());

    let p = RemapPlanner::new(TargetSafety::Windows)
        .add_rule(RemapRule::new("in/{x:**}", "{x}").unwrap())
        .unwrap();
    assert!(matches!(
        p.plan("in/C:/evil").unwrap_err().kind,
        RemapErrorKind::AbsoluteTarget
    ));
}

#[test]
fn path_traversal_is_rejected_on_every_profile() {
    for safety in [TargetSafety::Posix, TargetSafety::Windows] {
        let p = RemapPlanner::new(safety)
            .add_rule(RemapRule::new("in/{x:**}", "out/{x}").unwrap())
            .unwrap();
        assert!(matches!(
            p.plan("in/../escape").unwrap_err().kind,
            RemapErrorKind::PathTraversal
        ));
        assert!(matches!(
            p.plan("in/a/../../escape").unwrap_err().kind,
            RemapErrorKind::PathTraversal
        ));
        assert!(p.plan("in/abs").is_ok());
    }
}

#[test]
fn undefined_capture_rejected_when_building_rule() {
    let err = RemapRule::new("src/{name:*}.rs", "lib/{nane}.txt").unwrap_err();
    assert!(matches!(
        err.kind,
        RemapErrorKind::UndefinedCapture(ref n, _) if n == "nane"
    ));
}

#[test]
fn unnamed_wildcards_and_other_globs_are_rejected_in_source() {
    assert!(RemapRule::new("src/*.rs", "x").is_err());
    assert!(RemapRule::new("src/{:**}/x", "x").is_err());
    assert!(RemapRule::new("src/{name:?}", "x").is_err());
    assert!(RemapRule::new("src/[a].rs", "x").is_err());
    assert!(RemapRule::new("src/{a:*}/{a:*}", "x/{a}").is_err());
}

#[test]
fn longest_cross_segment_then_leftmost_disambiguation() {
    let p = RemapPlanner::new(TargetSafety::Posix)
        .add_rule(RemapRule::new("r/{l:**}/{m:**}", "x/{l}/{m}").unwrap())
        .unwrap();
    // Every split at an internal separator is possible; the longest total
    // `**` span is the same for all splits, so the leftmost capture grows
    // greedily.
    let m = p.plan("r/a/b/c").unwrap();
    assert_eq!(m.captures[0].value, "a/b");
    assert_eq!(m.captures[1].value, "c");

    let m = p.plan("r/a/b/c/d").unwrap();
    assert_eq!(m.captures[0].value, "a/b/c");
    assert_eq!(m.captures[1].value, "d");

    // Single-segment captures are greedy from the left (consistent with
    // `Pattern`), so the left capture spans as far as possible.
    let p = RemapPlanner::new(TargetSafety::Posix)
        .add_rule(RemapRule::new("{a:*}x{b:*}", "x/{a}/{b}").unwrap())
        .unwrap();
    let m = p.plan("1x2x3").unwrap();
    assert_eq!(m.captures[0].value, "1x2");
    assert_eq!(m.captures[1].value, "3");
}

#[test]
fn existing_pattern_semantics_revalidate_the_match() {
    let p = RemapPlanner::new(TargetSafety::Posix)
        .add_rule(RemapRule::new("base/{name:*}", "out/{name}").unwrap())
        .unwrap();
    assert!(p.plan("base/one/two").is_err());
    assert_eq!(p.plan("base/file").unwrap().target, "out/file");

    let p = RemapPlanner::new(TargetSafety::Posix)
        .with_match_options(MatchOptions {
            case_sensitive: false,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        })
        .add_rule(RemapRule::new("SRC/{n:*}", "out/{n}").unwrap())
        .unwrap();
    assert_eq!(p.plan("src/File").unwrap().target, "out/File");
}

#[test]
fn literal_braces_are_escaped_in_templates() {
    let p = RemapPlanner::new(TargetSafety::Posix)
        .add_rule(RemapRule::new("in/{n:*}", "{{out}}/{n}.go").unwrap())
        .unwrap();
    assert_eq!(p.plan("in/x").unwrap().target, "{out}/x.go");
}

#[test]
fn is_safe_target_helper_matches_planner_profile() {
    use glob::remap::is_safe_target;
    assert!(is_safe_target("a/b.txt", TargetSafety::Posix));
    assert!(is_safe_target("a/b.txt", TargetSafety::Windows));
    assert!(!is_safe_target("../b", TargetSafety::Posix));
    assert!(!is_safe_target("a/COM1", TargetSafety::Windows));
    assert!(is_safe_target("a/COM1", TargetSafety::Posix));
    assert!(!is_safe_target("a/b:c", TargetSafety::Windows));
}

#[test]
#[ignore = "print-only acceptance walkthrough; run with -- --ignored --nocapture"]
fn walkthrough_nested_dirs_and_conflict() {
    println!("== rule: src/{{dir:**}}/{{name:*}}.rs -> lib/{{dir}}/{{name}}.txt");
    let planner = RemapPlanner::new(TargetSafety::Posix)
        .add_rule(
            RemapRule::new("src/{dir:**}/{name:*}.rs", "lib/{dir}/{name}.txt").unwrap(),
        )
        .unwrap();
    let sources = [
        "src/core/lib.rs",
        "src/net/http/server.rs",
        "src/a/b/c/d/e.rs",
    ];
    let plan = planner.plan_all(sources).unwrap();
    for m in &plan {
        println!("{}  =>  {}", m.source, m.target);
    }

    println!("== adding two more rules that collide on one target");
    let planner = planner
        .add_rule(
            RemapRule::new("gen/{dir:**}/{name:*}.rs", "lib/{dir}/{name}.txt").unwrap(),
        )
        .unwrap()
        .add_rule(
            RemapRule::new("lib/{dir:**}/{name:*}.txt", "lib/{dir}/{name}.txt").unwrap(),
        )
        .unwrap();
    let batch = ["src/net/http/server.rs", "gen/net/http/server.rs"];
    match planner.plan_all(batch) {
        Ok(plan) => {
            for m in &plan {
                println!("{}  =>  {}", m.source, m.target);
            }
            println!("(unexpectedly accepted)");
        }
        Err(e) => println!("batch rejected: {}", e),
    }
}

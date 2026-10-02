use glob::intersect::{intersect_patterns, IntersectError, IntersectOptions};
use glob::{MatchOptions, Pattern};

fn match_options(options: &IntersectOptions) -> MatchOptions {
    MatchOptions {
        case_sensitive: options.case_sensitive,
        require_literal_separator: true,
        require_literal_leading_dot: !options.allow_dot_files,
    }
}

/// Asserts that the patterns intersect and that the returned shortest
/// witness really is matched by both patterns under the corresponding
/// match options.
fn assert_intersects_with_options(a: &str, b: &str, options: IntersectOptions) -> String {
    let witness = intersect_patterns(a, b, options)
        .unwrap()
        .unwrap_or_else(|| panic!("expected {:?} and {:?} to intersect", a, b));
    let match_options = match_options(&options);
    assert!(
        Pattern::new(a).unwrap().matches_with(&witness, match_options),
        "witness {:?} does not match first pattern {:?}",
        witness,
        a
    );
    assert!(
        Pattern::new(b).unwrap().matches_with(&witness, match_options),
        "witness {:?} does not match second pattern {:?}",
        witness,
        b
    );
    witness
}

fn assert_intersects(a: &str, b: &str) -> String {
    assert_intersects_with_options(a, b, IntersectOptions::new())
}

fn assert_disjoint_with_options(a: &str, b: &str, options: IntersectOptions) {
    assert_eq!(
        intersect_patterns(a, b, options).unwrap(),
        None,
        "expected {:?} and {:?} to be disjoint",
        a,
        b
    );
}

fn assert_disjoint(a: &str, b: &str) {
    assert_disjoint_with_options(a, b, IntersectOptions::new());
}

#[test]
fn acceptance_criteria() {
    let witness = assert_intersects("src/*.rs", "src/main.*");
    assert_eq!(witness, "src/main.rs");

    assert_disjoint("src/*.rs", "tests/*.rs");
}

#[test]
fn literals_and_stars() {
    assert_eq!(assert_intersects("a*b", "a_b"), "a_b");
    assert_eq!(assert_intersects("cat", "c?t"), "cat");
    assert_eq!(assert_intersects("*x", "x*"), "x");
    assert_eq!(assert_intersects("foo", "foo"), "foo");

    assert_disjoint("foo", "bar");
    assert_disjoint("a?", "abc");
    assert_disjoint("*.rs", "*.txt");
    assert_disjoint("", "a");
    assert_eq!(assert_intersects("", ""), "");
}

#[test]
fn stars_do_not_cross_separators() {
    assert_disjoint("a/*", "a/b/c");
    assert_disjoint("a/*/c", "a/b/c/d");
    assert_eq!(assert_intersects("a/*", "a/?"), "a/a");
}

#[test]
fn empty_segments() {
    // `*` can match an empty path component, so the shortest common path of
    // these two patterns contains an empty segment.
    assert_eq!(assert_intersects("a/*/b", "a//b"), "a//b");
    assert_eq!(assert_intersects("a/**/b", "a//b"), "a//b");
    assert_disjoint("a/?/b", "a//b");
}

#[test]
fn character_classes() {
    let witness = assert_intersects("[a-c]x", "[b-d]x");
    assert_eq!(witness.len(), 2);
    assert_eq!(assert_intersects("[0-9]", "[5-7]"), "5");
    assert_disjoint("[a-c]", "[d-f]");
    assert_disjoint("[a-c]x", "dx");
}

#[test]
fn negated_character_classes() {
    assert_eq!(assert_intersects("[!a-c]x", "dx"), "dx");
    assert_disjoint("[!a-c]", "b");
    // The intersection of two negated classes is non-empty.
    assert_intersects("[!abc]", "[!def]");
    assert_intersects("[!a-z]", "[!A-Z]");
    // A negated class never matches a separator.
    assert_disjoint("[!a/]", "/");
}

#[test]
fn recursive_wildcards() {
    assert_eq!(assert_intersects("**", "a/b/c"), "a/b/c");
    assert_eq!(assert_intersects("a/**", "a/b"), "a/b");
    assert_eq!(assert_intersects("a/**/c", "a/b/c"), "a/b/c");
    assert_eq!(assert_intersects("a/**/b", "**/x/b"), "a/x/b");
    assert_eq!(assert_intersects("src/**", "**/main.rs"), "src/main.rs");
    // `**` also matches zero path components.
    assert_eq!(assert_intersects("a/**/b", "a/b"), "a/b");

    assert_disjoint("a/**/b", "a/**/c");
    assert_disjoint("a/**/b", "x/**/b");
}

#[test]
fn case_sensitivity() {
    let insensitive = IntersectOptions {
        case_sensitive: false,
        ..IntersectOptions::new()
    };
    let witness = assert_intersects_with_options("SRC/*.RS", "src/*.rs", insensitive);
    assert!(witness.to_ascii_lowercase().starts_with("src/"));
    assert!(witness.to_ascii_lowercase().ends_with(".rs"));

    assert_disjoint_with_options("SRC/*.RS", "src/*.rs", IntersectOptions::new());

    // Case-insensitive character classes.
    assert_intersects_with_options("[A-Z]", "[a-z]", insensitive);
    assert_disjoint_with_options("[A-Z]", "[a-z]", IntersectOptions::new());
}

#[test]
fn dot_files() {
    let no_dots = IntersectOptions {
        allow_dot_files: false,
        ..IntersectOptions::new()
    };

    // Wildcards may not match a leading dot unless dot files are allowed.
    assert_disjoint_with_options("*", ".*", no_dots);
    assert_disjoint_with_options("**", ".hidden/x", no_dots);
    assert_disjoint_with_options("?env", ".env", no_dots);
    assert_eq!(assert_intersects_with_options("*", ".*", IntersectOptions::new()), ".");

    // A literal leading dot still works when dot files are disallowed.
    assert_eq!(assert_intersects_with_options(".env", ".e?v", no_dots), ".env");
    // A dot that is not leading is not special.
    assert_eq!(assert_intersects_with_options("a.*", "a.?", no_dots), "a.a");
}

#[test]
fn state_budget() {
    let zero_budget = IntersectOptions {
        state_budget: 0,
        ..IntersectOptions::new()
    };
    assert!(matches!(
        intersect_patterns("a", "a", zero_budget),
        Err(IntersectError::StateBudgetExceeded)
    ));

    let tiny_budget = IntersectOptions {
        state_budget: 5,
        ..IntersectOptions::new()
    };
    assert!(matches!(
        intersect_patterns("a*a*a*a*a*a*a*a", "a*a*a*a*a*a*a*a", tiny_budget),
        Err(IntersectError::StateBudgetExceeded)
    ));

    // The default budget is plenty for reasonable patterns.
    assert!(intersect_patterns("a*b*c*d*e*f*g", "z*y*x*w*v*u*t", IntersectOptions::new())
        .unwrap()
        .is_none());
}

#[test]
fn unsupported_syntax_is_an_error() {
    // Backslashes are not supported: the separator is always `/`.
    match intersect_patterns("a\\b", "x", IntersectOptions::new()) {
        Err(IntersectError::InvalidFirstPattern(e)) => {
            assert!(e.msg.contains("backslash"), "unexpected message: {}", e.msg);
            assert_eq!(e.pos, 1);
        }
        other => panic!("expected InvalidFirstPattern, got {:?}", other),
    }
    assert!(matches!(
        intersect_patterns("x", "a\\b", IntersectOptions::new()),
        Err(IntersectError::InvalidSecondPattern(_))
    ));

    // Syntax rejected by the glob parser itself is reported too.
    assert!(matches!(
        intersect_patterns("a/**b", "x", IntersectOptions::new()),
        Err(IntersectError::InvalidFirstPattern(_))
    ));
    assert!(matches!(
        intersect_patterns("x", "abc[def", IntersectOptions::new()),
        Err(IntersectError::InvalidSecondPattern(_))
    ));
}

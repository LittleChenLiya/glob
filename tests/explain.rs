//! Integration tests for the `Pattern::explain`/`Pattern::explain_with`
//! match-explanation interface.

use glob::{MatchOptions, Pattern, WildcardKind};

fn options(
    case_sensitive: bool,
    require_literal_separator: bool,
    require_literal_leading_dot: bool,
) -> MatchOptions {
    MatchOptions {
        case_sensitive,
        require_literal_separator,
        require_literal_leading_dot,
    }
}

/// Demonstrates the explanation report for `src/**/test_?.rs` against a
/// path containing a Chinese directory name.
#[test]
fn demo_explanation_report_with_chinese_directory() {
    let pattern = Pattern::new("src/**/test_?.rs").unwrap();
    let input = "src/模块一/test_q.rs";
    let explanation = pattern.explain(input);

    let mut report = format!(
        "pattern: {}\ninput:   {}\n{}\n",
        pattern, input, explanation
    );
    for w in &explanation.wildcards {
        report.push_str(&format!(
            "  {:?} ate {:?} (bytes {}..{})\n",
            w.kind,
            w.text(input),
            w.start,
            w.end
        ));
    }
    // Visible with `cargo test -- --nocapture`.
    println!("{}", report);

    assert!(explanation.matched);
    assert_eq!(explanation.failure_offset, None);
    assert_eq!(explanation.wildcards.len(), 2);

    let recursive = &explanation.wildcards[0];
    assert_eq!(recursive.kind, WildcardKind::RecursiveStar);
    assert_eq!(recursive.text(input), "模块一/");
    assert_eq!((recursive.start, recursive.end), (4, 14));

    let question = &explanation.wildcards[1];
    assert_eq!(question.kind, WildcardKind::Question);
    assert_eq!(question.text(input), "q");
    assert_eq!((question.start, question.end), (19, 20));

    // Byte ranges never split a UTF-8 character.
    for w in &explanation.wildcards {
        assert!(input.is_char_boundary(w.start));
        assert!(input.is_char_boundary(w.end));
    }
}

#[test]
fn double_star_consumes_nothing_or_whole_components() {
    let pattern = Pattern::new("src/**/test_?.rs").unwrap();

    // `**` matches the empty string between the two separators.
    let input = "src/test_q.rs";
    let explanation = pattern.explain(input);
    assert!(explanation.matched);
    assert_eq!(explanation.wildcards[0].kind, WildcardKind::RecursiveStar);
    assert_eq!(explanation.wildcards[0].text(input), "");
    assert_eq!(explanation.wildcards[1].text(input), "q");

    // `**` consumes whole components including the trailing separator.
    let input = "src/one/two/test_q.rs";
    let explanation = pattern.explain(input);
    assert!(explanation.matched);
    assert_eq!(explanation.wildcards[0].text(input), "one/two/");
}

#[test]
fn multiple_stars_resolve_ambiguity_left_to_right_minimal() {
    // The leftmost star consumes as little as possible, then the next one.
    let pattern = Pattern::new("a*a*a").unwrap();
    let input = "aaaa";
    let explanation = pattern.explain(input);
    assert!(explanation.matched);
    let texts: Vec<&str> = explanation
        .wildcards
        .iter()
        .map(|w| w.text(input))
        .collect();
    assert_eq!(texts, ["", "a"]);

    let pattern = Pattern::new("*a*").unwrap();
    let input = "banana";
    let explanation = pattern.explain(input);
    assert!(explanation.matched);
    let texts: Vec<&str> = explanation
        .wildcards
        .iter()
        .map(|w| w.text(input))
        .collect();
    assert_eq!(texts, ["b", "nana"]);

    // The first `*` stops at the first possible dot, not the last one.
    let pattern = Pattern::new("*.*").unwrap();
    let input = "a.b.c";
    let explanation = pattern.explain(input);
    assert!(explanation.matched);
    let texts: Vec<&str> = explanation
        .wildcards
        .iter()
        .map(|w| w.text(input))
        .collect();
    assert_eq!(texts, ["a", "b.c"]);
}

#[test]
fn empty_inputs_and_empty_consumption() {
    // Empty pattern matches empty input, with no wildcards to report.
    let explanation = Pattern::new("").unwrap().explain("");
    assert!(explanation.matched);
    assert!(explanation.wildcards.is_empty());

    // A star happily consumes nothing.
    let explanation = Pattern::new("*").unwrap().explain("");
    assert!(explanation.matched);
    assert_eq!(explanation.wildcards.len(), 1);
    assert_eq!(explanation.wildcards[0].text(""), "");

    // Empty input against a literal fails at offset 0.
    let explanation = Pattern::new("a").unwrap().explain("");
    assert!(!explanation.matched);
    assert_eq!(explanation.failure_offset, Some(0));

    // Empty pattern against a non-empty input fails at offset 0.
    let explanation = Pattern::new("").unwrap().explain("a");
    assert!(!explanation.matched);
    assert_eq!(explanation.failure_offset, Some(0));
}

#[test]
fn case_sensitivity_option_is_respected() {
    let pattern = Pattern::new("A*").unwrap();
    let input = "aBC";

    let explanation = pattern.explain_with(input, options(false, false, false));
    assert!(explanation.matched);
    assert_eq!(explanation.wildcards[0].text(input), "BC");

    let explanation = pattern.explain_with(input, options(true, false, false));
    assert!(!explanation.matched);
    assert_eq!(explanation.failure_offset, Some(0));

    // Case-insensitive character classes are reported too.
    let pattern = Pattern::new("[a-c]*").unwrap();
    let explanation = pattern.explain_with("Bz", options(false, false, false));
    assert!(explanation.matched);
    assert_eq!(explanation.wildcards[0].kind, WildcardKind::CharClass);
    assert_eq!(explanation.wildcards[0].text("Bz"), "B");
    assert_eq!(explanation.wildcards[1].text("Bz"), "z");
}

#[test]
fn require_literal_separator_option_is_respected() {
    let pattern = Pattern::new("a*b").unwrap();
    let input = "a/b";

    let explanation = pattern.explain_with(input, options(true, false, false));
    assert!(explanation.matched);
    assert_eq!(explanation.wildcards[0].text(input), "/");

    // With a literal separator required, `*` may not cross `/`.
    let explanation = pattern.explain_with(input, options(true, true, false));
    assert!(!explanation.matched);
    assert_eq!(explanation.failure_offset, Some(1));

    // `?` may not match a separator either.
    let pattern = Pattern::new("a?b").unwrap();
    let explanation = pattern.explain_with(input, options(true, true, false));
    assert!(!explanation.matched);
    assert_eq!(explanation.failure_offset, Some(1));

    // `**` is allowed to cross separators regardless.
    let pattern = Pattern::new("a/**/b").unwrap();
    let explanation = pattern.explain_with("a/x/y/b", options(true, true, false));
    assert!(explanation.matched);
    assert_eq!(explanation.wildcards[0].text("a/x/y/b"), "x/y/");
}

#[test]
fn require_literal_leading_dot_option_is_respected() {
    let input = ".hidden";

    let explanation = Pattern::new("*")
        .unwrap()
        .explain_with(input, options(true, false, true));
    assert!(!explanation.matched);
    assert_eq!(explanation.failure_offset, Some(0));

    // A literal dot in the pattern still matches.
    let explanation = Pattern::new(".*")
        .unwrap()
        .explain_with(input, options(true, false, true));
    assert!(explanation.matched);
    assert_eq!(explanation.wildcards[0].text(input), "hidden");
}

#[test]
fn invalid_patterns_are_rejected_before_explaining() {
    // `explain` cannot even be called on these: `Pattern::new` fails first.
    assert!(Pattern::new("a/**b").is_err());
    assert!(Pattern::new("a/bc**").is_err());
    assert!(Pattern::new("a/*****").is_err());
    assert!(Pattern::new("abc[def").is_err());
    assert!(Pattern::new("abc[!").is_err());
}

#[test]
fn failure_offset_reports_earliest_mismatch() {
    // Literal mismatch partway through.
    let explanation = Pattern::new("abc").unwrap().explain("abd");
    assert!(!explanation.matched);
    assert_eq!(explanation.failure_offset, Some(2));

    // Input exhausted before the pattern.
    let explanation = Pattern::new("abc").unwrap().explain("ab");
    assert!(!explanation.matched);
    assert_eq!(explanation.failure_offset, Some(2));

    // Input left over after the pattern.
    let explanation = Pattern::new("ab").unwrap().explain("abc");
    assert!(!explanation.matched);
    assert_eq!(explanation.failure_offset, Some(2));

    // The star can consume the `x`s, so the earliest failure is where the
    // first non-`b` character shows up after `a`.
    let explanation = Pattern::new("a*b").unwrap().explain("axxx");
    assert!(!explanation.matched);
    assert_eq!(explanation.failure_offset, Some(1));

    // The failure offset is reported on a UTF-8 boundary of a Chinese path.
    let pattern = Pattern::new("src/**/test_?.rs").unwrap();
    let input = "src/模块一/main.rs";
    let explanation = pattern.explain(input);
    assert!(!explanation.matched);
    let offset = explanation.failure_offset.unwrap();
    assert_eq!(offset, 4);
    assert!(input.is_char_boundary(offset));
    println!("failure report: {}", explanation);
}

#[test]
fn question_mark_and_char_classes_report_single_chars() {
    // Multi-byte characters are consumed whole.
    let explanation = Pattern::new("?").unwrap().explain("模");
    assert!(explanation.matched);
    assert_eq!(explanation.wildcards[0].kind, WildcardKind::Question);
    assert_eq!(
        (explanation.wildcards[0].start, explanation.wildcards[0].end),
        (0, 3)
    );

    let explanation = Pattern::new("[模]").unwrap().explain("模");
    assert!(explanation.matched);
    assert_eq!(explanation.wildcards[0].kind, WildcardKind::CharClass);
    assert_eq!(explanation.wildcards[0].text("模"), "模");

    let explanation = Pattern::new("[!a]").unwrap().explain("模");
    assert!(explanation.matched);
    assert_eq!(
        explanation.wildcards[0].kind,
        WildcardKind::NegatedCharClass
    );

    let explanation = Pattern::new("模*").unwrap().explain("模块ab");
    assert!(explanation.matched);
    assert_eq!(explanation.wildcards[0].text("模块ab"), "块ab");
}

/// Exhaustively compare `explain_with` against `matches_with` over a grid
/// of short patterns, short inputs, and every combination of match options.
#[test]
fn exhaustive_comparison_with_matches() {
    let patterns = [
        "",
        "a",
        "A",
        "ab",
        "a/b",
        ".",
        "/",
        "*",
        "**",
        "?",
        "[a]",
        "[!a]",
        "[a-c]",
        "[/]",
        "[.]",
        "a*",
        "*a",
        "a*a",
        "*a*",
        "a?",
        "?a",
        "a[bc]",
        "**/a",
        "a/**",
        "**/a/**",
        "a/**/b",
        "**/*",
        "*/**",
        "*.txt",
        ".*",
        "*.*",
        "a*b*c",
        "src/**/test_?.rs",
        "[!a]*b",
        "a[/]b",
        "[a-]b",
        "????",
    ];
    let alphabet = ['a', 'b', '.', '/', 'A'];
    let mut inputs: Vec<String> = vec![String::new()];
    for &c1 in &alphabet {
        inputs.push(c1.to_string());
        for &c2 in &alphabet {
            inputs.push(format!("{}{}", c1, c2));
            for &c3 in &alphabet {
                inputs.push(format!("{}{}{}", c1, c2, c3));
            }
        }
    }
    for extra in [
        "模",
        "a模",
        "模/a",
        "src/模块一/test_q.rs",
        "src/test_q.rs",
        "hello.txt",
        ".hidden",
    ] {
        inputs.push(extra.to_string());
    }

    for case_sensitive in [true, false] {
        for require_literal_separator in [true, false] {
            for require_literal_leading_dot in [true, false] {
                let opts = options(
                    case_sensitive,
                    require_literal_separator,
                    require_literal_leading_dot,
                );
                for &pat in &patterns {
                    let pattern = Pattern::new(pat).unwrap();
                    for input in &inputs {
                        let explanation = pattern.explain_with(input, opts);
                        assert_eq!(
                            explanation.matched,
                            pattern.matches_with(input, opts),
                            "match disagreement: pattern {:?}, input {:?}, options {:?}",
                            pat,
                            input,
                            opts
                        );
                        if explanation.matched {
                            assert_eq!(explanation.failure_offset, None);
                            let mut prev_end = 0;
                            for w in &explanation.wildcards {
                                assert!(w.start <= w.end);
                                assert!(w.end <= input.len());
                                assert!(input.is_char_boundary(w.start));
                                assert!(input.is_char_boundary(w.end));
                                assert!(w.start >= prev_end);
                                prev_end = w.end;
                                let consumed = w.text(input).chars().count();
                                match w.kind {
                                    WildcardKind::Question
                                    | WildcardKind::CharClass
                                    | WildcardKind::NegatedCharClass => {
                                        assert_eq!(consumed, 1)
                                    }
                                    WildcardKind::Star | WildcardKind::RecursiveStar => {}
                                }
                            }
                        } else {
                            assert!(explanation.wildcards.is_empty());
                            let offset = explanation.failure_offset.unwrap();
                            assert!(offset <= input.len());
                            assert!(input.is_char_boundary(offset));
                        }
                    }
                }
            }
        }
    }
}

/// Patterns with many consecutive stars must be explained in polynomial
/// time: the explanation search is memoized, so there is no exponential
/// backtracking blow-up.
#[test]
fn repeated_stars_do_not_blow_up() {
    // Non-matching worst case: the trailing `b` is never found, so a naive
    // backtracker explores every distribution of the `a`s among the stars.
    let pattern = Pattern::new("*a*a*a*a*a*a*a*a*a*a*b").unwrap();
    let input = "a".repeat(300);
    let explanation = pattern.explain(&input);
    assert!(!explanation.matched);
    assert!(explanation.failure_offset.is_some());

    // Matching case with the same shape.
    let pattern = Pattern::new("*a*a*a*a*a*a*a*a*a*a*").unwrap();
    let explanation = pattern.explain(&input);
    assert!(explanation.matched);
    assert_eq!(explanation.wildcards.len(), 11);
    // Left-to-right minimal: the first ten stars consume nothing and the
    // last one soaks up the remaining `a`s.
    for w in &explanation.wildcards[..10] {
        assert_eq!(w.text(&input), "");
    }
    assert_eq!(explanation.wildcards[10].text(&input), "a".repeat(290));

    // Same for interleaved recursive wildcards in a deep path.
    let pattern = Pattern::new("**/a/**/a/**/a/**/a/**/b").unwrap();
    let mut deep = "a/".repeat(150);
    deep.push('a');
    let explanation = pattern.explain(&deep);
    assert!(!explanation.matched);
    let mut deep_b = deep.clone();
    deep_b.push_str("/b");
    let explanation = pattern.explain(&deep_b);
    assert!(explanation.matched);
}

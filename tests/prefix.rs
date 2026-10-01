use glob::{MatchOptions, Pattern, PrefixMatch, PrefixMatcher};

fn alphabet() -> Vec<char> {
    "abcdefghijklmnopqrstuvwxyz0123456789._".chars().collect()
}

fn matcher(pattern: &str, options: MatchOptions, max_states: usize) -> PrefixMatcher {
    Pattern::new(pattern)
        .unwrap()
        .prefix_matcher(options, max_states)
        .unwrap()
}

fn default_matcher(pattern: &str) -> PrefixMatcher {
    matcher(pattern, MatchOptions::new(), 1000)
}

fn possible(result: PrefixMatch) -> String {
    match result {
        PrefixMatch::Possible(completion) => completion,
        other => panic!("expected Possible, got {:?}", other),
    }
}

fn join(prefix: &[&str], completion: &str) -> String {
    if prefix.is_empty() {
        completion.to_string()
    } else {
        format!("{}/{}", prefix.join("/"), completion)
    }
}

/// Acceptance: `src/**/test?.rs` against the prefixes `src`, `docs` and
/// `src/a`; feasibility and completions are verified by full matches.
#[test]
fn acceptance_src_globstar_test_rs() {
    let options = MatchOptions::new();
    let pattern = Pattern::new("src/**/test?.rs").unwrap();
    let matcher = pattern.prefix_matcher(options, 1000).unwrap();
    let alphabet = alphabet();

    // `src` is feasible; the completion must match the whole pattern.
    let completion = possible(matcher.check(&["src"], &alphabet));
    assert!(!completion.is_empty());
    let full = join(&["src"], &completion);
    assert_eq!(full, "src/testa.rs");
    assert!(pattern.matches_with(&full, options));

    // `docs` can never lead to a match.
    assert_eq!(matcher.check(&["docs"], &alphabet), PrefixMatch::Impossible);

    // `src/a` is feasible through the recursive wildcard.
    let completion = possible(matcher.check(&["src", "a"], &alphabet));
    assert!(!completion.is_empty());
    let full = join(&["src", "a"], &completion);
    assert_eq!(full, "src/a/testa.rs");
    assert!(pattern.matches_with(&full, options));
}

#[test]
fn no_globstar() {
    let options = MatchOptions::new();
    let pattern = Pattern::new("src/test?.rs").unwrap();
    let matcher = pattern.prefix_matcher(options, 1000).unwrap();
    let alphabet = alphabet();

    // Empty prefix: the completion is the whole path.
    let completion = possible(matcher.check(&[], &alphabet));
    assert_eq!(completion, "src/testa.rs");
    assert!(pattern.matches_with(&completion, options));

    // One component in.
    let completion = possible(matcher.check(&["src"], &alphabet));
    assert_eq!(completion, "testa.rs");
    assert!(pattern.matches_with(&join(&["src"], &completion), options));

    // Without `**` an extra directory component can never match.
    assert_eq!(
        matcher.check(&["src", "a"], &alphabet),
        PrefixMatch::Impossible
    );
}

#[test]
fn prefix_matching_alone_does_not_imply_descendant() {
    let matcher = default_matcher("a");
    // The empty prefix (current directory) has "a" as a non-empty descendant.
    assert_eq!(
        matcher.check(&[], &['a']),
        PrefixMatch::Possible("a".to_string())
    );
    // The prefix "a" matches the pattern itself, but no strict descendant of
    // "a" can match it.
    assert_eq!(matcher.check(&["a"], &['a']), PrefixMatch::Impossible);
}

#[test]
fn char_sets() {
    let options = MatchOptions::new();
    let pattern = Pattern::new("[ab]c/[0-9]?.txt").unwrap();
    let matcher = pattern.prefix_matcher(options, 1000).unwrap();
    let alphabet = alphabet();

    for prefix in [&["ac"][..], &["bc"][..]] {
        let completion = possible(matcher.check(prefix, &alphabet));
        assert!(completion.chars().next().unwrap().is_ascii_digit());
        assert!(pattern.matches_with(&join(prefix, &completion), options));
    }
    assert_eq!(matcher.check(&["cc"], &alphabet), PrefixMatch::Impossible);

    // Negated sets restrict the alphabet.
    let matcher = default_matcher("[!a]b");
    assert_eq!(matcher.check(&[], &['a']), PrefixMatch::Impossible);
    assert_eq!(
        matcher.check(&[], &['a', 'b']),
        PrefixMatch::Possible("bb".to_string())
    );
}

#[test]
fn case_sensitivity() {
    let sensitive = MatchOptions::new();
    let insensitive = MatchOptions {
        case_sensitive: false,
        ..MatchOptions::new()
    };
    let pattern = Pattern::new("SRC/*.rs").unwrap();
    let alphabet: Vec<char> = "ab.rs".chars().collect();

    let matcher = pattern.prefix_matcher(sensitive, 1000).unwrap();
    assert_eq!(matcher.check(&["src"], &alphabet), PrefixMatch::Impossible);
    let completion = possible(matcher.check(&["SRC"], &alphabet));
    assert!(pattern.matches_with(&join(&["SRC"], &completion), sensitive));

    let matcher = pattern.prefix_matcher(insensitive, 1000).unwrap();
    let completion = possible(matcher.check(&["src"], &alphabet));
    assert!(pattern.matches_with(&join(&["src"], &completion), insensitive));

    // The alphabet alone decides feasibility under case folding.
    let pattern = Pattern::new("R").unwrap();
    let matcher = pattern.prefix_matcher(insensitive, 100).unwrap();
    assert_eq!(
        matcher.check(&[], &['r']),
        PrefixMatch::Possible("r".to_string())
    );
    let matcher = pattern.prefix_matcher(sensitive, 100).unwrap();
    assert_eq!(matcher.check(&[], &['r']), PrefixMatch::Impossible);
}

#[test]
fn alphabet_cannot_satisfy_suffix() {
    let matcher = default_matcher("*.rs");
    // The alphabet has no way to spell the literal ".rs" suffix.
    assert_eq!(matcher.check(&[], &['a', 'b']), PrefixMatch::Impossible);
    // A sufficient alphabet yields the shortest completion.
    assert_eq!(
        matcher.check(&[], &['a', '.', 'r', 's']),
        PrefixMatch::Possible(".rs".to_string())
    );
}

#[test]
fn state_limit() {
    let options = MatchOptions::new();

    // Compile-time limit: "abcdef" needs 7 states.
    let pattern = Pattern::new("abcdef").unwrap();
    assert!(pattern.prefix_matcher(options, 7).is_ok());
    let err = pattern.prefix_matcher(options, 6).unwrap_err();
    assert_eq!(err.required, 7);
    assert_eq!(err.limit, 6);

    // Search-time limit: the search reports Indeterminate instead of a
    // potentially wrong Impossible. "a?a?a?c" compiles to 8 states, but
    // deciding it needs more distinct state sets than that.
    let pattern = Pattern::new("a?a?a?c").unwrap();
    let matcher = pattern.prefix_matcher(options, 8).unwrap();
    assert_eq!(matcher.check(&[], &['a', 'b']), PrefixMatch::Indeterminate);
    // With enough headroom the same query reaches a verdict.
    let matcher = pattern.prefix_matcher(options, 100).unwrap();
    assert_eq!(matcher.check(&[], &['a', 'b']), PrefixMatch::Impossible);
}

#[test]
fn hidden_dot_and_separator_options() {
    // require_literal_leading_dot: `*` cannot produce a hidden component.
    let rld = MatchOptions {
        require_literal_leading_dot: true,
        ..MatchOptions::new()
    };
    let star = matcher("*", rld, 100);
    // Only the bare separator remains reachable; no hidden component can be
    // produced ("*" does match "/" when the separator need not be literal).
    assert_eq!(
        star.check(&[], &['.']),
        PrefixMatch::Possible("/".to_string())
    );
    assert_eq!(
        star.check(&[], &['.', 'a']),
        PrefixMatch::Possible("a".to_string())
    );
    // A literal dot in the pattern still matches a hidden component.
    let literal = matcher(".a", rld, 100);
    assert_eq!(
        literal.check(&[], &['.', 'a']),
        PrefixMatch::Possible(".a".to_string())
    );
    // A dot inside a char set does not (same as `matches_with`).
    let set = matcher("[.]a", rld, 100);
    assert_eq!(set.check(&[], &['.', 'a']), PrefixMatch::Impossible);

    // require_literal_separator: `*` cannot cross a separator.
    let rls = MatchOptions {
        require_literal_separator: true,
        ..MatchOptions::new()
    };
    let pattern = Pattern::new("a*b").unwrap();
    let matcher = pattern.prefix_matcher(rls, 100).unwrap();
    assert_eq!(
        matcher.check(&["a", "x"], &['a', 'b']),
        PrefixMatch::Impossible
    );
    let completion = possible(matcher.check(&[], &['a', 'b']));
    assert_eq!(completion, "ab");
    // Without the option the same prefix is feasible through `*`.
    let matcher = pattern.prefix_matcher(MatchOptions::new(), 100).unwrap();
    let completion = possible(matcher.check(&["a", "x"], &['a', 'b']));
    assert!(pattern.matches(&join(&["a", "x"], &completion)));
}

/// Cross-check against brute-force enumeration over a tiny alphabet and
/// bounded completion lengths.
#[test]
fn exhaustive_small_alphabet_cross_check() {
    let options = MatchOptions::new();
    let patterns = [
        "a", "a*b", "**/b", "a/**/b", "[ab]b", "a?b", "*", "b*a/b", "a/**", "[!a]b",
    ];
    let prefixes: [&[&str]; 4] = [&[], &["a"], &["b"], &["a", "b"]];
    let alphabet = ['a', 'b'];
    let step_chars = ['a', 'b', '/'];
    const MAX_LEN: usize = 4;

    // Shortest completion length found by enumerating all candidates.
    fn brute_shortest(
        pattern: &Pattern,
        options: &MatchOptions,
        prefix: &[&str],
        chars: &[char],
        max_len: usize,
    ) -> Option<usize> {
        let base = if prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", prefix.join("/"))
        };
        for len in 1..=max_len {
            let count = chars.len().pow(len as u32);
            for mut idx in 0..count {
                let mut full = base.clone();
                for _ in 0..len {
                    full.push(chars[idx % chars.len()]);
                    idx /= chars.len();
                }
                if pattern.matches_with(&full, *options) {
                    return Some(len);
                }
            }
        }
        None
    }

    for pattern_str in patterns {
        let pattern = Pattern::new(pattern_str).unwrap();
        let matcher = pattern.prefix_matcher(options, 1000).unwrap();
        for prefix in prefixes {
            let result = matcher.check(prefix, &alphabet);
            let shortest = brute_shortest(&pattern, &options, prefix, &step_chars, MAX_LEN);
            match result {
                PrefixMatch::Possible(completion) => {
                    assert!(!completion.is_empty());
                    let full = join(prefix, &completion);
                    assert!(
                        pattern.matches_with(&full, options),
                        "completion {:?} of {:?} below {:?} must match in full",
                        completion,
                        pattern_str,
                        prefix
                    );
                    assert_eq!(
                        Some(completion.len()),
                        shortest,
                        "completion of {:?} below {:?} must be shortest",
                        pattern_str,
                        prefix
                    );
                }
                PrefixMatch::Impossible => {
                    assert_eq!(
                        shortest, None,
                        "{:?} below {:?} was Impossible but brute force found a match",
                        pattern_str, prefix
                    );
                }
                PrefixMatch::Indeterminate => {
                    panic!("unexpected Indeterminate for {:?}", pattern_str)
                }
            }
        }
    }
}

/// Directory-pruning demo over a simulated in-memory tree (no real file
/// system access), counting the branches that pruning skips.
#[test]
fn pruning_demo_over_simulated_tree() {
    struct Dir {
        name: &'static str,
        children: &'static [Dir],
    }

    fn subtree_size(dir: &Dir) -> usize {
        1 + dir.children.iter().map(subtree_size).sum::<usize>()
    }

    static TREE: Dir = Dir {
        name: "",
        children: &[
            Dir {
                name: "src",
                children: &[
                    Dir {
                        name: "util",
                        children: &[Dir {
                            name: "test",
                            children: &[],
                        }],
                    },
                    Dir {
                        name: "test",
                        children: &[],
                    },
                ],
            },
            Dir {
                name: "docs",
                children: &[Dir {
                    name: "api",
                    children: &[Dir {
                        name: "v1",
                        children: &[],
                    }],
                }],
            },
            Dir {
                name: "tests",
                children: &[],
            },
        ],
    };

    let matcher = default_matcher("src/**/test?.rs");
    let alphabet = alphabet();

    let mut visited = 0;
    let mut skipped = 0;
    let mut pruned: Vec<String> = Vec::new();

    fn walk(
        dir: &Dir,
        prefix: &mut Vec<&'static str>,
        matcher: &PrefixMatcher,
        alphabet: &[char],
        visited: &mut usize,
        skipped: &mut usize,
        pruned: &mut Vec<String>,
    ) {
        match matcher.check(prefix, alphabet) {
            PrefixMatch::Possible(_) => {
                *visited += 1;
                for child in dir.children {
                    prefix.push(child.name);
                    walk(child, prefix, matcher, alphabet, visited, skipped, pruned);
                    prefix.pop();
                }
            }
            PrefixMatch::Impossible => {
                *skipped += subtree_size(dir);
                pruned.push(prefix.join("/"));
            }
            PrefixMatch::Indeterminate => panic!("unexpected Indeterminate"),
        }
    }

    let mut prefix = Vec::new();
    walk(
        &TREE,
        &mut prefix,
        &matcher,
        &alphabet,
        &mut visited,
        &mut skipped,
        &mut pruned,
    );

    println!(
        "pruning demo: visited={} skipped={} pruned={:?}",
        visited, skipped, pruned
    );

    // Root plus the four directories under src/ that can still match.
    assert_eq!(visited, 5);
    // docs/ (with api/ and api/v1/) and tests/ are never descended into.
    assert_eq!(skipped, 4);
    assert_eq!(pruned, ["docs".to_string(), "tests".to_string()]);
}

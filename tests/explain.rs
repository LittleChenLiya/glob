use glob::{ExplainResult, MatchOptions, Pattern, WildcardKind};

const OPTSETS: [MatchOptions; 4] = [
    MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    },
    MatchOptions {
        case_sensitive: false,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    },
    MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    },
    MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: true,
    },
];

#[test]
fn earlier_stars_stay_empty_while_later_grows() {
    let pattern = "*a".repeat(5) + "*b";
    let mut path = "a".repeat(10);
    path.push('b');
    let pat = Pattern::new(&pattern).unwrap();
    let r = pat.explain(&path);
    let lens: Vec<_> = r.captures().iter().map(|c| c.text.len()).collect();
    // five `*a` pairs align the stars empty, consuming five a's
    // positionally; the final star eats the remaining five a's and the
    // fixed b ends the path
    assert_eq!(lens, vec![0, 0, 0, 0, 0, 5]);
}

fn strings(alphabet: &[char], max_len: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    for len in 1..=max_len {
        let mut v = vec![String::new()];
        for _ in 0..len {
            v = v
                .into_iter()
                .flat_map(|s| {
                    alphabet.iter().map(move |c| {
                        let mut s = s.clone();
                        s.push(*c);
                        s
                    })
                })
                .collect();
        }
        out.extend(v);
    }
    out
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn fuzz_against_matches_with() {
    let pattern_alphabet = ['a', 'b', '/', '.', '*', '?', '[', ']', '!', '-', '中'];
    let path_alphabet = ['a', 'b', 'c', '/', '.', '中'];
    let mut rng = Rng(0x1234_5678_9abc_def0);
    let mut valid_patterns = 0u32;
    for _ in 0..40_000 {
        let plen = rng.pick(10);
        let pattern: String = (0..plen)
            .map(|_| pattern_alphabet[rng.pick(pattern_alphabet.len())])
            .collect();
        let pat = match Pattern::new(&pattern) {
            Ok(p) => p,
            Err(_) => continue,
        };
        valid_patterns += 1;
        let slen = rng.pick(14);
        let path: String = (0..slen)
            .map(|_| path_alphabet[rng.pick(path_alphabet.len())])
            .collect();
        for opts in OPTSETS {
            let m = pat.matches_with(&path, opts);
            let e = pat.explain_with(&path, opts);
            assert_eq!(m, e.is_match(), "pattern={pattern:?} path={path:?}");
            if m {
                check_captures(&pat, &pattern, &path, &e);
            }
        }
    }
    assert!(valid_patterns > 5000, "only {valid_patterns} valid");
}

fn slices_of(p: &str, s: &str, opts: MatchOptions) -> Option<Vec<String>> {
    match Pattern::new(p).unwrap().explain_with(s, opts) {
        ExplainResult::Match { captures } => {
            Some(captures.iter().map(|c| c.text.to_string()).collect())
        }
        ExplainResult::NoMatch { .. } => None,
    }
}

#[test]
fn repeated_stars_stress_match() {
    let pattern = "*a".repeat(200) + "*b";
    let mut path = "a".repeat(400);
    path.push('b');
    let pat = Pattern::new(&pattern).unwrap();
    let start = std::time::Instant::now();
    let result = pat.explain(&path);
    let elapsed = start.elapsed();
    assert!(result.is_match());
    assert_eq!(result.captures().len(), 201);
    assert!(result.captures()[0].text.is_empty());
    // the last star consumes the remaining 200 `a`s before the fixed `b`
    assert_eq!(result.captures()[200].text.len(), 200);
    assert!(elapsed.as_secs() < 5, "match took {elapsed:?}");
}

#[test]
fn repeated_stars_stress_mismatch() {
    // Naive backtracking explores this in exponential time.
    let pattern = "*a".repeat(300) + "*b";
    let path = "a".repeat(600);
    let pat = Pattern::new(&pattern).unwrap();
    let start = std::time::Instant::now();
    let result = pat.explain(&path);
    let elapsed = start.elapsed();
    assert!(!result.is_match());
    // greedy: the 300 `*a` pairs eat the first 300 `a`s, then `b` fails
    assert_eq!(result.failed_at_byte(), Some(300));
    assert!(elapsed.as_secs() < 5, "mismatch took {elapsed:?}");
}

#[test]
fn explanation_report_demo() {
    fn report(pattern: &str, path: &str) -> String {
        let pat = Pattern::new(pattern).unwrap();
        let mut out = format!("pattern {pattern:?} vs path {path:?}\n");
        match pat.explain(path) {
            ExplainResult::Match { captures } => {
                out.push_str("matched:\n");
                for cap in captures {
                    out.push_str(&format!(
                        "  {:?} @ pattern[{}..{}] consumed {:?} @ path[{}..{}]\n",
                        cap.kind,
                        cap.pattern_range.start,
                        cap.pattern_range.end,
                        cap.text,
                        cap.range.start,
                        cap.range.end,
                    ));
                }
            }
            ExplainResult::NoMatch { failed_at_byte } => {
                out.push_str(&format!(
                    "no match: earliest failure at byte {failed_at_byte}\n"
                ));
            }
        }
        out
    }

    let ok = report("src/**/test_?.rs", "src/项目/子目录/test_中.rs");
    println!("\n{ok}");
    assert!(ok.contains("matched:"));
    assert!(ok.contains("consumed \"项目/子目录/\""));
    assert!(ok.contains("consumed \"中\""));
    let bad = report("src/**/test_?.rs", "src/项目/主.ts");
    assert!(bad.contains("no match"));
}

#[test]
fn accept_chinese_example() {
    let pat = Pattern::new("src/**/test_?.rs").unwrap();
    let r = pat.explain("src/项目/子目录/test_中.rs");
    let caps = r.captures();
    assert_eq!(caps.len(), 2);
    assert_eq!(caps[0].kind, WildcardKind::RecursiveSequence);
    assert_eq!(caps[0].text, "项目/子目录/");
    assert_eq!(caps[1].kind, WildcardKind::Question);
    assert_eq!(caps[1].text, "中");
    let path = "src/项目/子目录/test_中.rs";
    assert_eq!(&path[caps[0].range.clone()], "项目/子目录/");
    assert_eq!(&path[caps[1].range.clone()], "中");
    // ranges are exact UTF-8 byte offsets
    assert_eq!(caps[0].range.start, 4);
    assert_eq!(caps[1].range.start, path.len() - 6); // 中 is 3 bytes + ".rs"
}

#[test]
fn ambiguity_leftmost_shortest() {
    let opts = MatchOptions::new();
    assert_eq!(
        slices_of("*a*", "aaa", opts),
        Some(vec!["".to_string(), "aa".to_string()])
    );
    assert_eq!(
        slices_of("*a*a*", "aaaa", opts),
        Some(vec!["".to_string(), "".to_string(), "aa".to_string()])
    );
    assert_eq!(
        slices_of("*/*", "a/b/c", opts),
        Some(vec!["a".to_string(), "b/c".to_string()])
    );
    assert_eq!(slices_of("a/**/b", "a/b", opts), Some(vec!["".to_string()]));
    assert_eq!(
        slices_of("a/**/b", "a/x/y/b", opts),
        Some(vec!["x/y/".to_string()])
    );
    assert_eq!(
        slices_of("*x*x", "xxxx", opts),
        Some(vec!["".to_string(), "xx".to_string()])
    );
    assert_eq!(
        slices_of("**/a", "x/y/a", opts),
        Some(vec!["x/y/".to_string()])
    );
    // trailing `**` consumes the remaining component(s)
    assert_eq!(
        slices_of("a/**", "a/xyz", opts),
        Some(vec!["xyz".to_string()])
    );
    assert_eq!(
        slices_of("a/**/b", "a/xyz/b", opts),
        Some(vec!["xyz/".to_string()])
    );
    assert_eq!(
        slices_of("a/**/b/**/c", "a/x/b/y/z/c", opts),
        Some(vec!["x/".to_string(), "y/z/".to_string()])
    );
}

#[test]
fn empty_strings() {
    let opts = MatchOptions::new();
    assert_eq!(slices_of("", "", opts), Some(vec![]));
    assert_eq!(slices_of("*", "", opts), Some(vec!["".to_string()]));
    assert_eq!(slices_of("**", "", opts), Some(vec!["".to_string()]));
    assert_eq!(
        Pattern::new("a").unwrap().explain("").failed_at_byte(),
        Some(0)
    );
    assert!(Pattern::new("").unwrap().explain("x").failed_at_byte() == Some(0));
}

#[test]
fn case_option() {
    let ci = MatchOptions {
        case_sensitive: false,
        ..MatchOptions::new()
    };
    assert_eq!(slices_of("a?c", "ABC", MatchOptions::new()), None);
    assert_eq!(slices_of("a?c", "ABC", ci), Some(vec!["B".to_string()]));
    assert_eq!(
        slices_of("[A-C]*", "bxy", ci),
        Some(vec!["b".to_string(), "xy".to_string()])
    );
}

#[test]
fn separator_and_dot_options() {
    let lit = MatchOptions {
        require_literal_separator: true,
        ..MatchOptions::new()
    };
    assert_eq!(slices_of("a*b", "a/x/b", lit), None);
    assert_eq!(
        Pattern::new("a?b")
            .unwrap()
            .explain_with("a/b", lit)
            .failed_at_byte(),
        Some(1)
    );
    assert_eq!(
        slices_of("a*b", "a/x/b", MatchOptions::new()),
        Some(vec!["/x/".to_string()])
    );
    let dot = MatchOptions {
        require_literal_leading_dot: true,
        ..MatchOptions::new()
    };
    assert_eq!(slices_of("*", ".hidden", dot), None);
    assert_eq!(
        slices_of(".*", ".hidden", dot),
        Some(vec!["hidden".to_string()])
    );
    assert_eq!(slices_of("**/*", "a/.b", dot), None);
}

#[test]
fn invalid_patterns_error() {
    for bad in ["a/**b", "a/*****", "a**b", "abc[def", "abc[!def", "abc["] {
        assert!(Pattern::new(bad).is_err(), "{bad:?} should be invalid");
    }
}

#[test]
fn failure_positions() {
    let opts = MatchOptions::new();
    let pos = |p: &str, s: &str| {
        Pattern::new(p)
            .unwrap()
            .explain_with(s, opts)
            .failed_at_byte()
    };
    assert_eq!(pos("abc", "abd"), Some(2));
    assert_eq!(pos("abc", "ab"), Some(2));
    assert_eq!(pos("ab", "abc"), Some(2));
    // on the shortest (empty star) attempt, the fixed `c` conflicts first
    assert_eq!(pos("a*c", "abd"), Some(1));
    assert_eq!(pos("a*c", "ab"), Some(1));
    assert_eq!(pos("*x", "abc"), Some(0));
    assert_eq!(pos("a/**/b", "a/x/c"), Some(2));
    // failure must land on a UTF-8 boundary (start of 文)
    assert_eq!(pos("a中", "a文"), Some(1));
}

#[test]
fn pattern_ranges_reported() {
    let pat = Pattern::new("src/**/test_?.rs").unwrap();
    let r = pat.explain("src/foo/test_x.rs");
    let caps = r.captures();
    assert_eq!(&pat.as_str()[caps[0].pattern_range.clone()], "**");
    assert_eq!(&pat.as_str()[caps[1].pattern_range.clone()], "?");
    let pat = Pattern::new("a[!0-9]b").unwrap();
    let explained = pat.explain("axb");
    let caps = explained.captures();
    assert_eq!(&pat.as_str()[caps[0].pattern_range.clone()], "[!0-9]");
    assert_eq!(caps[0].kind, WildcardKind::CharacterClass { negated: true });
}

#[test]
fn exhaustive_small_alphabet() {
    let patterns = [
        "",
        "a",
        "b",
        "?",
        "*",
        "**",
        "a*",
        "*a",
        "*a*",
        "a*b",
        "*a*b*",
        "?",
        "??",
        "a?",
        "?a",
        "*?",
        "?*",
        "*?*",
        "[a]",
        "[ab]",
        "[!a]",
        "[a-c]",
        "[!a-c]",
        "[]a]",
        "[!]a]",
        "[a-]",
        "a[b]",
        "a[!b]",
        "*[ab]*",
        "a/*",
        "*/a",
        "**/a",
        "a/**",
        "a/**/b",
        "a/**/b/**/c",
        "**/**/a",
        "*/*",
        "*/**/*",
        "a*b*c",
        "*a*a*",
        "**/*",
        ".",
        ".*",
        "*.",
        "**/.*",
        "a//b",
        "/a",
        "a/",
        "a*b?",
        "?*?",
        "**?",
        "a/**/**/b",
        "[Aa]",
        "[a-c]*[x-z]",
        "**/x/**",
    ];
    let paths = strings(&['a', 'b', 'c', '/', '.'], 4);
    let mut checks = 0;
    for p in patterns {
        let pat = match Pattern::new(p) {
            Ok(pat) => pat,
            Err(_) => continue,
        };
        for opts in OPTSETS {
            for path in &paths {
                let m = pat.matches_with(path, opts);
                let e = pat.explain_with(path, opts);
                assert_eq!(m, e.is_match(), "pattern={p:?} path={path:?}");
                if m {
                    check_captures(&pat, p, path, &e);
                }
                checks += 1;
            }
        }
    }
    assert!(checks > 100_000, "ran {checks} checks");
}

fn check_captures(_pat: &Pattern, p: &str, path: &str, e: &ExplainResult) {
    let caps = e.captures();
    for cap in caps {
        assert!(path.is_char_boundary(cap.range.start));
        assert!(path.is_char_boundary(cap.range.end));
        assert_eq!(&path[cap.range.clone()], cap.text);
    }
    // Capture ranges, together with fixed chars between them, tile the
    // whole path.
    let mut tiled = String::new();
    let mut last = 0usize;
    for cap in caps {
        tiled.push_str(&path[last..cap.range.start]);
        tiled.push_str(cap.text);
        last = cap.range.end;
    }
    tiled.push_str(&path[last..]);
    if tiled != path {
        // `**/` consumes the following separator as part of its token, which
        // is invisible to fixed chars; allow that single duplicated slash.
        let strip = |s: &str| s.chars().filter(|c| *c != '/').collect::<String>();
        assert_eq!(strip(&tiled), strip(path), "pattern={p:?} path={path:?}");
    }
}

#[test]
fn exhaustive_unicode_alphabet() {
    let patterns = ["*", "?", "中*", "*中", "*中*", "?中?", "[中-文]", "**/x"];
    let paths = strings(&['中', '文', '/', 'a'], 3);
    for p in patterns {
        let pat = Pattern::new(p).unwrap();
        for opts in OPTSETS {
            for path in &paths {
                assert_eq!(
                    pat.matches_with(path, opts),
                    pat.explain_with(path, opts).is_match()
                );
            }
        }
    }
}

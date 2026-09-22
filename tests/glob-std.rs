// Copyright 2013-2014 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// ignore-windows TempDir may cause IoError on windows: #10462

#![cfg_attr(test, deny(warnings))]

use glob::{glob, glob_with};
use std::env;
use std::fs;
use std::path::PathBuf;

#[test]
fn main() {
    fn mk_file(path: &str, directory: bool) {
        if directory {
            fs::create_dir(path).unwrap();
        } else {
            fs::File::create(path).unwrap();
        }
    }

    fn mk_symlink_file(original: &str, link: &str) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(original, link).unwrap();
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::symlink_file;
            symlink_file(original, link).unwrap();
        }
    }

    fn mk_symlink_dir(original: &str, link: &str) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(original, link).unwrap();
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::symlink_dir;
            symlink_dir(original, link).unwrap();
        }
    }

    fn glob_vec(pattern: &str) -> Vec<PathBuf> {
        glob(pattern).unwrap().map(|r| r.unwrap()).collect()
    }

    fn glob_with_vec(pattern: &str, options: glob::MatchOptions) -> Vec<PathBuf> {
        glob_with(pattern, options)
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    let root = tempfile::tempdir();
    let root = root.expect("Should have created a temp directory");
    assert!(env::set_current_dir(root.path()).is_ok());

    mk_file("aaa", true);
    mk_file("aaa/apple", true);
    mk_file("aaa/orange", true);
    mk_file("aaa/tomato", true);
    mk_file("aaa/tomato/tomato.txt", false);
    mk_file("aaa/tomato/tomoto.txt", false);
    mk_file("bbb", true);
    mk_file("bbb/specials", true);
    mk_file("bbb/specials/!", false);
    // Creating symlinks may require privileges the test environment lacks
    // (e.g. Windows without developer mode); skip the test in that case.
    {
        #[cfg(unix)]
        let probe = std::os::unix::fs::symlink("aaa/apple", "aaa/symlink_probe");
        #[cfg(windows)]
        let probe = std::os::windows::fs::symlink_file("aaa/apple", "aaa/symlink_probe");
        #[cfg(not(any(unix, windows)))]
        let probe: std::io::Result<()> = Err(std::io::Error::new(std::io::ErrorKind::Other, "n/a"));
        match probe {
            Ok(()) => fs::remove_file("aaa/symlink_probe").unwrap(),
            Err(e) => {
                eprintln!("skipping: cannot create symlinks ({})", e);
                return;
            }
        }
    }

    // a valid symlink
    mk_symlink_file("aaa/apple", "aaa/green_apple");
    // a broken symlink
    mk_symlink_file("aaa/setsuna", "aaa/kazusa");

    // windows does not allow `*` or `?` characters to exist in filenames
    if env::consts::FAMILY != "windows" {
        mk_file("bbb/specials/*", false);
        mk_file("bbb/specials/?", false);
    }

    mk_file("bbb/specials/[", false);
    mk_file("bbb/specials/]", false);
    mk_file("ccc", true);
    mk_file("xyz", true);
    mk_file("xyz/x", false);
    mk_file("xyz/y", false);
    mk_file("xyz/z", false);

    mk_file("r", true);
    mk_file("r/current_dir.md", false);
    mk_file("r/one", true);
    mk_file("r/one/a.md", false);
    mk_file("r/one/another", true);
    mk_file("r/one/another/a.md", false);
    mk_file("r/one/another/deep", true);
    mk_file("r/one/another/deep/spelunking.md", false);
    mk_file("r/another", true);
    mk_file("r/another/a.md", false);
    mk_file("r/two", true);
    mk_file("r/two/b.md", false);
    mk_file("r/three", true);
    mk_file("r/three/c.md", false);

    mk_file("dirsym", true);
    mk_symlink_dir(root.path().join("r").to_str().unwrap(), "dirsym/link");

    assert_eq!(
        glob_vec("dirsym/**/*.md"),
        vec!(
            PathBuf::from("dirsym/link/another/a.md"),
            PathBuf::from("dirsym/link/current_dir.md"),
            PathBuf::from("dirsym/link/one/a.md"),
            PathBuf::from("dirsym/link/one/another/a.md"),
            PathBuf::from("dirsym/link/one/another/deep/spelunking.md"),
            PathBuf::from("dirsym/link/three/c.md"),
            PathBuf::from("dirsym/link/two/b.md")
        )
    );

    // all recursive entities
    assert_eq!(
        glob_vec("r/**"),
        vec!(
            PathBuf::from("r/another"),
            PathBuf::from("r/one"),
            PathBuf::from("r/one/another"),
            PathBuf::from("r/one/another/deep"),
            PathBuf::from("r/three"),
            PathBuf::from("r/two")
        )
    );

    // std-canonicalized windows verbatim disk paths should work
    if env::consts::FAMILY == "windows" {
        let r_verbatim = PathBuf::from("r").canonicalize().unwrap();
        assert_eq!(
            glob_vec(&format!("{}\\**", r_verbatim.display().to_string()))
                .into_iter()
                .map(|p| p.strip_prefix(&r_verbatim).unwrap().to_owned())
                .collect::<Vec<_>>(),
            vec!(
                PathBuf::from("another"),
                PathBuf::from("one"),
                PathBuf::from("one\\another"),
                PathBuf::from("one\\another\\deep"),
                PathBuf::from("three"),
                PathBuf::from("two")
            )
        );
    }

    // collapse consecutive recursive patterns
    assert_eq!(
        glob_vec("r/**/**"),
        vec!(
            PathBuf::from("r/another"),
            PathBuf::from("r/one"),
            PathBuf::from("r/one/another"),
            PathBuf::from("r/one/another/deep"),
            PathBuf::from("r/three"),
            PathBuf::from("r/two")
        )
    );

    assert_eq!(
        glob_vec("r/**/*"),
        vec!(
            PathBuf::from("r/another"),
            PathBuf::from("r/another/a.md"),
            PathBuf::from("r/current_dir.md"),
            PathBuf::from("r/one"),
            PathBuf::from("r/one/a.md"),
            PathBuf::from("r/one/another"),
            PathBuf::from("r/one/another/a.md"),
            PathBuf::from("r/one/another/deep"),
            PathBuf::from("r/one/another/deep/spelunking.md"),
            PathBuf::from("r/three"),
            PathBuf::from("r/three/c.md"),
            PathBuf::from("r/two"),
            PathBuf::from("r/two/b.md")
        )
    );

    // followed by a wildcard
    assert_eq!(
        glob_vec("r/**/*.md"),
        vec!(
            PathBuf::from("r/another/a.md"),
            PathBuf::from("r/current_dir.md"),
            PathBuf::from("r/one/a.md"),
            PathBuf::from("r/one/another/a.md"),
            PathBuf::from("r/one/another/deep/spelunking.md"),
            PathBuf::from("r/three/c.md"),
            PathBuf::from("r/two/b.md")
        )
    );

    // followed by a precise pattern
    assert_eq!(
        glob_vec("r/one/**/a.md"),
        vec!(
            PathBuf::from("r/one/a.md"),
            PathBuf::from("r/one/another/a.md")
        )
    );

    // followed by another recursive pattern
    // collapses consecutive recursives into one
    assert_eq!(
        glob_vec("r/one/**/**/a.md"),
        vec!(
            PathBuf::from("r/one/a.md"),
            PathBuf::from("r/one/another/a.md")
        )
    );

    // followed by two precise patterns
    assert_eq!(
        glob_vec("r/**/another/a.md"),
        vec!(
            PathBuf::from("r/another/a.md"),
            PathBuf::from("r/one/another/a.md")
        )
    );

    assert_eq!(glob_vec(""), Vec::<PathBuf>::new());
    assert_eq!(glob_vec("."), vec!(PathBuf::from(".")));
    assert_eq!(glob_vec(".."), vec!(PathBuf::from("..")));

    assert_eq!(glob_vec("aaa"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("aaa/"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("a"), Vec::<PathBuf>::new());
    assert_eq!(glob_vec("aa"), Vec::<PathBuf>::new());
    assert_eq!(glob_vec("aaaa"), Vec::<PathBuf>::new());

    assert_eq!(glob_vec("aaa/apple"), vec!(PathBuf::from("aaa/apple")));
    assert_eq!(glob_vec("aaa/apple/nope"), Vec::<PathBuf>::new());

    // windows should support both / and \ as directory separators
    if env::consts::FAMILY == "windows" {
        assert_eq!(glob_vec("aaa\\apple"), vec!(PathBuf::from("aaa/apple")));
    }

    assert_eq!(
        glob_vec("???/"),
        vec!(
            PathBuf::from("aaa"),
            PathBuf::from("bbb"),
            PathBuf::from("ccc"),
            PathBuf::from("xyz")
        )
    );

    assert_eq!(
        glob_vec("aaa/tomato/tom?to.txt"),
        vec!(
            PathBuf::from("aaa/tomato/tomato.txt"),
            PathBuf::from("aaa/tomato/tomoto.txt")
        )
    );

    assert_eq!(
        glob_vec("xyz/?"),
        vec!(
            PathBuf::from("xyz/x"),
            PathBuf::from("xyz/y"),
            PathBuf::from("xyz/z")
        )
    );

    assert_eq!(glob_vec("a*"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("*a*"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("a*a"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("aaa*"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("*aaa"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("*aaa*"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("*a*a*a*"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("aaa*/"), vec!(PathBuf::from("aaa")));

    assert_eq!(
        glob_vec("aaa/*"),
        vec!(
            PathBuf::from("aaa/apple"),
            PathBuf::from("aaa/green_apple"),
            PathBuf::from("aaa/kazusa"),
            PathBuf::from("aaa/orange"),
            PathBuf::from("aaa/tomato"),
        )
    );

    assert_eq!(
        glob_vec("aaa/*a*"),
        vec!(
            PathBuf::from("aaa/apple"),
            PathBuf::from("aaa/green_apple"),
            PathBuf::from("aaa/kazusa"),
            PathBuf::from("aaa/orange"),
            PathBuf::from("aaa/tomato")
        )
    );

    assert_eq!(
        glob_vec("*/*/*.txt"),
        vec!(
            PathBuf::from("aaa/tomato/tomato.txt"),
            PathBuf::from("aaa/tomato/tomoto.txt")
        )
    );

    assert_eq!(
        glob_vec("*/*/t[aob]m?to[.]t[!y]t"),
        vec!(
            PathBuf::from("aaa/tomato/tomato.txt"),
            PathBuf::from("aaa/tomato/tomoto.txt")
        )
    );

    assert_eq!(glob_vec("./aaa"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("./*"), glob_vec("*"));
    assert_eq!(glob_vec("*/..").pop().unwrap(), PathBuf::from("xyz/.."));
    assert_eq!(glob_vec("aaa/../bbb"), vec!(PathBuf::from("aaa/../bbb")));
    assert_eq!(glob_vec("nonexistent/../bbb"), Vec::<PathBuf>::new());
    assert_eq!(glob_vec("aaa/tomato/tomato.txt/.."), Vec::<PathBuf>::new());

    assert_eq!(glob_vec("aaa/tomato/tomato.txt/"), Vec::<PathBuf>::new());

    // Ensure to find a broken symlink.
    assert_eq!(glob_vec("aaa/kazusa"), vec!(PathBuf::from("aaa/kazusa")));

    assert_eq!(glob_vec("aa[a]"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("aa[abc]"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("a[bca]a"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("aa[b]"), Vec::<PathBuf>::new());
    assert_eq!(glob_vec("aa[xyz]"), Vec::<PathBuf>::new());
    assert_eq!(glob_vec("aa[]]"), Vec::<PathBuf>::new());

    assert_eq!(glob_vec("aa[!b]"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("aa[!bcd]"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("a[!bcd]a"), vec!(PathBuf::from("aaa")));
    assert_eq!(glob_vec("aa[!a]"), Vec::<PathBuf>::new());
    assert_eq!(glob_vec("aa[!abc]"), Vec::<PathBuf>::new());

    assert_eq!(
        glob_vec("bbb/specials/[[]"),
        vec!(PathBuf::from("bbb/specials/["))
    );
    assert_eq!(
        glob_vec("bbb/specials/!"),
        vec!(PathBuf::from("bbb/specials/!"))
    );
    assert_eq!(
        glob_vec("bbb/specials/[]]"),
        vec!(PathBuf::from("bbb/specials/]"))
    );

    mk_file("i", true);
    mk_file("i/qwe", true);
    mk_file("i/qwe/.aaa", false);
    mk_file("i/qwe/.bbb", true);
    mk_file("i/qwe/.bbb/ccc", false);
    mk_file("i/qwe/.bbb/.ddd", false);
    mk_file("i/qwe/eee", false);

    let options = glob::MatchOptions {
        case_sensitive: false,
        require_literal_separator: true,
        require_literal_leading_dot: true,
    };
    assert_eq!(glob_with_vec("i/**/*a*", options), Vec::<PathBuf>::new());
    assert_eq!(glob_with_vec("i/**/*c*", options), Vec::<PathBuf>::new());
    assert_eq!(glob_with_vec("i/**/*d*", options), Vec::<PathBuf>::new());
    assert_eq!(
        glob_with_vec("i/**/*e*", options),
        vec!(PathBuf::from("i/qwe"), PathBuf::from("i/qwe/eee"))
    );

    if env::consts::FAMILY != "windows" {
        assert_eq!(
            glob_vec("bbb/specials/[*]"),
            vec!(PathBuf::from("bbb/specials/*"))
        );
        assert_eq!(
            glob_vec("bbb/specials/[?]"),
            vec!(PathBuf::from("bbb/specials/?"))
        );
    }

    if env::consts::FAMILY == "windows" {
        assert_eq!(
            glob_vec("bbb/specials/[![]"),
            vec!(
                PathBuf::from("bbb/specials/!"),
                PathBuf::from("bbb/specials/]")
            )
        );

        assert_eq!(
            glob_vec("bbb/specials/[!]]"),
            vec!(
                PathBuf::from("bbb/specials/!"),
                PathBuf::from("bbb/specials/[")
            )
        );

        assert_eq!(
            glob_vec("bbb/specials/[!!]"),
            vec!(
                PathBuf::from("bbb/specials/["),
                PathBuf::from("bbb/specials/]")
            )
        );
    } else {
        assert_eq!(
            glob_vec("bbb/specials/[![]"),
            vec!(
                PathBuf::from("bbb/specials/!"),
                PathBuf::from("bbb/specials/*"),
                PathBuf::from("bbb/specials/?"),
                PathBuf::from("bbb/specials/]")
            )
        );

        assert_eq!(
            glob_vec("bbb/specials/[!]]"),
            vec!(
                PathBuf::from("bbb/specials/!"),
                PathBuf::from("bbb/specials/*"),
                PathBuf::from("bbb/specials/?"),
                PathBuf::from("bbb/specials/[")
            )
        );

        assert_eq!(
            glob_vec("bbb/specials/[!!]"),
            vec!(
                PathBuf::from("bbb/specials/*"),
                PathBuf::from("bbb/specials/?"),
                PathBuf::from("bbb/specials/["),
                PathBuf::from("bbb/specials/]")
            )
        );

        assert_eq!(
            glob_vec("bbb/specials/[!*]"),
            vec!(
                PathBuf::from("bbb/specials/!"),
                PathBuf::from("bbb/specials/?"),
                PathBuf::from("bbb/specials/["),
                PathBuf::from("bbb/specials/]")
            )
        );

        assert_eq!(
            glob_vec("bbb/specials/[!?]"),
            vec!(
                PathBuf::from("bbb/specials/!"),
                PathBuf::from("bbb/specials/*"),
                PathBuf::from("bbb/specials/["),
                PathBuf::from("bbb/specials/]")
            )
        );
    }
}

mod pattern_set {
    use glob::{ErrorPolicy, PatternSet, PatternSetOptions, RuleKind};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn mk_tree(root: &Path) {
        fs::create_dir_all(root.join("src/generated")).unwrap();
        fs::write(root.join("src/a.rs"), "a").unwrap();
        fs::write(root.join("src/generated/x.rs"), "x").unwrap();
        fs::write(root.join("src/generated/keep.rs"), "keep").unwrap();
    }

    fn reinclude_set() -> PatternSet {
        PatternSet::compile(
            &[
                (RuleKind::Include, "src/**/*.rs"),
                (RuleKind::Exclude, "src/generated/**"),
                (RuleKind::Include, "src/generated/keep.rs"),
            ],
            PatternSetOptions::default(),
        )
        .unwrap()
    }

    fn rel_strings(outcome: &glob::WalkOutcome) -> Vec<String> {
        let mut v: Vec<String> = outcome
            .matches
            .iter()
            .map(|m| m.relative.join("/"))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn include_exclude_reinclude() {
        let root = tempfile::tempdir().unwrap();
        mk_tree(root.path());
        let outcome = reinclude_set().walk(root.path()).unwrap();
        assert_eq!(rel_strings(&outcome), ["src/a.rs", "src/generated/keep.rs"]);

        // keep.rs is ultimately selected by the last rule, which overrides
        // both earlier matching rules.
        let keep = outcome
            .matches
            .iter()
            .find(|m| m.relative == ["src", "generated", "keep.rs"])
            .unwrap();
        assert_eq!(keep.winner, 2);
        assert_eq!(keep.overridden, [0, 1]);

        let a = outcome
            .matches
            .iter()
            .find(|m| m.relative == ["src", "a.rs"])
            .unwrap();
        assert_eq!(a.winner, 0);
        assert!(a.overridden.is_empty());
    }

    #[test]
    fn explain_does_not_touch_filesystem() {
        let set = reinclude_set();
        // This path does not exist anywhere on disk.
        let ex = set.explain(&["src", "generated", "keep.rs"]);
        assert!(ex.included);
        assert_eq!(ex.winner, Some(2));
        assert_eq!(ex.matched, [0, 1, 2]);

        let ex = set.explain(&["src", "generated", "x.rs"]);
        assert!(!ex.included);
        assert_eq!(ex.winner, Some(1));

        let ex = set.explain(&["README.md"]);
        assert!(!ex.included);
        assert_eq!(ex.winner, None);
    }

    #[test]
    fn naive_pruning_loses_reincluded_file() {
        // Simulate a walker that prunes any directory the rule set currently
        // excludes: keep.rs is never even visited, so the re-include rule
        // cannot rescue it. This is why PatternSet::walk never prunes.
        let root = tempfile::tempdir().unwrap();
        mk_tree(root.path());
        let set = reinclude_set();

        fn naive_walk(set: &PatternSet, dir: &Path, rel: &mut Vec<String>, out: &mut Vec<String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().into_owned();
                rel.push(name);
                let refs: Vec<&str> = rel.iter().map(String::as_str).collect();
                if entry.file_type().unwrap().is_dir() {
                    // Prune directories whose contents an exclude rule claims,
                    // probed via a placeholder child path; unmentioned
                    // directories are still descended.
                    let mut probe = refs.clone();
                    probe.push("probe");
                    let ex = set.explain(&probe);
                    let explicitly_excluded = !ex.included && ex.winner.is_some();
                    if !explicitly_excluded {
                        naive_walk(set, &entry.path(), rel, out);
                    } // else: pruned!
                } else if set.explain(&refs).included {
                    out.push(rel.join("/"));
                }
                rel.pop();
            }
        }

        let mut found = Vec::new();
        naive_walk(&set, root.path(), &mut Vec::new(), &mut found);
        found.sort();
        assert_eq!(found, ["src/a.rs"]);
        assert!(!found.contains(&"src/generated/keep.rs".to_string()));

        // The real walk does find it.
        let outcome = set.walk(root.path()).unwrap();
        assert!(rel_strings(&outcome).contains(&"src/generated/keep.rs".to_string()));
    }

    #[test]
    fn case_policy_is_explicit() {
        let rules = [(RuleKind::Include, "SRC/*.RS")];
        let sensitive = PatternSet::compile(
            &rules,
            PatternSetOptions {
                case_sensitive: true,
                ..PatternSetOptions::default()
            },
        )
        .unwrap();
        assert!(!sensitive.explain(&["src", "a.rs"]).included);

        let insensitive = PatternSet::compile(
            &rules,
            PatternSetOptions {
                case_sensitive: false,
                ..PatternSetOptions::default()
            },
        )
        .unwrap();
        assert!(insensitive.explain(&["src", "a.rs"]).included);
    }

    #[test]
    fn trailing_separator_makes_a_directory_rule() {
        let set = PatternSet::compile(
            &[
                (RuleKind::Include, "src/**/*.rs"),
                (RuleKind::Exclude, "src/generated/"),
            ],
            PatternSetOptions::default(),
        )
        .unwrap();
        // The directory rule covers the directory itself and its contents.
        assert!(!set.explain(&["src", "generated"]).included);
        assert!(!set.explain(&["src", "generated", "x.rs"]).included);
        assert!(
            !set.explain(&["src", "generated", "nested", "y.rs"])
                .included
        );
        assert!(set.explain(&["src", "a.rs"]).included);

        // A file rule with the same text but no trailing separator matches
        // only the exact path.
        let file_rule = PatternSet::compile(
            &[(RuleKind::Exclude, "src/generated")],
            PatternSetOptions::default(),
        )
        .unwrap();
        assert!(!file_rule.explain(&["src", "generated"]).included);
        assert!(file_rule
            .explain(&["src", "generated", "x.rs"])
            .winner
            .is_none());
    }

    #[test]
    fn invalid_rule_reports_rule_and_position() {
        let err = PatternSet::compile(
            &[
                (RuleKind::Include, "src/**/*.rs"),
                (RuleKind::Exclude, "src/***bad"),
            ],
            PatternSetOptions::default(),
        )
        .unwrap_err();
        assert_eq!(err.rule_index, 1);
        assert_eq!(err.rule, "src/***bad");
        // The error carries the character position within the rule.
        assert_eq!(err.error.pos, 6);
        let msg = err.to_string();
        assert!(msg.contains("rule 1"), "{}", msg);
        assert!(msg.contains("position 6"), "{}", msg);

        // Compilation is atomic: the valid first rule does not produce a
        // half-working set.
        assert!(PatternSet::compile(
            &[(RuleKind::Include, "src/**/*.rs")],
            PatternSetOptions::default()
        )
        .is_ok());
    }

    #[test]
    fn read_errors_honor_fail_and_report_policies() {
        let missing = PathBuf::from("definitely/not/a/real/directory");
        let rules = [(RuleKind::Include, "**/*")];

        let failing = PatternSet::compile(&rules, PatternSetOptions::default()).unwrap();
        assert!(failing.walk(&missing).is_err());

        let reporting = PatternSet::compile(
            &rules,
            PatternSetOptions {
                error_policy: ErrorPolicy::Report,
                ..PatternSetOptions::default()
            },
        )
        .unwrap();
        let outcome = reporting.walk(&missing).unwrap();
        assert!(outcome.matches.is_empty());
        assert_eq!(outcome.errors.len(), 1);
    }

    fn try_symlink_dir(original: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(original, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(original, link).is_ok()
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (original, link);
            false
        }
    }

    #[test]
    fn symlink_loop_is_not_followed_forever() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("f.rs"), "f").unwrap();
        if !try_symlink_dir(&real, &real.join("loop")) {
            eprintln!("skipping: platform cannot create directory symlinks");
            return;
        }

        let rules = [(RuleKind::Include, "**/*.rs")];

        // Default: links are not followed at all.
        let set = PatternSet::compile(&rules, PatternSetOptions::default()).unwrap();
        let outcome = set.walk(root.path()).unwrap();
        assert_eq!(rel_strings(&outcome), ["real/f.rs"]);

        // Following links: the loop back into the real directory is detected
        // via the canonical directory identity and traversal terminates.
        let following = PatternSet::compile(
            &rules,
            PatternSetOptions {
                follow_links: true,
                ..PatternSetOptions::default()
            },
        )
        .unwrap();
        let outcome = following.walk(root.path()).unwrap();
        assert_eq!(rel_strings(&outcome), ["real/f.rs"]);
    }

    #[test]
    fn symlink_escaping_root_is_not_followed() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("root");
        let outside = outer.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.rs"), "s").unwrap();
        if !try_symlink_dir(&outside, &root.join("escape")) {
            eprintln!("skipping: platform cannot create directory symlinks");
            return;
        }

        let set = PatternSet::compile(
            &[(RuleKind::Include, "**/*.rs")],
            PatternSetOptions {
                follow_links: true,
                ..PatternSetOptions::default()
            },
        )
        .unwrap();
        let outcome = set.walk(&root).unwrap();
        assert!(outcome.matches.is_empty());
    }
}

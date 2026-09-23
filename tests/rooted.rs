use glob::{glob_rooted, glob_rooted_with, FollowLinks, MatchOptions, RootedOptions};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

struct TempTree(PathBuf);

impl TempTree {
    fn new(name: &str) -> TempTree {
        let dir = env::temp_dir().join(format!("glob-rooted-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempTree(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        // Best effort: a directory made unreadable by a test must be
        // restored before removal.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let denied = self.0.join("base").join("denied");
            let _ = fs::set_permissions(&denied, fs::Permissions::from_mode(0o755));
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink_dir(target: &Path, link: &Path) -> io::Result<()> {
    match std::os::windows::fs::symlink_dir(target, link) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Directory junctions need no special privilege and behave like
            // symlinks for this API (reparse point, resolved by
            // canonicalize), so fall back to them to still exercise the
            // link-following logic when symlink creation is denied.
            let junction = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output();
            match junction {
                Ok(out) if out.status.success() => Ok(()),
                _ => Err(e),
            }
        }
    }
}

/// Builds the fixture tree:
///
/// <temp>/glob-rooted-<name>-<pid>/
///   base/                 <- scan root
///     real/a.txt
///     real/inner/b.txt
///     plain.txt
///     alias1 -> real      (two aliases to the same internal directory)
///     alias2 -> real
///     ext    -> ../base_outside (external; name starts with root's name)
///     broken -> ./nonexistent
///     loop   -> .         (back to the root: an ancestor cycle)
///   base_outside/
///     out.txt
///
/// Returns None (after printing a note) when symlinks cannot be created,
/// e.g. on Windows without the required privilege.
fn build_link_tree(name: &str) -> Option<TempTree> {
    let tree = TempTree::new(name);
    let base = tree.path().join("base");
    let outside = tree.path().join("base_outside");
    fs::create_dir_all(base.join("real").join("inner")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(base.join("real").join("a.txt"), "a").unwrap();
    fs::write(base.join("real").join("inner").join("b.txt"), "b").unwrap();
    fs::write(base.join("plain.txt"), "p").unwrap();
    fs::write(outside.join("out.txt"), "o").unwrap();

    let links = [
        (base.join("real"), base.join("alias1")),
        (base.join("real"), base.join("alias2")),
        (outside.clone(), base.join("ext")),
        (base.join("nonexistent"), base.join("broken")),
        (base.clone(), base.join("loop")),
    ];
    for (target, link) in &links {
        if let Err(e) = symlink_dir(target, link) {
            eprintln!(
                "skipping symlink-dependent assertions: cannot create {:?} -> {:?}: {}",
                link, target, e
            );
            return None;
        }
    }
    Some(tree)
}

fn options(follow_links: FollowLinks, max_depth: Option<usize>) -> RootedOptions {
    RootedOptions {
        match_options: MatchOptions::new(),
        follow_links,
        max_depth,
    }
}

/// Collected scan output as comparable, displayable items.
fn collect(root: &Path, pattern: &str, opts: RootedOptions) -> Vec<String> {
    glob_rooted_with(root, pattern, opts)
        .unwrap()
        .map(|item| match item {
            Ok(path) => format!("ok:{}", path.display()),
            Err(e) => format!("err:{}:{:?}", e.path().display(), e.error().kind()),
        })
        .collect()
}

fn p(s: &str) -> String {
    let mut buf = PathBuf::new();
    for part in s.split('/') {
        buf.push(part);
    }
    format!("ok:{}", buf.display())
}

#[test]
fn never_follows_links() {
    let Some(tree) = build_link_tree("never") else {
        return;
    };
    let got = collect(
        &tree.path().join("base"),
        "**",
        options(FollowLinks::Never, None),
    );
    let expected = vec![
        p("alias1"),
        p("alias2"),
        p("broken"),
        p("ext"),
        p("loop"),
        p("plain.txt"),
        p("real"),
        p("real/a.txt"),
        p("real/inner"),
        p("real/inner/b.txt"),
    ];
    assert_eq!(got, expected);
}

#[test]
fn follows_links_within_root() {
    let Some(tree) = build_link_tree("within") else {
        return;
    };
    let got = collect(
        &tree.path().join("base"),
        "**",
        options(FollowLinks::WithinRoot, None),
    );
    let expected = vec![
        // Both aliases into the same real directory are scanned.
        p("alias1"),
        p("alias1/a.txt"),
        p("alias1/inner"),
        p("alias1/inner/b.txt"),
        p("alias2"),
        p("alias2/a.txt"),
        p("alias2/inner"),
        p("alias2/inner/b.txt"),
        // Broken link: distinguishable NotFound error item.
        "err:broken:NotFound".to_string(),
        // External link (sibling whose name starts with the root's name):
        // reported as a leaf, never descended into.
        p("ext"),
        // The ancestor cycle is cut with an error naming the path...
        "err:loop:Other".to_string(),
        // ...and the remaining siblings are still scanned.
        p("plain.txt"),
        p("real"),
        p("real/a.txt"),
        p("real/inner"),
        p("real/inner/b.txt"),
    ];
    assert_eq!(got, expected);
}

#[test]
fn loop_error_names_the_cut_path() {
    let Some(tree) = build_link_tree("loopmsg") else {
        return;
    };
    let errors: Vec<_> = glob_rooted_with(
        &tree.path().join("base"),
        "**",
        options(FollowLinks::WithinRoot, None),
    )
    .unwrap()
    .filter_map(|item| item.err())
    .filter(|e| e.error().kind() == io::ErrorKind::Other)
    .collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].path(), Path::new("loop"));
    assert!(errors[0].error().to_string().contains("loop"));
}

#[test]
fn external_link_target_is_not_scanned() {
    let Some(tree) = build_link_tree("external") else {
        return;
    };
    let got = collect(
        &tree.path().join("base"),
        "**",
        options(FollowLinks::WithinRoot, None),
    );
    // Nothing from base_outside (whose name starts with "base") leaks in.
    assert!(got.iter().all(|item| !item.contains("out.txt")));
    assert!(got.iter().all(|item| !item.contains("base_outside")));
}

#[test]
fn max_depth_counts_logical_components() {
    let Some(tree) = build_link_tree("depth") else {
        return;
    };
    let got = collect(
        &tree.path().join("base"),
        "**",
        options(FollowLinks::WithinRoot, Some(1)),
    );
    let expected = vec![
        p("alias1"),
        p("alias2"),
        "err:broken:NotFound".to_string(),
        p("ext"),
        "err:loop:Other".to_string(),
        p("plain.txt"),
        p("real"),
    ];
    assert_eq!(got, expected);
}

#[test]
fn non_matching_links_are_still_handled_per_policy() {
    let Some(tree) = build_link_tree("nonmatch") else {
        return;
    };
    // The link names do not match *.txt, but followed links are still
    // descended into so their matching contents are found.
    let got = collect(
        &tree.path().join("base"),
        "*.txt",
        options(FollowLinks::WithinRoot, None),
    );
    let expected = vec![
        p("alias1/a.txt"),
        p("alias1/inner/b.txt"),
        p("alias2/a.txt"),
        p("alias2/inner/b.txt"),
        "err:broken:NotFound".to_string(),
        "err:loop:Other".to_string(),
        p("plain.txt"),
        p("real/a.txt"),
        p("real/inner/b.txt"),
    ];
    assert_eq!(got, expected);

    // With Never, links are leaves: only real paths match.
    let got = collect(
        &tree.path().join("base"),
        "*.txt",
        options(FollowLinks::Never, None),
    );
    let expected = vec![p("plain.txt"), p("real/a.txt"), p("real/inner/b.txt")];
    assert_eq!(got, expected);
}

#[test]
fn missing_root_is_a_single_not_found_error() {
    let tree = TempTree::new("noroot");
    let missing = tree.path().join("does-not-exist");
    let mut iter = glob_rooted(&missing, "**").unwrap();
    match iter.next() {
        Some(Err(e)) => {
            assert_eq!(e.error().kind(), io::ErrorKind::NotFound);
            assert_eq!(e.path(), missing.as_path());
        }
        other => panic!("expected a single NotFound error, got {:?}", other),
    }
    assert!(iter.next().is_none());
}

#[test]
fn invalid_pattern_is_a_construction_error() {
    let tree = TempTree::new("badpat");
    assert!(glob_rooted(tree.path(), "a**b").is_err());
}

#[test]
fn match_options_semantics() {
    // No symlinks involved: runs everywhere.
    let tree = TempTree::new("options");
    let base = tree.path().join("base");
    fs::create_dir_all(base.join("sub")).unwrap();
    fs::write(base.join(".hidden"), "h").unwrap();
    fs::write(base.join("sub").join("File.TXT"), "f").unwrap();

    // Hidden files: require_literal_leading_dot skips dotfiles.
    let opts = RootedOptions {
        match_options: MatchOptions {
            case_sensitive: true,
            require_literal_separator: false,
            require_literal_leading_dot: true,
        },
        ..RootedOptions::new()
    };
    let got = collect(&base, "**", opts);
    assert_eq!(got, vec![p("sub"), p("sub/File.TXT")]);

    // Case-insensitive matching.
    let opts = RootedOptions {
        match_options: MatchOptions {
            case_sensitive: false,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        },
        ..RootedOptions::new()
    };
    let got = collect(&base, "*.txt", opts);
    assert_eq!(got, vec![p("sub/File.TXT")]);

    // require_literal_separator: * no longer crosses components.
    let opts = RootedOptions {
        match_options: MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        },
        ..RootedOptions::new()
    };
    let got = collect(&base, "*.TXT", opts);
    assert!(got.is_empty());
}

#[cfg(unix)]
#[test]
fn permission_error_is_reported_and_scan_continues() {
    use std::os::unix::fs::PermissionsExt;

    let tree = TempTree::new("denied");
    let base = tree.path().join("base");
    fs::create_dir_all(base.join("denied")).unwrap();
    fs::write(base.join("z.txt"), "z").unwrap();
    fs::set_permissions(base.join("denied"), fs::Permissions::from_mode(0o000)).unwrap();

    // Root (and some CI users) can read anything; skip rather than fake it.
    if fs::read_dir(base.join("denied")).is_ok() {
        eprintln!("skipping: directory is readable despite mode 000");
        return;
    }

    let got = collect(&base, "**", options(FollowLinks::Never, None));
    assert_eq!(
        got,
        vec![
            p("denied"),
            "err:denied:PermissionDenied".to_string(),
            p("z.txt"),
        ]
    );
}

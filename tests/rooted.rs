// Tests for the root-bounded recursive glob entry point (`glob_rooted`).
//
// All fixtures live in fresh temporary directories; no real user directory
// is ever touched. Symlink-dependent cases are skipped explicitly when the
// platform refuses to create symlinks (e.g. Windows without the privilege)
// instead of silently passing.

use std::fs;
use std::io;
use std::path::Path;

use glob::{
    glob_rooted, FollowLinks, MatchOptions, RootedErrorKind, RootedOptions,
};
use tempfile::TempDir;

fn try_symlink_dir(original: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(original, link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(original, link)
    }
}

fn symlinks_available(dir: &Path) -> bool {
    let probe = dir.join(".symlink-probe");
    match try_symlink_dir(Path::new("."), &probe) {
        Ok(()) => {
            let _ = fs::remove_dir(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Builds the shared fixture:
///
/// ```text
/// <tmp>/root/                 scan root
///   real/a.txt
///   real/sub/b.txt
///   plain/c.txt
///   alias1        -> real             (internal alias #1)
///   alias2        -> real             (internal alias #2, same target)
///   outside_link  -> ../root-external (escapes the root)
///   loop_link     -> .                (points back at an ancestor)
///   dangling      -> no-such-target   (broken)
/// <tmp>/root-external/        name shares a string prefix with "root"
///   secret.txt
/// ```
///
/// Returns `None` (and the test skips) when symlinks cannot be created.
fn build_fixture() -> Option<TempDir> {
    let tmp = tempfile::tempdir().unwrap();
    if !symlinks_available(tmp.path()) {
        eprintln!("skipping: symlink creation not permitted on this platform");
        return None;
    }
    let root = tmp.path().join("root");
    fs::create_dir_all(root.join("real/sub")).unwrap();
    fs::create_dir_all(root.join("plain")).unwrap();
    fs::write(root.join("real/a.txt"), "a").unwrap();
    fs::write(root.join("real/sub/b.txt"), "b").unwrap();
    fs::write(root.join("plain/c.txt"), "c").unwrap();

    let external = tmp.path().join("root-external");
    fs::create_dir_all(&external).unwrap();
    fs::write(external.join("secret.txt"), "s").unwrap();

    try_symlink_dir(Path::new("real"), &root.join("alias1")).unwrap();
    try_symlink_dir(Path::new("real"), &root.join("alias2")).unwrap();
    try_symlink_dir(Path::new("../root-external"), &root.join("outside_link")).unwrap();
    try_symlink_dir(Path::new("."), &root.join("loop_link")).unwrap();
    try_symlink_dir(Path::new("no-such-target"), &root.join("dangling")).unwrap();
    Some(tmp)
}

fn options(follow_links: FollowLinks) -> RootedOptions {
    RootedOptions {
        follow_links,
        ..RootedOptions::default()
    }
}

/// Collected scan output with separators normalized to `/` so expectations
/// read the same on every platform.
enum Item {
    Ok(String),
    Err(RootedErrorKind, String),
}

fn collect(root: &Path, pattern: &str, options: RootedOptions) -> Vec<Item> {
    glob_rooted(root, pattern, options)
        .unwrap()
        .map(|entry| match entry {
            Ok(path) => Item::Ok(normalize(&path)),
            Err(error) => Item::Err(error.kind(), normalize(error.path())),
        })
        .collect()
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn ok_paths(items: &[Item]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::Ok(path) => Some(path.as_str()),
            _ => None,
        })
        .collect()
}

fn assert_sequence(items: &[Item], expected: &[&str]) {
    let actual: Vec<String> = items
        .iter()
        .map(|item| match item {
            Item::Ok(path) => format!("ok:{}", path),
            Item::Err(kind, path) => format!("err:{:?}:{}", kind, path),
        })
        .collect();
    assert_eq!(actual, expected, "unexpected scan sequence");
}

#[test]
fn never_follow_treats_links_as_plain_entries() {
    let tmp = match build_fixture() {
        Some(tmp) => tmp,
        None => return,
    };
    let items = collect(&tmp.path().join("root"), "**", options(FollowLinks::Never));
    assert_sequence(
        &items,
        &[
            "ok:alias1",
            "ok:alias2",
            "ok:dangling",
            "ok:loop_link",
            "ok:outside_link",
            "ok:plain",
            "ok:plain/c.txt",
            "ok:real",
            "ok:real/a.txt",
            "ok:real/sub",
            "ok:real/sub/b.txt",
        ],
    );
}

#[test]
fn within_root_full_result_and_error_order() {
    let tmp = match build_fixture() {
        Some(tmp) => tmp,
        None => return,
    };
    let items = collect(
        &tmp.path().join("root"),
        "**",
        options(FollowLinks::WithinRoot),
    );
    // Both aliases of the same real directory produce their own results
    // (ancestor-chain loop detection, no global visited set). The broken
    // link and the truncated loop show up as distinguishable error items
    // naming their paths, and the scan still reaches the siblings that
    // sort after them (plain/, real/).
    assert_sequence(
        &items,
        &[
            "ok:alias1",
            "ok:alias1/a.txt",
            "ok:alias1/sub",
            "ok:alias1/sub/b.txt",
            "ok:alias2",
            "ok:alias2/a.txt",
            "ok:alias2/sub",
            "ok:alias2/sub/b.txt",
            "err:BrokenLink:dangling",
            "ok:loop_link",
            "err:Loop:loop_link",
            "ok:outside_link",
            "ok:plain",
            "ok:plain/c.txt",
            "ok:real",
            "ok:real/a.txt",
            "ok:real/sub",
            "ok:real/sub/b.txt",
        ],
    );
}

#[test]
fn prefix_sibling_is_not_inside_root() {
    let tmp = match build_fixture() {
        Some(tmp) => tmp,
        None => return,
    };
    let items = collect(
        &tmp.path().join("root"),
        "**",
        options(FollowLinks::WithinRoot),
    );
    let paths = ok_paths(&items);
    // "root-external" starts with the root's name as a string but is a
    // different component, so the link to it must not be followed.
    assert!(!paths.iter().any(|p| p.starts_with("outside_link/")));
    assert!(!paths.iter().any(|p| p.contains("secret.txt")));
    assert!(paths.contains(&"outside_link"));
}

#[test]
fn max_depth_counts_logical_components() {
    let tmp = match build_fixture() {
        Some(tmp) => tmp,
        None => return,
    };
    let root = tmp.path().join("root");

    let mut depth_one = options(FollowLinks::Never);
    depth_one.max_depth = Some(1);
    let items = collect(&root, "**", depth_one);
    assert_sequence(
        &items,
        &[
            "ok:alias1",
            "ok:alias2",
            "ok:dangling",
            "ok:loop_link",
            "ok:outside_link",
            "ok:plain",
            "ok:real",
        ],
    );

    let mut depth_two = options(FollowLinks::Never);
    depth_two.max_depth = Some(2);
    let items = collect(&root, "**", depth_two);
    let paths = ok_paths(&items);
    assert!(paths.contains(&"real/sub"));
    assert!(paths.contains(&"plain/c.txt"));
    assert!(!paths.contains(&"real/sub/b.txt"));
}

#[test]
fn links_that_do_not_match_are_still_traversed() {
    let tmp = match build_fixture() {
        Some(tmp) => tmp,
        None => return,
    };
    let items = collect(
        &tmp.path().join("root"),
        "**/b.txt",
        options(FollowLinks::WithinRoot),
    );
    // The aliases themselves do not match the pattern, yet the walk still
    // passes through them to reach matching contents.
    assert_eq!(
        ok_paths(&items),
        vec!["alias1/sub/b.txt", "alias2/sub/b.txt", "real/sub/b.txt"]
    );
}

#[test]
fn trailing_separator_requires_directory() {
    let tmp = match build_fixture() {
        Some(tmp) => tmp,
        None => return,
    };
    let items = collect(&tmp.path().join("root"), "*/", options(FollowLinks::Never));
    assert_sequence(
        &items,
        &[
            "ok:alias1",
            "ok:alias2",
            "ok:loop_link",
            "ok:outside_link",
            "ok:plain",
            "ok:real",
        ],
    );
}

#[test]
fn missing_root_is_a_distinct_error_item() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("missing");
    let mut paths = glob_rooted(&root, "**", RootedOptions::default()).unwrap();
    let first = paths.next().expect("expected one error item");
    let error = first.unwrap_err();
    assert_eq!(error.kind(), RootedErrorKind::RootNotFound);
    assert_eq!(error.path(), root.as_path());
    assert!(paths.next().is_none(), "iteration ends after the root error");
}

#[test]
fn hidden_files_follow_match_options() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    fs::create_dir_all(root.join(".hidden")).unwrap();
    fs::write(root.join(".hidden/h.txt"), "h").unwrap();
    fs::write(root.join("shown.txt"), "s").unwrap();

    // Default options: dot entries are visible, like glob_with defaults.
    let items = collect(&root, "**", RootedOptions::default());
    let paths = ok_paths(&items);
    assert!(paths.contains(&".hidden"));
    assert!(paths.contains(&".hidden/h.txt"));

    // require_literal_leading_dot hides them from wildcard traversal.
    let mut strict = RootedOptions::default();
    strict.match_options = MatchOptions {
        require_literal_leading_dot: true,
        ..MatchOptions::new()
    };
    let items = collect(&root, "**", strict);
    assert_eq!(ok_paths(&items), vec!["shown.txt"]);

    // A literal dot component in the pattern still reaches hidden entries.
    let items = collect(&root, ".hidden/*", strict);
    assert_eq!(ok_paths(&items), vec![".hidden/h.txt"]);
}

#[test]
fn case_sensitivity_follows_match_options() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("Visible.TXT"), "v").unwrap();

    let items = collect(&root, "*.txt", RootedOptions::default());
    assert!(ok_paths(&items).is_empty());

    let mut insensitive = RootedOptions::default();
    insensitive.match_options = MatchOptions {
        case_sensitive: false,
        ..MatchOptions::new()
    };
    let items = collect(&root, "*.txt", insensitive);
    assert_eq!(ok_paths(&items), vec!["Visible.TXT"]);
}

#[cfg(unix)]
#[test]
fn permission_error_is_distinct_and_scan_continues() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    fs::create_dir_all(root.join("locked")).unwrap();
    fs::create_dir_all(root.join("open")).unwrap();
    fs::write(root.join("locked/secret.txt"), "s").unwrap();
    fs::write(root.join("open/o.txt"), "o").unwrap();
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();

    if fs::read_dir(root.join("locked")).is_ok() {
        // e.g. running as root: cannot simulate the failure, skip honestly.
        let _ = fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o755));
        eprintln!("skipping: permission checks not enforced for this user");
        return;
    }

    let items = collect(&root, "**", RootedOptions::default());
    let _ = fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o755));

    let locked = items
        .iter()
        .find(|item| matches!(item, Item::Err(RootedErrorKind::PermissionDenied, p) if p == "locked"));
    assert!(locked.is_some(), "expected a PermissionDenied error for locked/");
    // The failure does not end the scan: later siblings are still returned.
    let paths = ok_paths(&items);
    assert!(paths.contains(&"open"));
    assert!(paths.contains(&"open/o.txt"));
}

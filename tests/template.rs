use glob::{PatternTemplate, TemplateErrorKind};

#[test]
fn acceptance_literal_name_with_metachars() {
    let template = PatternTemplate::new("src/{module}/**/{name}.rs").unwrap();

    // `a[1]*` comes from external input and must match literally.
    let pattern = template.bind(&[("module", "cli"), ("name", "a[1]*")]).unwrap();
    assert!(pattern.matches("src/cli/a[1]*.rs"));
    assert!(pattern.matches("src/cli/deep/nested/a[1]*.rs"));
    assert!(!pattern.matches("src/cli/a1xyz.rs"));
    assert!(!pattern.matches("src/cli/deep/a1xyz.rs"));
    assert!(!pattern.matches("src/cli/a1.rs"));
    assert!(!pattern.matches("src/cli/a[1]*.txt"));
}

#[test]
fn module_with_parent_dir_or_separator_is_rejected() {
    let template = PatternTemplate::new("src/{module}/**/{name}.rs").unwrap();

    for bad in ["../etc", "a/b", "a\\b", "..", "sub/../x"] {
        let err = template.bind(&[("module", bad), ("name", "x")]).unwrap_err();
        assert!(
            matches!(
                err.kind,
                TemplateErrorKind::ParentDirectory(_) | TemplateErrorKind::ContainsSeparator(_)
            ),
            "value {:?} should be rejected, got {:?}",
            bad,
            err
        );
    }

    // Empty values are rejected too.
    let err = template.bind(&[("module", ""), ("name", "x")]).unwrap_err();
    assert!(matches!(err.kind, TemplateErrorKind::EmptyValue(_)));
}

#[test]
fn template_reuse_does_not_leak_values_between_bindings() {
    let template = PatternTemplate::new("src/{module}/**/{name}.rs").unwrap();

    let first = template.bind(&[("module", "alpha"), ("name", "one")]).unwrap();
    let second = template.bind(&[("module", "beta"), ("name", "two")]).unwrap();

    // The first binding keeps its own values after the second bind.
    assert!(first.matches("src/alpha/one.rs"));
    assert!(!first.matches("src/beta/two.rs"));
    // The second binding contains no trace of the first one's values.
    assert!(second.matches("src/beta/two.rs"));
    assert!(!second.matches("src/alpha/one.rs"));
    // The compiled template itself is unchanged.
    assert_eq!(template.as_str(), "src/{module}/**/{name}.rs");
    assert_eq!(template.slots().collect::<Vec<_>>(), ["module", "name"]);
}

#[test]
fn repeated_slots_share_one_binding() {
    let template = PatternTemplate::new("{crate}/src/{crate}/{mod}.rs").unwrap();
    assert_eq!(template.slots().collect::<Vec<_>>(), ["crate", "mod"]);

    let pattern = template.bind(&[("crate", "foo"), ("mod", "lib")]).unwrap();
    assert!(pattern.matches("foo/src/foo/lib.rs"));
    assert!(!pattern.matches("foo/src/bar/lib.rs"));
}

#[test]
fn escaped_braces_are_literal() {
    // `{{` and `}}` produce literal braces; `{n}` is a real slot in between.
    let template = PatternTemplate::new("{{{n}}}").unwrap();
    assert_eq!(template.slots().collect::<Vec<_>>(), ["n"]);
    let pattern = template.bind(&[("n", "x")]).unwrap();
    assert!(pattern.matches("{x}"));
    assert!(!pattern.matches("x"));

    // Purely literal braces, no slots at all.
    let template = PatternTemplate::new("{{literal}}/*.rs").unwrap();
    assert_eq!(template.slots().count(), 0);
    let pattern = template.bind(&[]).unwrap();
    assert!(pattern.matches("{literal}/main.rs"));
    assert!(!pattern.matches("literal/main.rs"));

    // A lone `}` is an error.
    let err = PatternTemplate::new("a}b").unwrap_err();
    assert!(matches!(err.kind, TemplateErrorKind::UnmatchedCloseBrace));
    assert_eq!(err.pos, 1);
}

#[test]
fn error_positions_are_char_indices_with_multibyte_text() {
    // Each CJK character counts as one char; the slot starts at char index 3.
    let template = PatternTemplate::new("中文/{n}/*.rs").unwrap();
    let err = template.bind(&[]).unwrap_err();
    assert!(matches!(err.kind, TemplateErrorKind::MissingBinding(ref n) if n == "n"));
    assert_eq!(err.pos, 3);

    // Unclosed slot after multibyte prefix: `{` is at char index 2.
    let err = PatternTemplate::new("中文{ab").unwrap_err();
    assert!(matches!(err.kind, TemplateErrorKind::UnclosedSlot));
    assert_eq!(err.pos, 2);

    // Unicode slot names are fine.
    let template = PatternTemplate::new("{模块}/*.rs").unwrap();
    let pattern = template.bind(&[("模块", "核心")]).unwrap();
    assert!(pattern.matches("核心/lib.rs"));
}

#[test]
fn missing_and_extra_bindings_are_reported_before_generation() {
    let template = PatternTemplate::new("src/{module}/{name}.rs").unwrap();

    let err = template.bind(&[("module", "m")]).unwrap_err();
    assert!(matches!(err.kind, TemplateErrorKind::MissingBinding(ref n) if n == "name"));
    assert_eq!(err.pos, 13);

    let err = template
        .bind(&[("module", "m"), ("name", "n"), ("unknown", "x")])
        .unwrap_err();
    assert!(matches!(err.kind, TemplateErrorKind::ExtraBinding(ref n) if n == "unknown"));
}

#[test]
fn slot_inside_char_class_is_rejected() {
    let err = PatternTemplate::new("src/[{mod}].rs").unwrap_err();
    assert!(matches!(err.kind, TemplateErrorKind::SlotInCharClass));
    assert_eq!(err.pos, 5);

    // Braces that do not form a slot are ordinary class members.
    let template = PatternTemplate::new("[{}]/{n}").unwrap();
    let pattern = template.bind(&[("n", "x")]).unwrap();
    assert!(pattern.matches("{/x"));
    assert!(pattern.matches("}/x"));
}

#[test]
fn binding_cannot_widen_wildcard_structure() {
    let template = PatternTemplate::new("logs/{name}.txt").unwrap();

    // `*` and `?` in the value must not act as wildcards.
    let pattern = template.bind(&[("name", "*")]).unwrap();
    assert!(pattern.matches("logs/*.txt"));
    assert!(!pattern.matches("logs/anything.txt"));

    let pattern = template.bind(&[("name", "a?c")]).unwrap();
    assert!(pattern.matches("logs/a?c.txt"));
    assert!(!pattern.matches("logs/abc.txt"));
}

#[test]
fn invalid_slot_syntax() {
    let err = PatternTemplate::new("src/{}").unwrap_err();
    assert!(matches!(err.kind, TemplateErrorKind::EmptySlotName));

    let err = PatternTemplate::new("src/{1st}").unwrap_err();
    assert!(matches!(err.kind, TemplateErrorKind::InvalidSlotName(_)));

    let err = PatternTemplate::new("src/{mod").unwrap_err();
    assert!(matches!(err.kind, TemplateErrorKind::UnclosedSlot));
}

#[test]
fn invalid_glob_in_literal_part_is_reported_on_bind() {
    let template = PatternTemplate::new("src/{name}/[").unwrap();
    let err = template.bind(&[("name", "x")]).unwrap_err();
    assert!(matches!(err.kind, TemplateErrorKind::InvalidPattern(_)));
}

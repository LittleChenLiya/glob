// Copyright 2024 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Path remapping planner.
//!
//! This module provides [`RemapPlanner`], which maps source relative paths to
//! target relative paths according to an ordered list of rules. Each rule
//! pairs a source glob pattern (which may contain named captures) with a
//! target template that references those captures.
//!
//! # Source patterns
//!
//! A source pattern is a Unix shell style glob with two extra constructs:
//!
//! - `{name:*}` is a **single-segment capture**. It matches a (possibly empty)
//!   run of characters that does not cross a path separator, and records it
//!   under `name`.
//! - `{name:**}` is a **cross-segment capture**. It matches zero or more path
//!   segments (including the separators between them) and records them under
//!   `name`. A `**` capture must occupy an entire path segment, just like a
//!   plain `**` glob.
//!
//! All other characters are interpreted as ordinary glob syntax. The source
//! pattern is compiled into a [`Pattern`] (with captures replaced by `*` /
//! `**`) and every match is re-verified against it.
//!
//! # Target templates
//!
//! A target template is a literal path that may reference captures with
//! `{name}`. Only captures defined by the source rule may be referenced; an
//! undefined capture is an error.
//!
//! # Matching and disambiguation
//!
//! Rules are tried in declaration order; the first rule whose source pattern
//! matches wins. When a `**` capture could span different numbers of segments,
//! the leftmost such capture is greedy: it takes the longest possible span.
//!
//! # Safety
//!
//! Rendered targets are checked against a safety boundary: they must be
//! relative, must not escape the root through `..`, must not contain
//! characters reserved by the platform, and must not be empty. Two distinct
//! source paths that would map to the same target are rejected.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::path::{self, Component, Path, PathBuf};

use crate::{
    in_char_specifiers, parse_char_specifiers, CharSpecifier, MatchOptions, Pattern, PatternError,
};

/// The kind of a named capture in a source glob rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureKind {
    /// A single-segment capture, written `{name:*}`.
    Single,
    /// A cross-segment (recursive) capture, written `{name:**}`.
    Recursive,
}

/// A named capture extracted from a source path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// The name of the capture.
    pub name: String,
    /// The kind of capture.
    pub kind: CaptureKind,
}

/// A remapping rule: a source glob pattern with named captures paired with a
/// target template that references those captures.
///
/// # Examples
///
/// ```
/// use glob::remap::RemapRule;
///
/// let rule = RemapRule {
///     source: "src/{dir:**}/{name:*}.rs".to_string(),
///     target: "lib/{dir}/{name}.txt".to_string(),
/// };
/// ```
#[derive(Debug, Clone)]
pub struct RemapRule {
    /// The source glob pattern, e.g. `src/{dir:**}/{name:*}.rs`.
    pub source: String,
    /// The target template, e.g. `lib/{dir}/{name}.txt`.
    pub target: String,
}

/// A single planned mapping from a source relative path to a target relative
/// path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    /// The source relative path that was matched.
    pub source: PathBuf,
    /// The target relative path produced by the matching rule.
    pub target: PathBuf,
    /// The index of the rule (in declaration order) that produced this mapping.
    pub rule: usize,
}

/// The reason a rendered target path was rejected as unsafe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsafeReason {
    /// The target is absolute (has a root or prefix component).
    Absolute,
    /// The target escapes the root through a `..` component.
    ParentDir,
    /// The target is empty.
    Empty,
    /// The target contains a character reserved by the platform.
    ReservedChar(char),
    /// The target ends in a dot or a space (a Windows path quirk).
    TrailingDotOrSpace,
}

/// An error encountered while building or running a remap planner.
#[derive(Debug)]
pub enum RemapError {
    /// A source glob pattern failed to compile.
    InvalidPattern(PatternError),
    /// A target template references a capture that the source rule does not
    /// define.
    UndefinedCapture {
        /// The index of the offending rule.
        rule: usize,
        /// The capture name that was referenced but not defined.
        name: String,
    },
    /// A target template is malformed.
    InvalidTemplate {
        /// The index of the offending rule.
        rule: usize,
        /// A message describing the problem.
        msg: &'static str,
    },
    /// A rendered target path is not safe.
    UnsafeTarget {
        /// The index of the rule that produced the target.
        rule: usize,
        /// The rendered target string.
        target: String,
        /// Why the target was rejected.
        reason: UnsafeReason,
    },
    /// Two distinct source paths map to the same target.
    DuplicateTarget {
        /// The shared target.
        target: PathBuf,
        /// The first source that mapped to the target.
        first: PathBuf,
        /// The second source that mapped to the target.
        second: PathBuf,
    },
    /// A source path is not relative (it has a root or prefix).
    AbsoluteSource(PathBuf),
    /// Internal error: the compiled [`Pattern`] did not agree with the
    /// capture extractor.
    PatternMismatch {
        /// The source path that failed verification.
        source: PathBuf,
        /// The source pattern that did not match.
        pattern: String,
    },
}

impl fmt::Display for RemapError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RemapError::InvalidPattern(e) => write!(f, "invalid source pattern: {}", e),
            RemapError::UndefinedCapture { rule, name } => {
                write!(f, "rule {} references undefined capture `{}`", rule, name)
            }
            RemapError::InvalidTemplate { rule, msg } => {
                write!(f, "rule {} has invalid target template: {}", rule, msg)
            }
            RemapError::UnsafeTarget { rule, target, reason } => {
                write!(
                    f,
                    "rule {} produced unsafe target `{}`: {:?}",
                    rule, target, reason
                )
            }
            RemapError::DuplicateTarget {
                target,
                first,
                second,
            } => {
                write!(
                    f,
                    "sources `{}` and `{}` both map to target `{}`",
                    first.display(),
                    second.display(),
                    target.display()
                )
            }
            RemapError::AbsoluteSource(p) => {
                write!(f, "source `{}` is not a relative path", p.display())
            }
            RemapError::PatternMismatch { source, pattern } => {
                write!(
                    f,
                    "source `{}` did not match pattern `{}` during verification",
                    source.display(),
                    pattern
                )
            }
        }
    }
}

impl Error for RemapError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            RemapError::InvalidPattern(e) => Some(e),
            _ => None,
        }
    }
}

impl From<PatternError> for RemapError {
    fn from(e: PatternError) -> Self {
        RemapError::InvalidPattern(e)
    }
}

/// A compiled rule: the source pattern split into segments and the parsed
/// target template.
#[derive(Debug)]
struct CompiledRule {
    /// The source pattern with captures replaced by `*` / `**`, used for
    /// verification.
    source_glob: Pattern,
    /// The source pattern split into path segments.
    source_segments: Vec<SourceSegment>,
    /// The parsed target template.
    target: Vec<TemplateToken>,
}

/// A segment of a source pattern.
#[derive(Debug)]
enum SourceSegment {
    /// A whole-segment cross-segment capture `{name:**}`.
    Recursive(String),
    /// A shell-like segment: literal glob chars mixed with `*` captures.
    Shell(Vec<ShellToken>),
}

/// A token within a shell segment of a source pattern.
#[derive(Debug)]
enum ShellToken {
    /// A literal character (ordinary glob syntax).
    Literal(char),
    /// A single-segment capture `{name:*}`.
    Capture(String),
    /// A `[...]` character class. The bool is `true` for negation (`[!...]`).
    CharClass(bool, Vec<CharSpecifier>),
}

/// A token of a target template.
#[derive(Debug)]
enum TemplateToken {
    /// A literal string.
    Literal(String),
    /// A reference to a capture `{name}`.
    Capture(String),
}

/// A planner that remaps source relative paths to target relative paths.
///
/// See the [module documentation](self) for details on the pattern and
/// template syntax.
#[derive(Debug)]
pub struct RemapPlanner {
    rules: Vec<CompiledRule>,
    options: MatchOptions,
}

impl RemapPlanner {
    /// Create a planner from an ordered list of rules.
    ///
    /// Rules are tried in declaration order; the first matching rule wins.
    /// Matching uses platform-default case sensitivity (case-insensitive on
    /// Windows, case-sensitive elsewhere) and treats `*` as single-segment.
    pub fn new(rules: Vec<RemapRule>) -> Result<Self, RemapError> {
        Self::with_options(
            rules,
            MatchOptions {
                case_sensitive: cfg!(not(windows)),
                require_literal_separator: true,
                require_literal_leading_dot: false,
            },
        )
    }

    /// Create a planner with explicit match options.
    ///
    /// `require_literal_separator` is always forced to `true`, because `*`
    /// captures are single-segment by definition. `case_sensitive` controls
    /// whether literal characters are compared case-sensitively.
    pub fn with_options(
        rules: Vec<RemapRule>,
        options: MatchOptions,
    ) -> Result<Self, RemapError> {
        let options = MatchOptions {
            require_literal_separator: true,
            ..options
        };

        let mut compiled = Vec::with_capacity(rules.len());
        for (i, rule) in rules.iter().enumerate() {
            let (source_glob, source_segments, captures) = parse_source(&rule.source)?;
            let defined: HashSet<String> = captures.iter().map(|c| c.name.clone()).collect();
            let target = parse_target(&rule.target, i, &defined)?;
            compiled.push(CompiledRule {
                source_glob,
                source_segments,
                target,
            });
        }

        Ok(RemapPlanner {
            rules: compiled,
            options,
        })
    }

    /// Build the complete plan: for each source relative path, find the first
    /// matching rule and render its target.
    ///
    /// Sources that match no rule are skipped. An error is returned if a
    /// source is absolute, if a rendered target is unsafe, if two sources map
    /// to the same target, or if the compiled pattern disagrees with the
    /// capture extractor.
    pub fn plan(&self, sources: &[PathBuf]) -> Result<Vec<Mapping>, RemapError> {
        let mut mappings = Vec::new();
        // Normalized target key -> (rule index, source). Used for duplicate
        // detection.
        let mut seen: HashMap<String, (usize, PathBuf)> = HashMap::new();

        for source in sources {
            // A source that is rooted (has a root or prefix component) is not
            // a relative path, even on Windows where `\foo` is "drive
            // relative" rather than strictly absolute.
            let rooted = source
                .components()
                .any(|c| matches!(c, Component::RootDir | Component::Prefix(_)));
            if rooted {
                return Err(RemapError::AbsoluteSource(source.clone()));
            }

            let matched = self.rules.iter().enumerate().find_map(|(i, rule)| {
                extract_captures(rule, source, self.options).map(|caps| (i, rule, caps))
            });

            let (i, rule, caps) = match matched {
                Some(m) => m,
                None => continue,
            };

            // Re-verify the match using the existing Pattern semantics.
            if !rule
                .source_glob
                .matches_path_with(source, self.options)
            {
                return Err(RemapError::PatternMismatch {
                    source: source.clone(),
                    pattern: rule.source_glob.as_str().to_string(),
                });
            }

            let target_str = render_target(&rule.target, &caps);
            let target = check_target(&target_str, i)?;

            let key = target_key(&target, self.options.case_sensitive);
            if let Some((_, prev_source)) = seen.get(&key) {
                return Err(RemapError::DuplicateTarget {
                    target,
                    first: prev_source.clone(),
                    second: source.clone(),
                });
            }
            seen.insert(key, (i, source.clone()));

            mappings.push(Mapping {
                source: source.clone(),
                target,
                rule: i,
            });
        }

        Ok(mappings)
    }
}

// ---------------------------------------------------------------------------
// Source pattern parsing
// ---------------------------------------------------------------------------

/// Parse a source pattern into a compiled glob, its segments, and its captures.
fn parse_source(
    pattern: &str,
) -> Result<(Pattern, Vec<SourceSegment>, Vec<Capture>), RemapError> {
    let mut segments = Vec::new();
    let mut captures = Vec::new();
    let mut glob_str = String::new();

    for (si, seg) in pattern.split(path::is_separator).enumerate() {
        if si > 0 {
            glob_str.push('/');
        }

        if seg.is_empty() {
            return Err(RemapError::InvalidPattern(PatternError {
                pos: 0,
                msg: "source pattern contains an empty path segment",
            }));
        }

        // A `**` capture must occupy an entire segment.
        if let Some(inner) = seg
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
        {
            if let Some(name) = inner.strip_suffix(":**") {
                validate_name(name)?;
                if captures.iter().any(|c: &Capture| c.name == name) {
                    return Err(RemapError::InvalidPattern(PatternError {
                        pos: 0,
                        msg: "duplicate capture name in source pattern",
                    }));
                }
                captures.push(Capture {
                    name: name.to_string(),
                    kind: CaptureKind::Recursive,
                });
                segments.push(SourceSegment::Recursive(name.to_string()));
                glob_str.push_str("**");
                continue;
            }
        }

        // A shell segment: literals mixed with `*` captures.
        let mut toks = Vec::new();
        let mut glob_seg = String::new();
        let mut chars = seg.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '{' {
                let mut inner = String::new();
                let mut closed = false;
                while let Some(&pc) = chars.peek() {
                    if pc == '}' {
                        chars.next();
                        closed = true;
                        break;
                    }
                    inner.push(pc);
                    chars.next();
                }
                if !closed {
                    return Err(RemapError::InvalidPattern(PatternError {
                        pos: 0,
                        msg: "unclosed capture `{` in source pattern",
                    }));
                }
                if let Some(name) = inner.strip_suffix(":*") {
                    validate_name(name)?;
                    if captures.iter().any(|c: &Capture| c.name == name) {
                        return Err(RemapError::InvalidPattern(PatternError {
                            pos: 0,
                            msg: "duplicate capture name in source pattern",
                        }));
                    }
                    captures.push(Capture {
                        name: name.to_string(),
                        kind: CaptureKind::Single,
                    });
                    toks.push(ShellToken::Capture(name.to_string()));
                    glob_seg.push('*');
                } else if inner.strip_suffix(":**").is_some() {
                    return Err(RemapError::InvalidPattern(PatternError {
                        pos: 0,
                        msg: "`**` capture must form a single path component",
                    }));
                } else {
                    return Err(RemapError::InvalidPattern(PatternError {
                        pos: 0,
                        msg: "malformed capture in source pattern",
                    }));
                }
            } else if c == '[' {
                // A `[...]` character class.
                let (negated, specifiers) = parse_char_class(&mut chars)?;
                glob_seg.push('[');
                // Reconstruct the class for the glob string.
                if negated {
                    glob_seg.push('!');
                }
                for s in &specifiers {
                    match s {
                        CharSpecifier::SingleChar(ch) => glob_seg.push(*ch),
                        CharSpecifier::CharRange(a, b) => {
                            glob_seg.push(*a);
                            glob_seg.push('-');
                            glob_seg.push(*b);
                        }
                    }
                }
                glob_seg.push(']');
                toks.push(ShellToken::CharClass(negated, specifiers));
            } else {
                toks.push(ShellToken::Literal(c));
                glob_seg.push(c);
            }
        }
        segments.push(SourceSegment::Shell(toks));
        glob_str.push_str(&glob_seg);
    }

    let compiled = Pattern::new(&glob_str)?;
    Ok((compiled, segments, captures))
}

/// Validate a capture name: non-empty, alphanumeric or `_`, not starting with
/// a digit.
fn validate_name(name: &str) -> Result<(), RemapError> {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => {
            return Err(RemapError::InvalidPattern(PatternError {
                pos: 0,
                msg: "capture name must be a valid identifier",
            }))
        }
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(RemapError::InvalidPattern(PatternError {
            pos: 0,
            msg: "capture name must be a valid identifier",
        }));
    }
    Ok(())
}

/// Parse a `[...]` character class. The leading `[` has already been
/// consumed. Returns `(negated, specifiers)`.
fn parse_char_class(
    chars: &mut std::iter::Peekable<std::str::Chars>,
) -> Result<(bool, Vec<CharSpecifier>), RemapError> {
    let negated = if chars.peek() == Some(&'!') {
        chars.next();
        true
    } else {
        false
    };

    let mut content = Vec::new();
    let mut closed = false;
    while let Some(&c) = chars.peek() {
        if c == ']' && !content.is_empty() {
            chars.next();
            closed = true;
            break;
        }
        content.push(c);
        chars.next();
    }

    if !closed {
        return Err(RemapError::InvalidPattern(PatternError {
            pos: 0,
            msg: "unclosed bracket in source pattern",
        }));
    }

    Ok((negated, parse_char_specifiers(&content)))
}

// ---------------------------------------------------------------------------
// Target template parsing
// ---------------------------------------------------------------------------

/// Parse a target template, checking that every capture it references is
/// defined by the source rule.
fn parse_target(
    template: &str,
    rule: usize,
    defined: &HashSet<String>,
) -> Result<Vec<TemplateToken>, RemapError> {
    let mut toks = Vec::new();
    let mut literal = String::new();
    let mut chars = template.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '{' {
            if !literal.is_empty() {
                toks.push(TemplateToken::Literal(std::mem::take(&mut literal)));
            }
            let mut name = String::new();
            let mut closed = false;
            while let Some(&pc) = chars.peek() {
                if pc == '}' {
                    chars.next();
                    closed = true;
                    break;
                }
                name.push(pc);
                chars.next();
            }
            if !closed {
                return Err(RemapError::InvalidTemplate {
                    rule,
                    msg: "unclosed `{` in target template",
                });
            }
            if name.is_empty() {
                return Err(RemapError::InvalidTemplate {
                    rule,
                    msg: "empty capture name in target template",
                });
            }
            if !defined.contains(&name) {
                return Err(RemapError::UndefinedCapture { rule, name });
            }
            toks.push(TemplateToken::Capture(name));
        } else if c == '}' {
            return Err(RemapError::InvalidTemplate {
                rule,
                msg: "unexpected `}` in target template",
            });
        } else {
            literal.push(c);
        }
    }

    if !literal.is_empty() {
        toks.push(TemplateToken::Literal(literal));
    }

    Ok(toks)
}

// ---------------------------------------------------------------------------
// Capture extraction
// ---------------------------------------------------------------------------

/// Try to match `source` against `rule`, extracting captures on success.
fn extract_captures(
    rule: &CompiledRule,
    source: &Path,
    options: MatchOptions,
) -> Option<HashMap<String, String>> {
    let mut segments: Vec<&str> = Vec::new();
    for comp in source.components() {
        match comp {
            Component::Normal(s) => segments.push(s.to_str()?),
            Component::CurDir => {}
            // Absolute paths and paths that escape the root are not remapped.
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return None,
        }
    }

    let mut caps = HashMap::new();
    if match_segments(
        &rule.source_segments,
        &segments,
        options,
        &mut caps,
    ) {
        Some(caps)
    } else {
        None
    }
}

/// Match pattern segments against path segments, extracting captures.
///
/// `**` captures are greedy (they take the longest possible span), which
/// gives deterministic results when several assignments are possible.
fn match_segments(
    pat: &[SourceSegment],
    path: &[&str],
    options: MatchOptions,
    caps: &mut HashMap<String, String>,
) -> bool {
    match pat.first() {
        None => path.is_empty(),
        Some(SourceSegment::Recursive(name)) => {
            for k in (0..=path.len()).rev() {
                let mut trial = caps.clone();
                trial.insert(name.clone(), path[..k].join("/"));
                if match_segments(&pat[1..], &path[k..], options, &mut trial) {
                    *caps = trial;
                    return true;
                }
            }
            false
        }
        Some(SourceSegment::Shell(toks)) => {
            if let Some(seg) = path.first() {
                let mut trial = caps.clone();
                if match_shell(toks, seg, options, &mut trial)
                    && match_segments(&pat[1..], &path[1..], options, &mut trial)
                {
                    *caps = trial;
                    return true;
                }
            }
            false
        }
    }
}

/// Match a shell segment (literals, char classes, and `*` captures) against a
/// single path segment.
///
/// `*` captures are greedy (they take the longest possible run).
fn match_shell(
    toks: &[ShellToken],
    s: &str,
    options: MatchOptions,
    caps: &mut HashMap<String, String>,
) -> bool {
    match toks.first() {
        None => s.is_empty(),
        Some(ShellToken::Literal(c)) => {
            if s.is_empty() {
                return false;
            }
            let sc = s.chars().next().unwrap();
            if chars_eq(sc, *c, options.case_sensitive) {
                match_shell(&toks[1..], &s[sc.len_utf8()..], options, caps)
            } else {
                false
            }
        }
        Some(ShellToken::CharClass(negated, specifiers)) => {
            if s.is_empty() {
                return false;
            }
            let sc = s.chars().next().unwrap();
            let within = in_char_specifiers(specifiers, sc, options);
            let matched = if *negated { !within } else { within };
            if matched {
                match_shell(&toks[1..], &s[sc.len_utf8()..], options, caps)
            } else {
                false
            }
        }
        Some(ShellToken::Capture(name)) => {
            for k in (0..=s.len()).rev() {
                if s.is_char_boundary(k) {
                    let mut trial = caps.clone();
                    trial.insert(name.clone(), s[..k].to_string());
                    if match_shell(&toks[1..], &s[k..], options, &mut trial) {
                        *caps = trial;
                        return true;
                    }
                }
            }
            false
        }
    }
}

/// Compare two characters, optionally case-insensitively (ASCII only).
fn chars_eq(a: char, b: char, case_sensitive: bool) -> bool {
    if !case_sensitive && a.is_ascii() && b.is_ascii() {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

// ---------------------------------------------------------------------------
// Target rendering and safety
// ---------------------------------------------------------------------------

/// Render a target template by substituting captures.
fn render_target(toks: &[TemplateToken], caps: &HashMap<String, String>) -> String {
    let mut out = String::new();
    for tok in toks {
        match tok {
            TemplateToken::Literal(s) => out.push_str(s),
            TemplateToken::Capture(name) => {
                out.push_str(caps.get(name).expect("capture defined and matched"));
            }
        }
    }
    out
}

/// Characters reserved by Windows (and conservative cross-platform safety).
fn is_reserved(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (c as u32) < 32
}

/// Rendered-target safety: relative, no escape, no reserved chars, not empty.
fn check_target(target: &str, rule: usize) -> Result<PathBuf, RemapError> {
    for c in target.chars() {
        if is_reserved(c) {
            return Err(RemapError::UnsafeTarget {
                rule,
                target: target.to_string(),
                reason: UnsafeReason::ReservedChar(c),
            });
        }
    }

    if target.ends_with('.') || target.ends_with(' ') {
        return Err(RemapError::UnsafeTarget {
            rule,
            target: target.to_string(),
            reason: UnsafeReason::TrailingDotOrSpace,
        });
    }

    let path = PathBuf::from(target);

    let mut depth = 0u64;
    for comp in path.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => {
                return Err(RemapError::UnsafeTarget {
                    rule,
                    target: target.to_string(),
                    reason: UnsafeReason::Absolute,
                });
            }
            Component::ParentDir => {
                return Err(RemapError::UnsafeTarget {
                    rule,
                    target: target.to_string(),
                    reason: UnsafeReason::ParentDir,
                });
            }
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
        }
    }

    if depth == 0 {
        return Err(RemapError::UnsafeTarget {
            rule,
            target: target.to_string(),
            reason: UnsafeReason::Empty,
        });
    }

    Ok(path.components().collect::<PathBuf>())
}

/// Build a comparison key for a target path. The key is built from the
/// normalized components joined by `/`, and is lowercased when matching is
/// case-insensitive (so `lib/A` and `lib/a` are the same target on Windows).
fn target_key(target: &Path, case_sensitive: bool) -> String {
    let key = target
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    if case_sensitive {
        key
    } else {
        key.to_lowercase()
    }
}

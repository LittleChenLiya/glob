//! Path remapping planner.
//!
//! This module builds *plans* that rewrite source-relative paths into
//! target-relative paths. A [`RemapRule`] pairs a **source pattern** with a
//! **target template**:
//!
//! * the source pattern is a glob in which only the single-component wildcard
//!   `*` and the cross-component wildcard `**` may be used, and both forms can
//!   be named with braces: `{name:*}` and `{dir:**}`. The named captures are
//!   recorded for every concrete source path that matches;
//! * the target template may only reference captures declared by its own
//!   source pattern via `{name}`. Literal `{` and `}` are written as `{{` and
//!   `}}`.
//!
//! Rules are evaluated **in declaration order** and the first match wins.
//! When a pattern can match a path in more than one way the captures are
//! disambiguated by the **longest cross-component (`**`) capture first, then
//! the leftmost capture** (the capture with the smallest start offset among
//! the remaining `**` captures, and so on for `*`).
//!
//! Every chosen match is re-validated with the crate's existing [`Pattern`]
//! semantics (with separators required to be literal) before it is accepted.
//!
//! The generated target is finally checked as a path itself: empty captures,
//! path traversal, reserved characters and platform specific device names are
//! rejected, and a whole batch is rejected if two distinct sources would be
//! written to the same target.
//!
//! # Example
//!
//! ```rust
//! use glob::remap::{RemapPlanner, RemapRule, TargetSafety};
//!
//! let planner = RemapPlanner::new(TargetSafety::Posix)
//!     .add_rule(
//!         RemapRule::new("src/{dir:**}/{name:*}.rs", "lib/{dir}/{name}.txt")
//!             .unwrap(),
//!     )
//!     .unwrap();
//!
//! let plan = planner.plan_all(["src/a/b/c/main.rs"]).unwrap();
//! assert_eq!(plan[0].target, "lib/a/b/c/main.txt");
//! ```

use std::cmp::Reverse;
use std::error::Error;
use std::fmt;
use std::path::Path;

use crate::{MatchOptions, Pattern};

/// Which platform's path rules are used to validate generated target paths.
///
/// [`TargetSafety::host`] selects the platform the code is compiled for, while
/// the other variants make planning deterministic on every host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TargetSafety {
    /// NUL bytes and path traversal are rejected; `\` is an ordinary
    /// file-name character.
    Posix,
    /// Conservative Windows rules: reserved characters (`\ : * ? " < > |`),
    /// control characters, trailing dots/spaces and reserved device names
    /// such as `CON`, `NUL` or `AUX` are rejected for every component, and
    /// targets differing only in ASCII case are treated as the same path.
    Windows,
}

impl TargetSafety {
    /// The safety profile of the platform the code is compiled for.
    pub fn host() -> TargetSafety {
        if cfg!(windows) {
            TargetSafety::Windows
        } else {
            TargetSafety::Posix
        }
    }

    fn separators(self) -> &'static [char] {
        match self {
            TargetSafety::Posix => &['/'],
            TargetSafety::Windows => &['/', '\\'],
        }
    }

    fn case_insensitive(self) -> bool {
        matches!(self, TargetSafety::Windows)
    }
}

/// A single source pattern / target template rule.
#[derive(Clone, Debug)]
pub struct RemapRule {
    src: String,
    tokens: Vec<SourceToken>,
    captures: Vec<CaptureInfo>,
    plain: String,
    dst: String,
    dst_tokens: Vec<TargetToken>,
}

#[derive(Clone, Debug)]
enum SourceToken {
    Literal(char),
    Separator,
    Single(usize),
    Recursive(usize),
}

#[derive(Clone, Debug)]
struct CaptureInfo {
    name: String,
    recursive: bool,
}

#[derive(Clone, Debug)]
enum TargetToken {
    Literal(char),
    Capture(usize),
}

/// An error while constructing a [`RemapRule`] / [`RemapPlanner`] or while
/// planning a batch of sources.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemapError {
    /// 1-based rule index within the planner, or 0 for an error that is not
    /// tied to a rule.
    pub rule: usize,
    /// Kind of the error.
    pub kind: RemapErrorKind,
}

/// The specific reason a remapping operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RemapErrorKind {
    /// The source pattern is malformed. Message and approximate byte
    /// position.
    InvalidSource(String, usize),
    /// The target template references a capture the source does not declare.
    /// Carries the unknown name and approximate position.
    UndefinedCapture(String, usize),
    /// The target template itself is malformed.
    InvalidTarget(String, usize),
    /// No rule matches the given source path.
    NoRule,
    /// A capture matched the empty string; targets never contain empty
    /// components. Carries the capture name.
    EmptyCapture(String),
    /// The generated target escapes the relative root (`..` component).
    PathTraversal,
    /// The generated target is absolute.
    AbsoluteTarget,
    /// A target component contains reserved characters. Carries the
    /// offending component.
    ReservedCharacter(String),
    /// A target component is a reserved Windows device name.
    ReservedDeviceName(String),
    /// Two distinct source paths would be written to the same target.
    TargetConflict {
        /// The current source that produced the colliding target.
        source: String,
        /// The earlier source that already claimed the target.
        other_source: String,
        /// The shared target path.
        target: String,
    },
}

impl fmt::Display for RemapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            RemapErrorKind::InvalidSource(m, pos) => {
                write!(f, "invalid source pattern near position {}: {}", pos, m)
            }
            RemapErrorKind::UndefinedCapture(name, pos) => write!(
                f,
                "target template references undefined capture `{}` near position {}",
                name, pos
            ),
            RemapErrorKind::InvalidTarget(m, pos) => {
                write!(f, "invalid target template near position {}: {}", pos, m)
            }
            RemapErrorKind::NoRule => f.write_str("no remap rule matches the source path"),
            RemapErrorKind::EmptyCapture(name) => {
                write!(f, "capture `{}` matched an empty string", name)
            }
            RemapErrorKind::PathTraversal => {
                f.write_str("target path escapes the relative root (`..` component)")
            }
            RemapErrorKind::AbsoluteTarget => f.write_str("target path must be relative"),
            RemapErrorKind::ReservedCharacter(c) => {
                write!(f, "target component `{}` contains a reserved character", c)
            }
            RemapErrorKind::ReservedDeviceName(c) => {
                write!(f, "target component `{}` is a reserved device name", c)
            }
            RemapErrorKind::TargetConflict {
                source,
                other_source,
                target,
            } => write!(
                f,
                "sources `{}` and `{}` both map to target `{}`",
                source, other_source, target
            ),
        }
    }
}

impl Error for RemapError {}

fn err(rule: usize, kind: RemapErrorKind) -> RemapError {
    RemapError { rule, kind }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

fn is_ident_cont(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

/// Whether a character separates path components on the current host. Mirrors
/// `std::path::is_separator` (`\` only counts on Windows).
fn is_separator(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

impl RemapRule {
    /// Compile a source pattern / target template pair.
    pub fn new(src: &str, dst: &str) -> Result<RemapRule, RemapError> {
        let (tokens, captures, plain) = parse_source(src)?;
        let dst_tokens = parse_target(dst, &captures)?;
        Ok(RemapRule {
            src: src.to_string(),
            tokens,
            captures,
            plain,
            dst: dst.to_string(),
            dst_tokens,
        })
    }

    /// The original source pattern.
    pub fn source(&self) -> &str {
        &self.src
    }

    /// The original target template.
    pub fn target_template(&self) -> &str {
        &self.dst
    }

    /// Names of the captures declared by the source pattern, in order.
    pub fn capture_names(&self) -> impl Iterator<Item = &str> {
        self.captures.iter().map(|c| c.name.as_str())
    }
}

fn parse_source(src: &str) -> Result<(Vec<SourceToken>, Vec<CaptureInfo>, String), RemapError> {
    if src.is_empty() {
        return Err(err(0, RemapErrorKind::InvalidSource(
            "source patterns must not be empty".to_string(),
            0,
        )));
    }
    let chars: Vec<char> = src.chars().collect();
    if is_separator(chars[0]) {
        return Err(err(0, RemapErrorKind::InvalidSource(
            "source patterns must be relative".to_string(),
            0,
        )));
    }
    if is_separator(chars[chars.len() - 1]) {
        return Err(err(0, RemapErrorKind::InvalidSource(
            "source patterns must not end with a separator".to_string(),
            chars.len() - 1,
        )));
    }
    let mut tokens: Vec<SourceToken> = Vec::new();
    let mut captures: Vec<CaptureInfo> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    let mut at_component_start = true;
    while i < chars.len() {
        let c = chars[i];
        if is_separator(c) {
            if at_component_start {
                return Err(err(0, RemapErrorKind::InvalidSource(
                    "empty path component".to_string(),
                    i,
                )));
            }
            tokens.push(SourceToken::Separator);
            plain.push('/');
            at_component_start = true;
            i += 1;
            continue;
        }

        if c == '{' {
            let close = match find_close_brace(&chars, i) {
                Some(p) => p,
                None => {
                    return Err(err(0, RemapErrorKind::InvalidSource(
                        "unbalanced `{`".to_string(),
                        i,
                    )))
                }
            };
            // `{{` is an escaped literal brace.
            if close == i + 1 && chars[i + 1] == '{' {
                tokens.push(SourceToken::Literal('{'));
                plain.push_str("[{]");
                at_component_start = false;
                i = close + 1;
                continue;
            }
            let inner: String = chars[i + 1..close].iter().collect();
            let colon = inner.find(':').ok_or_else(|| {
                err(0, RemapErrorKind::InvalidSource(
                    "expected `{name:*}` or `{name:**}`".to_string(),
                    i + 1,
                ))
            })?;
            let (name, wildcard) = inner.split_at(colon);
            let wildcard = &wildcard[1..];
            if name.is_empty()
                || !name.chars().next().map_or(false, is_ident_start)
                || !name.chars().all(is_ident_cont)
            {
                return Err(err(0, RemapErrorKind::InvalidSource(
                    "capture name must start with a letter or underscore and contain only \
                     ascii alphanumerics or underscores"
                        .to_string(),
                    i + 1,
                )));
            }
            if captures.iter().any(|ci| ci.name == name) {
                return Err(err(0, RemapErrorKind::InvalidSource(
                    format!("duplicate capture name `{}`", name),
                    i + 1,
                )));
            }
            let follows_separator =
                close + 1 == chars.len() || is_separator(chars[close + 1]);
            let idx = captures.len();
            match wildcard {
                "*" => {
                    // `*` never crosses separators; literal prefixes or
                    // suffixes inside the same component (e.g.
                    // `{name:*}.rs`) are accepted.
                    let _ = follows_separator;
                    captures.push(CaptureInfo {
                        name: name.to_string(),
                        recursive: false,
                    });
                    tokens.push(SourceToken::Single(idx));
                    plain.push('*');
                }
                "**" => {
                    if !at_component_start || !follows_separator {
                        return Err(err(0, RemapErrorKind::InvalidSource(
                            "a named `**` capture must be an entire path component".to_string(),
                            i,
                        )));
                    }
                    captures.push(CaptureInfo {
                        name: name.to_string(),
                        recursive: true,
                    });
                    tokens.push(SourceToken::Recursive(idx));
                    plain.push_str("**");
                }
                _ => {
                    return Err(err(0, RemapErrorKind::InvalidSource(
                        "only named `*` and `**` captures are supported".to_string(),
                        i + 1 + colon,
                    )));
                }
            }
            at_component_start = false;
            // Consume the separator following a capture component.
            if close + 1 < chars.len() && is_separator(chars[close + 1]) {
                tokens.push(SourceToken::Separator);
                plain.push('/');
                i = close + 2;
                at_component_start = true;
            } else {
                i = close + 1;
            }
            continue;
        }

        if c == '}' {
            return Err(err(0, RemapErrorKind::InvalidSource(
                "unbalanced `}`; write `}}` for a literal brace".to_string(),
                i,
            )));
        }
        if c == '?' || c == '[' || c == ']' {
            return Err(err(0, RemapErrorKind::InvalidSource(
                "only named `*` and `**` wildcards are supported in remap sources".to_string(),
                i,
            )));
        }
        if c == '*' {
            return Err(err(0, RemapErrorKind::InvalidSource(
                "wildcards must be named: write `{name:*}` or `{dir:**}`".to_string(),
                i,
            )));
        }
        tokens.push(SourceToken::Literal(c));
        plain.push_str(&Pattern::escape(&c.to_string()));
        at_component_start = false;
        i += 1;
    }
    Ok((tokens, captures, plain))
}

/// Find the brace closing the one opened at `open`, honoring `{{` escapes.
fn find_close_brace(chars: &[char], open: usize) -> Option<usize> {
    if open + 1 < chars.len() && chars[open + 1] == '{' {
        return Some(open + 1);
    }
    let mut j = open + 1;
    while j < chars.len() {
        if chars[j] == '}' {
            return Some(j);
        }
        j += 1;
    }
    None
}

fn parse_target(dst: &str, captures: &[CaptureInfo]) -> Result<Vec<TargetToken>, RemapError> {
    let chars: Vec<char> = dst.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '{' {
            if i + 1 < chars.len() && chars[i + 1] == '{' {
                tokens.push(TargetToken::Literal('{'));
                i += 2;
                continue;
            }
            let close = chars[i + 1..]
                .iter()
                .position(|&x| x == '}')
                .map(|p| p + i + 1)
                .ok_or_else(|| {
                    err(0, RemapErrorKind::InvalidTarget("unbalanced `{`".to_string(), i))
                })?;
            let name: String = chars[i + 1..close].iter().collect();
            if name.is_empty()
                || !name.chars().next().map_or(false, is_ident_start)
                || !name.chars().all(is_ident_cont)
            {
                return Err(err(0, RemapErrorKind::InvalidTarget(
                    "capture reference must be `{name}`".to_string(),
                    i + 1,
                )));
            }
            let idx = captures
                .iter()
                .position(|ci| ci.name == name)
                .ok_or_else(|| {
                    err(0, RemapErrorKind::UndefinedCapture(name.clone(), i + 1))
                })?;
            tokens.push(TargetToken::Capture(idx));
            i = close + 1;
            continue;
        }
        if c == '}' {
            if i + 1 < chars.len() && chars[i + 1] == '}' {
                tokens.push(TargetToken::Literal('}'));
                i += 2;
                continue;
            }
            return Err(err(0, RemapErrorKind::InvalidTarget(
                "unbalanced `}`; write `}}` for a literal brace".to_string(),
                i,
            )));
        }
        tokens.push(TargetToken::Literal(c));
        i += 1;
    }
    Ok(tokens)
}

/// Ordered rules plus the validation profile used to build a plan.
#[derive(Clone, Debug)]
pub struct RemapPlanner {
    safety: TargetSafety,
    options: MatchOptions,
    rules: Vec<CompiledRule>,
}

#[derive(Clone, Debug)]
struct CompiledRule {
    rule: RemapRule,
    pattern: Pattern,
}

impl RemapPlanner {
    /// Create an empty planner with the given target safety profile and the
    /// default [`MatchOptions`].
    pub fn new(safety: TargetSafety) -> RemapPlanner {
        RemapPlanner {
            safety,
            options: MatchOptions::new(),
            rules: Vec::new(),
        }
    }

    /// Override the match options used while matching sources.
    ///
    /// `require_literal_separator` is always effectively enabled: single
    /// captures never cross separators and every result is re-validated with
    /// [`Pattern`] under that requirement.
    pub fn with_match_options(mut self, options: MatchOptions) -> RemapPlanner {
        self.options = options;
        self
    }

    /// Append a rule; rules are tried in insertion order.
    pub fn add_rule(mut self, rule: RemapRule) -> Result<RemapPlanner, RemapError> {
        let pattern = Pattern::new(&rule.plain).map_err(|e| {
            err(self.rules.len() + 1, RemapErrorKind::InvalidSource(
                e.to_string(),
                e.pos,
            ))
        })?;
        self.rules.push(CompiledRule { rule, pattern });
        Ok(self)
    }

    /// Number of rules currently installed.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether the planner has no rules.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Produce the full plan for a batch of source paths.
    ///
    /// Rules are applied in declaration order (first match wins). The whole
    /// batch is rejected with [`RemapErrorKind::TargetConflict`] if two of the
    /// supplied sources would land on the same target.
    pub fn plan_all<I, P>(&self, sources: I) -> Result<Vec<PathMapping>, RemapError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<str>,
    {
        let mut mappings: Vec<PathMapping> = Vec::new();
        for (i, source) in sources.into_iter().enumerate() {
            let source = source.as_ref();
            let mapping = self.plan(source)?;
            for earlier in &mappings {
                if same_target(&earlier.target, &mapping.target, self.safety) {
                    return Err(err(
                        0,
                        RemapErrorKind::TargetConflict {
                            source: source.to_string(),
                            other_source: earlier.source.clone(),
                            target: mapping.target.clone(),
                        },
                    ));
                }
            }
            mappings.push(mapping);
        }
        Ok(mappings)
    }

    /// Plan a single source path, returning [`RemapErrorKind::NoRule`] when no
    /// rule matches.
    pub fn plan(&self, source: &str) -> Result<PathMapping, RemapError> {
        if source.is_empty() {
            return Err(err(0, RemapErrorKind::InvalidSource(
                "source paths must not be empty".to_string(),
                0,
            )));
        }
        for (rule_idx, compiled) in self.rules.iter().enumerate() {
            if let Some(captured) = best_match(compiled, source, self.options) {
                let target = render_target(compiled, &captured)
                    .map_err(|mut e| {
                        e.rule = rule_idx + 1;
                        e
                    })?;
                validate_target(&target, self.safety)
                    .map_err(|kind| err(rule_idx + 1, kind))?;
                return Ok(PathMapping {
                    source: source.to_string(),
                    target,
                    rule_index: rule_idx,
                    captures: compiled
                        .rule
                        .captures
                        .iter()
                        .map(|c| Capture {
                            name: c.name.clone(),
                            value: captured
                                .iter()
                                .find(|m| m.name == c.name)
                                .map(|m| m.value.clone())
                                .unwrap_or_default(),
                        })
                        .collect(),
                });
            }
        }
        Err(err(0, RemapErrorKind::NoRule))
    }
}

fn same_target(a: &str, b: &str, safety: TargetSafety) -> bool {
    fn normalize(s: &str, safety: TargetSafety) -> String {
        let s = match safety {
            TargetSafety::Windows => s.replace('\\', "/"),
            TargetSafety::Posix => s.to_string(),
        };
        if safety.case_insensitive() {
            s.to_ascii_lowercase()
        } else {
            s
        }
    }
    normalize(a, safety) == normalize(b, safety)
}

/// One concrete source -> target entry of a generated plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathMapping {
    /// The source path as supplied.
    pub source: String,
    /// The rendered target path.
    pub target: String,
    /// Index of the rule (in declaration order) that produced this mapping.
    pub rule_index: usize,
    /// The captured substrings in capture declaration order.
    pub captures: Vec<Capture>,
}

/// A named capture value inside a [`PathMapping`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    /// Name of the capture.
    pub name: String,
    /// Captured substring.
    pub value: String,
}

#[derive(Clone, Debug)]
struct Match {
    name: String,
    value: String,
    start: usize,
    len: usize,
}

/// Preference key.
///
/// Disambiguation follows the declared rule: cross-component (`**`) captures
/// are considered first, then single-component (`*`) captures; within each
/// group the captures are compared in declaration order, longest span first,
/// and for equal spans the capture with the smallest start offset (leftmost)
/// wins.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    recursive: Vec<(usize, Reverse<usize>)>,
    single: Vec<(usize, Reverse<usize>)>,
}

fn match_key(matches: &[Match], rule: &CompiledRule) -> Key {
    let mut recursive = Vec::new();
    let mut single = Vec::new();
    for capture in &rule.rule.captures {
        let m = matches
            .iter()
            .find(|m| m.name == capture.name)
            .expect("all captures assigned");
        let entry = (m.len, Reverse(m.start));
        if capture.recursive {
            recursive.push(entry);
        } else {
            single.push(entry);
        }
    }
    Key { recursive, single }
}

/// Run the capture engine, then keep the best proposal that the existing
/// `Pattern` semantics also accept.
fn best_match(rule: &CompiledRule, input: &str, options: MatchOptions) -> Option<Vec<Match>> {
    let chars: Vec<char> = input.chars().collect();
    let mut proposals: Vec<Vec<Match>> = Vec::new();
    let mut current: Vec<Match> = Vec::new();
    search(
        rule,
        &chars,
        0,
        0,
        true,
        options,
        &mut current,
        &mut proposals,
    );
    let recheck = recheck_options(options);
    proposals
        .into_iter()
        .filter(|_| rule.pattern.matches_with(input, recheck))
        .max_by(|a, b| match_key(a, rule).cmp(&match_key(b, rule)))
}

fn recheck_options(options: MatchOptions) -> MatchOptions {
    MatchOptions {
        case_sensitive: options.case_sensitive,
        require_literal_separator: true,
        require_literal_leading_dot: options.require_literal_leading_dot,
    }
}

#[allow(clippy::too_many_arguments)]
fn search(
    rule: &CompiledRule,
    chars: &[char],
    ti: usize,
    si: usize,
    at_component_start: bool,
    options: MatchOptions,
    current: &mut Vec<Match>,
    proposals: &mut Vec<Vec<Match>>,
) {
    let tokens = &rule.rule.tokens;
    if ti == tokens.len() {
        if si == chars.len() {
            proposals.push(current.clone());
        }
        return;
    }
    match &tokens[ti] {
        SourceToken::Literal(c) => {
            if si < chars.len() && char_eq(chars[si], *c, options) {
                search(
                    rule,
                    chars,
                    ti + 1,
                    si + 1,
                    is_separator(chars[si]),
                    options,
                    current,
                    proposals,
                );
            }
        }
        SourceToken::Separator => {
            if si < chars.len() && is_separator(chars[si]) {
                search(rule, chars, ti + 1, si + 1, true, options, current, proposals);
            }
        }
        SourceToken::Single(idx) => {
            let info = &rule.rule.captures[*idx];
            // Enumerate every run up to (but not crossing) a separator. The
            // empty run is enumerated as well, but is rejected when the
            // target is rendered.
            let mut end = si;
            loop {
                let leading_dot =
                    at_component_start && end > si && chars.get(si) == Some(&'.');
                if !(leading_dot && options.require_literal_leading_dot) {
                    let value: String = chars[si..end].iter().collect();
                    current.push(Match {
                        name: info.name.clone(),
                        value,
                        start: si,
                        len: end - si,
                    });
                    search(
                        rule,
                        chars,
                        ti + 1,
                        end,
                        at_component_start,
                        options,
                        current,
                        proposals,
                    );
                    current.pop();
                }
                if end >= chars.len() || is_separator(chars[end]) {
                    break;
                }
                end += 1;
            }
        }
        SourceToken::Recursive(idx) => {
            let info = rule.rule.captures[*idx].clone();
            // Enumerate every boundary. The token following `**` is the
            // separator the parser emitted: for an empty capture it is kept
            // for the literal structure (matching `a/**/b` against `a/b`);
            // for a non-empty capture the input separator is consumed.
            let mut end = si;
            loop {
                let segment_ok = end == si
                    || !(options.require_literal_leading_dot
                        && leading_component_starts_with_dot(chars, si, end));
                if segment_ok {
                    let value: String = chars[si..end].iter().collect();
                    current.push(Match {
                        name: info.name.clone(),
                        value,
                        start: si,
                        len: end - si,
                    });
                    // The Separator after `**` is parser bookkeeping: the
                    // slash is part of the `**/` token, exactly like in
                    // `Pattern`. An empty capture therefore skips it
                    // (`a/**/b` matches `a/b`), while a non-empty capture
                    // must consume the slash in the input.
                    let (next_ti, next_si) =
                        if ti + 1 < tokens.len()
                            && matches!(tokens[ti + 1], SourceToken::Separator)
                        {
                            if end == si {
                                (ti + 2, end)
                            } else if end < chars.len() && is_separator(chars[end]) {
                                (ti + 2, end + 1)
                            } else {
                                // the boundary in the input is not a separator
                                (ti + 2, usize::MAX)
                            }
                        } else {
                            (ti + 1, end)
                        };
                    if next_si != usize::MAX {
                        search(
                            rule,
                            chars,
                            next_ti,
                            next_si,
                            true,
                            options,
                            current,
                            proposals,
                        );
                    }
                    current.pop();
                }
                if end >= chars.len() {
                    break;
                }
                end += 1;
            }
        }
    }
}

fn leading_component_starts_with_dot(chars: &[char], start: usize, end: usize) -> bool {
    if start < end && chars[start] == '.' {
        return true;
    }
    for p in (start + 1)..end {
        if is_separator(chars[p - 1]) && chars[p] == '.' {
            return true;
        }
    }
    false
}

fn char_eq(a: char, b: char, options: MatchOptions) -> bool {
    if cfg!(windows) && is_separator(a) && is_separator(b) {
        true
    } else if !options.case_sensitive && a.is_ascii() && b.is_ascii() {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

fn render_target(rule: &CompiledRule, captures: &[Match]) -> Result<String, RemapError> {
    for m in captures {
        if m.value.is_empty() {
            return Err(err(0, RemapErrorKind::EmptyCapture(m.name.clone())));
        }
    }
    let mut out = String::new();
    for token in &rule.rule.dst_tokens {
        match token {
            TargetToken::Literal(c) => out.push(*c),
            TargetToken::Capture(idx) => {
                let m = captures
                    .iter()
                    .find(|m| m.name == rule.rule.captures[*idx].name)
                    .expect("capture assigned");
                out.push_str(&m.value);
            }
        }
    }
    Ok(out)
}

/// Validate a fully rendered target path against the platform profile.
fn validate_target(target: &str, safety: TargetSafety) -> Result<(), RemapErrorKind> {
    if target.is_empty() {
        return Err(RemapErrorKind::PathTraversal);
    }
    if is_separator(target.chars().next().unwrap()) {
        return Err(RemapErrorKind::AbsoluteTarget);
    }
    if let TargetSafety::Windows = safety {
        let chars: Vec<char> = target.chars().collect();
        if chars.len() >= 2 && chars[0].is_ascii_alphabetic() && chars[1] == ':' {
            return Err(RemapErrorKind::AbsoluteTarget);
        }
    }
    for component in target.split(|c| safety.separators().contains(&c)) {
        if component.is_empty() {
            return Err(RemapErrorKind::EmptyCapture(
                "<empty target component>".to_string(),
            ));
        }
        if component == "." || component == ".." {
            return Err(RemapErrorKind::PathTraversal);
        }
        match safety {
            TargetSafety::Posix => {
                if component.contains('\0') {
                    return Err(RemapErrorKind::ReservedCharacter(component.to_string()));
                }
            }
            TargetSafety::Windows => {
                if let Some(bad) = component.chars().find(|&c| {
                    matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                        || (c as u32) < 0x20
                }) {
                    let _ = bad;
                    return Err(RemapErrorKind::ReservedCharacter(component.to_string()));
                }
                let stem = component.split('.').next().unwrap_or(component);
                if RESERVED_WINDOWS_NAMES
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(stem))
                {
                    return Err(RemapErrorKind::ReservedDeviceName(component.to_string()));
                }
                let last = component.chars().last().unwrap();
                if last == '.' || last == ' ' {
                    return Err(RemapErrorKind::ReservedCharacter(component.to_string()));
                }
            }
        }
    }
    Ok(())
}

const RESERVED_WINDOWS_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Compile `(source, target)` pairs into a planner.
pub fn planner_from_rules<'a, I>(safety: TargetSafety, rules: I) -> Result<RemapPlanner, RemapError>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut planner = RemapPlanner::new(safety);
    for (src, dst) in rules {
        planner = planner.add_rule(RemapRule::new(src, dst)?)?;
    }
    Ok(planner)
}

/// Whether `path` would be accepted as a target under the profile.
pub fn is_safe_target<P: AsRef<Path>>(path: P, safety: TargetSafety) -> bool {
    path.as_ref()
        .to_str()
        .map_or(false, |s| validate_target(s, safety).is_ok())
}

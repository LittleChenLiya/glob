// Copyright 2026 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Compilable glob patterns with named slots.
//!
//! A [`PatternTemplate`] is a glob pattern in which certain path segments are
//! replaced by named slots, written `{name}`. The template is compiled once;
//! concrete values are then *bound* to the slots through
//! [`PatternTemplate::bind`], producing an ordinary [`Pattern`] that matches
//! the literal values.
//!
//! # Parsing rules
//!
//! Slots are parsed independently from the rest of the glob syntax:
//!
//! - A slot is written `{name}` where `name` is a non-empty sequence of ASCII
//!   letters, digits or underscores.
//! - A slot is never recognized inside a `[...]` character class; the whole
//!   class (including any braces it contains) is passed through to the glob
//!   compiler unchanged.
//! - The braces `{` and `}` can be matched literally by escaping them as
//!   `\{` and `\}`. A literal backslash is written `\\`. A backslash that is
//!   not followed by `{`, `}` or `\` is treated as a literal backslash and
//!   handed to the glob compiler.
//!
//! # Binding rules
//!
//! A value is only accepted when it forms a single path segment:
//!
//! - it must be non-empty;
//! - it must not contain a path separator (`/` or `\`);
//! - it must not be the parent directory segment `..`.
//!
//! Every accepted value is escaped with [`Pattern::escape`] before it is
//! inserted, so a value containing metacharacters such as `*`, `?` or `[`
//! matches only itself and can never expand the wildcard structure of the
//! template.
//!
//! Missing and unknown parameters are reported before any pattern is
//! generated. Repeated slots in the template always use the same value.
//!
//! # Examples
//!
//! ```rust
//! use glob::PatternTemplate;
//!
//! let template = PatternTemplate::new("src/{module}/**/{name}.rs").unwrap();
//!
//! let pattern = template
//!     .bind(&[("module", "server"), ("name", "a[1]*")])
//!     .unwrap();
//!
//! assert!(pattern.matches("src/server/a[1]*.rs"));
//! assert!(pattern.matches("src/server/sub/dir/a[1]*.rs"));
//! // The escaped value does not act as a character class:
//! assert!(!pattern.matches("src/server/a1xyz.rs"));
//! ```

use std::error::Error;
use std::fmt;
use std::str::FromStr;

use crate::{Pattern, PatternError};

/// A compiled glob pattern template with named slots.
///
/// The template is compiled once and can be bound any number of times.
/// Binding takes `&self` and returns a new [`Pattern`]; the compiled template
/// is never modified, so two bindings never share or leak values.
///
/// See the [module documentation](self) for the parsing and binding rules.
#[derive(Clone, Debug)]
pub struct PatternTemplate {
    original: String,
    segments: Vec<Segment>,
    slots: Vec<Slot>,
}

#[derive(Clone, Debug)]
struct Slot {
    name: String,
    /// Byte position of the slot's opening `{` in the original template.
    pos: usize,
}

#[derive(Clone, Debug)]
enum Segment {
    /// Literal glob text (braces already unescaped).
    Literal(String),
    /// Index into `slots`.
    Slot(usize),
}

/// An error returned when compiling a [`PatternTemplate`].
#[derive(Debug)]
pub struct TemplateError {
    /// The byte position in the template where the error was detected.
    pub pos: usize,
    kind: TemplateErrorKind,
}

impl TemplateError {
    fn new(pos: usize, kind: TemplateErrorKind) -> Self {
        TemplateError { pos, kind }
    }

    /// The reason the template could not be compiled.
    pub fn kind(&self) -> &TemplateErrorKind {
        &self.kind
    }
}

/// The reason a [`PatternTemplate`] could not be compiled.
#[derive(Debug)]
#[non_exhaustive]
pub enum TemplateErrorKind {
    /// The non-slot parts of the template are not valid glob syntax.
    InvalidPattern(PatternError),
    /// A slot name is empty or contains characters other than ASCII
    /// alphanumerics and underscores.
    InvalidSlotName,
    /// An opening `{` has no matching closing `}`.
    UnclosedSlot,
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            TemplateErrorKind::InvalidPattern(e) => {
                write!(f, "invalid glob in template near byte {}: {}", self.pos, e)
            }
            TemplateErrorKind::InvalidSlotName => write!(
                f,
                "invalid slot name near byte {}: slot names must be non-empty and \
                 contain only ASCII alphanumerics or underscores",
                self.pos
            ),
            TemplateErrorKind::UnclosedSlot => write!(
                f,
                "unclosed slot near byte {}: expected a closing `}}`",
                self.pos
            ),
        }
    }
}

impl Error for TemplateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            TemplateErrorKind::InvalidPattern(e) => Some(e),
            _ => None,
        }
    }
}

/// An error returned when binding values to a [`PatternTemplate`].
#[derive(Debug)]
pub struct BindError {
    kind: BindErrorKind,
}

impl BindError {
    /// The reason the values could not be bound.
    pub fn kind(&self) -> &BindErrorKind {
        &self.kind
    }
}

/// The reason values could not be bound to a [`PatternTemplate`].
#[derive(Debug)]
#[non_exhaustive]
pub enum BindErrorKind {
    /// A slot declared in the template was not given a value.
    MissingSlot {
        /// The name of the missing slot.
        name: String,
        /// The byte position of the slot in the original template.
        pos: usize,
    },
    /// A value was supplied for a name that does not appear as a slot.
    UnknownSlot {
        /// The supplied name that is not a slot.
        name: String,
    },
    /// A value is empty, contains a path separator, or is the parent
    /// directory segment `..`.
    InvalidSegment {
        /// The slot name the value was supplied for.
        name: String,
        /// The rejected value.
        value: String,
        /// The byte position of the slot in the original template.
        pos: usize,
    },
    /// The assembled pattern could not be compiled. This should not happen
    /// for a template that compiled successfully with escaped values.
    CompiledPattern(PatternError),
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            BindErrorKind::MissingSlot { name, pos } => write!(
                f,
                "missing value for slot `{}` (first used at byte {})",
                name, pos
            ),
            BindErrorKind::UnknownSlot { name } => {
                write!(f, "`{}` is not a slot in the template", name)
            }
            BindErrorKind::InvalidSegment { name, value, pos } => write!(
                f,
                "value for slot `{}` (at byte {}) must be a single path segment: \
                 {:?} is empty, contains a separator, or is `..`",
                name, pos, value
            ),
            BindErrorKind::CompiledPattern(e) => {
                write!(f, "failed to compile the bound pattern: {}", e)
            }
        }
    }
}

impl Error for BindError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            BindErrorKind::CompiledPattern(e) => Some(e),
            _ => None,
        }
    }
}

impl PatternTemplate {
    /// Compile a glob pattern template.
    ///
    /// See the [module documentation](self) for the slot syntax and parsing
    /// rules. An invalid template yields a [`TemplateError`].
    pub fn new(pattern: &str) -> Result<PatternTemplate, TemplateError> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut segments: Vec<Segment> = Vec::new();
        let mut slots: Vec<Slot> = Vec::new();
        let mut literal = String::new();
        let mut i = 0usize;
        let mut pos = 0usize;

        while i < chars.len() {
            match chars[i] {
                '\\' => {
                    // Only \{, \} and \\ are template-level escapes.
                    if i + 1 < chars.len() && matches!(chars[i + 1], '{' | '}' | '\\') {
                        literal.push(chars[i + 1]);
                        i += 2;
                        pos += 2;
                    } else {
                        literal.push('\\');
                        i += 1;
                        pos += 1;
                    }
                }
                '{' => {
                    let close = chars[i + 1..].iter().position(|&c| c == '}');
                    match close {
                        None => {
                            return Err(TemplateError::new(pos, TemplateErrorKind::UnclosedSlot));
                        }
                        Some(rel) => {
                            let name: String = chars[i + 1..i + 1 + rel].iter().collect();
                            if !is_valid_slot_name(&name) {
                                return Err(TemplateError::new(
                                    pos,
                                    TemplateErrorKind::InvalidSlotName,
                                ));
                            }
                            let idx = match slots.iter().position(|s| s.name == name) {
                                Some(idx) => idx,
                                None => {
                                    slots.push(Slot { name, pos });
                                    slots.len() - 1
                                }
                            };
                            if !literal.is_empty() {
                                segments.push(Segment::Literal(std::mem::take(&mut literal)));
                            }
                            segments.push(Segment::Slot(idx));
                            let consumed = 2 + rel;
                            pos += byte_len(&chars[i..i + consumed]);
                            i += consumed;
                        }
                    }
                }
                '[' => match scan_character_class(&chars, i) {
                    Some(after) => {
                        let class: String = chars[i..after].iter().collect();
                        pos += byte_len(&chars[i..after]);
                        i = after;
                        literal.push_str(&class);
                    }
                    None => {
                        return Err(TemplateError::new(
                            pos,
                            TemplateErrorKind::InvalidPattern(PatternError {
                                pos,
                                msg: crate::ERROR_INVALID_RANGE,
                            }),
                        ));
                    }
                },
                c => {
                    literal.push(c);
                    pos += c.len_utf8();
                    i += 1;
                }
            }
        }

        if !literal.is_empty() {
            segments.push(Segment::Literal(literal));
        }

        Ok(PatternTemplate {
            original: pattern.to_string(),
            segments,
            slots,
        })
    }

    /// Bind concrete values to the slots and compile the resulting [`Pattern`].
    ///
    /// The template is not modified: every call returns an independent
    /// pattern, so values bound in one call never leak into a later one.
    ///
    /// # Errors
    ///
    /// Missing and unknown parameters, as well as values that are not single
    /// path segments, are reported through [`BindError`] before any pattern
    /// is generated.
    pub fn bind(&self, values: &[(&str, &str)]) -> Result<Pattern, BindError> {
        // Reject unknown parameters before doing anything else.
        for (name, _) in values {
            if self.slots.iter().all(|s| s.name != *name) {
                return Err(BindError {
                    kind: BindErrorKind::UnknownSlot {
                        name: (*name).to_string(),
                    },
                });
            }
        }

        // Resolve every slot to an escaped literal, checking for missing and
        // invalid values before any pattern is generated.
        let mut replacements: Vec<String> = Vec::with_capacity(self.slots.len());
        for slot in &self.slots {
            match values.iter().find(|(name, _)| *name == slot.name.as_str()) {
                None => {
                    return Err(BindError {
                        kind: BindErrorKind::MissingSlot {
                            name: slot.name.clone(),
                            pos: slot.pos,
                        },
                    });
                }
                Some((_, value)) => {
                    validate_segment(&slot.name, value, slot.pos)?;
                    replacements.push(Pattern::escape(value));
                }
            }
        }

        let mut pattern = String::new();
        for segment in &self.segments {
            match segment {
                Segment::Literal(text) => pattern.push_str(text),
                Segment::Slot(idx) => pattern.push_str(&replacements[*idx]),
            }
        }

        // The template was validated at compile time and every slot value was
        // escaped, so this can only fail due to a bug; surface it anyway.
        Pattern::new(&pattern).map_err(|e| BindError {
            kind: BindErrorKind::CompiledPattern(e),
        })
    }

    /// The original template text.
    pub fn as_str(&self) -> &str {
        &self.original
    }

    /// The slot names in order of first appearance.
    ///
    /// A name that appears more than once in the template is yielded once.
    pub fn slot_names(&self) -> impl Iterator<Item = &str> {
        self.slots.iter().map(|s| s.name.as_str())
    }
}

impl fmt::Display for PatternTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.original)
    }
}

impl FromStr for PatternTemplate {
    type Err = TemplateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Returns `true` if `name` is a valid slot name.
fn is_valid_slot_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Byte length of a slice of characters.
fn byte_len(chars: &[char]) -> usize {
    chars.iter().map(|c| c.len_utf8()).sum()
}

/// Scan a `[...]` character class starting at `chars[i]` (which must be `[`).
///
/// Returns the character index immediately after the closing `]`, or `None`
/// if the class is unclosed. This mirrors the class scanning in
/// [`Pattern::new`], including the `[]` and `[!]` literal-bracket cases.
fn scan_character_class(chars: &[char], i: usize) -> Option<usize> {
    if i + 4 <= chars.len() && chars[i + 1] == '!' {
        chars[i + 3..]
            .iter()
            .position(|x| *x == ']')
            .map(|j| i + 3 + j + 1)
    } else if i + 3 <= chars.len() && chars[i + 1] != '!' {
        chars[i + 2..]
            .iter()
            .position(|x| *x == ']')
            .map(|j| i + 2 + j + 1)
    } else {
        None
    }
}

/// Validate that a bound value is a single path segment.
fn validate_segment(name: &str, value: &str, pos: usize) -> Result<(), BindError> {
    let is_separator = |c: char| c == '/' || c == '\\';
    if value.is_empty() || value.contains(is_separator) || value == ".." {
        return Err(BindError {
            kind: BindErrorKind::InvalidSegment {
                name: name.to_string(),
                value: value.to_string(),
                pos,
            },
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_literal_name() {
        let template = PatternTemplate::new("src/{module}/**/{name}.rs").unwrap();
        let pattern = template
            .bind(&[("module", "server"), ("name", "a[1]*")])
            .unwrap();

        assert!(pattern.matches("src/server/a[1]*.rs"));
        assert!(pattern.matches("src/server/sub/dir/a[1]*.rs"));
        // The escaped value must not act as a character class or wildcard:
        assert!(!pattern.matches("src/server/a1xyz.rs"));
        assert!(!pattern.matches("src/server/sub/a1xyz.rs"));
    }

    #[test]
    fn rejects_parent_directory_segment() {
        let template = PatternTemplate::new("src/{module}/**/{name}.rs").unwrap();
        for module in ["..", "../server", "server/..", "../"] {
            assert!(
                template.bind(&[("module", module), ("name", "x")]).is_err(),
                "module {:?} should be rejected",
                module
            );
        }
    }

    #[test]
    fn rejects_separators() {
        let template = PatternTemplate::new("src/{module}/**/{name}.rs").unwrap();
        assert!(template.bind(&[("module", "a/b"), ("name", "x")]).is_err());
        assert!(template.bind(&[("module", r"a\b"), ("name", "x")]).is_err());
        assert!(template.bind(&[("module", "m"), ("name", "a/b")]).is_err());
    }

    #[test]
    fn rejects_empty_value() {
        let template = PatternTemplate::new("src/{module}/**/{name}.rs").unwrap();
        assert!(template.bind(&[("module", ""), ("name", "x")]).is_err());
        assert!(template.bind(&[("module", "m"), ("name", "")]).is_err());
    }

    #[test]
    fn missing_parameter_reports_template_position() {
        let template = PatternTemplate::new("src/{module}/**/{name}.rs").unwrap();
        let err = template.bind(&[("module", "server")]).unwrap_err();
        match err.kind() {
            BindErrorKind::MissingSlot { name, pos } => {
                assert_eq!(name, "name");
                // `{name}` opens at byte 16: `src/` (4) + `{module}` (8) + `/**/` (4) = 16.
                assert_eq!(*pos, 16);
            }
            other => panic!("expected MissingSlot, got {:?}", other),
        }
    }

    #[test]
    fn unknown_parameter_is_rejected() {
        let template = PatternTemplate::new("src/{module}/**/{name}.rs").unwrap();
        let err = template
            .bind(&[("module", "a"), ("name", "b"), ("extra", "c")])
            .unwrap_err();
        match err.kind() {
            BindErrorKind::UnknownSlot { name } => assert_eq!(name, "extra"),
            other => panic!("expected UnknownSlot, got {:?}", other),
        }
    }

    #[test]
    fn repeated_slot_uses_same_value() {
        let template = PatternTemplate::new("{x}/{x}").unwrap();
        let pattern = template.bind(&[("x", "a[1]")]).unwrap();
        assert!(pattern.matches("a[1]/a[1]"));
        assert!(!pattern.matches("a[1]/a2"));
        assert!(!pattern.matches("a2/a[1]"));
    }

    #[test]
    fn escaped_braces_are_literal() {
        let template = PatternTemplate::new(r"\{literal\}/{name}").unwrap();
        let pattern = template.bind(&[("name", "x")]).unwrap();
        assert!(pattern.matches("{literal}/x"));
        assert!(!pattern.matches("x"));
        assert!(!pattern.matches(r"\{literal\}/x"));
    }

    #[test]
    fn escaped_backslash_is_literal() {
        let template = PatternTemplate::new(r"a\\b/{name}").unwrap();
        let pattern = template.bind(&[("name", "x")]).unwrap();
        assert!(pattern.matches(r"a\b/x"));
    }

    #[test]
    fn chinese_position_is_byte_offset() {
        // `源码/` is 7 bytes, `{module}` is 8 bytes, `/**/` is 4 bytes,
        // so `{name}` opens at byte 19.
        let template = PatternTemplate::new("源码/{module}/**/{name}.rs").unwrap();
        let err = template.bind(&[("module", "m")]).unwrap_err();
        match err.kind() {
            BindErrorKind::MissingSlot { name, pos } => {
                assert_eq!(name, "name");
                assert_eq!(*pos, 19);
            }
            other => panic!("expected MissingSlot, got {:?}", other),
        }
    }

    #[test]
    fn chinese_value_is_matched_literally() {
        let template = PatternTemplate::new("{name}").unwrap();
        let pattern = template.bind(&[("name", "中文")]).unwrap();
        assert!(pattern.matches("中文"));
        assert!(!pattern.matches("中文x"));
    }

    #[test]
    fn bindings_do_not_leak_values() {
        let template = PatternTemplate::new("{x}").unwrap();
        let first = template.bind(&[("x", "first")]).unwrap();
        let second = template.bind(&[("x", "second")]).unwrap();

        assert!(first.matches("first"));
        assert!(!first.matches("second"));
        assert!(second.matches("second"));
        assert!(!second.matches("first"));

        // The template is unchanged and rebinding reproduces the pattern.
        let rebound = template.bind(&[("x", "first")]).unwrap();
        assert_eq!(rebound.as_str(), first.as_str());
        assert_eq!(
            template.bind(&[("x", "second")]).unwrap().as_str(),
            second.as_str()
        );
    }

    #[test]
    fn slot_inside_character_class_is_literal() {
        let template = PatternTemplate::new("[{name}]").unwrap();
        assert_eq!(template.slot_names().count(), 0);
        let pattern = template.bind(&[]).unwrap();
        assert!(pattern.matches("{"));
        assert!(pattern.matches("n"));
        assert!(!pattern.matches("x"));
    }

    #[test]
    fn unclosed_character_class_is_rejected() {
        assert!(PatternTemplate::new("src/[unclosed/{name}").is_err());
    }

    #[test]
    fn invalid_slot_names_are_rejected() {
        assert!(PatternTemplate::new("{}").is_err());
        assert!(PatternTemplate::new("{na me}").is_err());
        assert!(PatternTemplate::new("{na-me}").is_err());
        assert!(PatternTemplate::new("{name").is_err());
    }

    #[test]
    fn template_compiles_once_and_binds_many_times() {
        let template = PatternTemplate::new("src/{module}/**/{name}.rs").unwrap();
        for module in ["a", "b", "c"] {
            let pattern = template.bind(&[("module", module), ("name", "n")]).unwrap();
            assert!(pattern.matches(&format!("src/{}/n.rs", module)));
            assert!(pattern.matches(&format!("src/{}/sub/dir/n.rs", module)));
            assert!(!pattern.matches(&format!("src/{}/n", module)));
        }
    }

    #[test]
    fn bound_pattern_does_not_expand_wildcards() {
        let template = PatternTemplate::new("{name}").unwrap();
        // Every metacharacter in the value is matched literally.
        let pattern = template.bind(&[("name", "?*[a-z]")]).unwrap();
        assert!(pattern.matches("?*[a-z]"));
        assert!(!pattern.matches("x"));
        assert!(!pattern.matches("a"));
    }

    #[test]
    fn display_and_from_str() {
        let template: PatternTemplate = "src/{module}/**/{name}.rs".parse().unwrap();
        assert_eq!(template.to_string(), "src/{module}/**/{name}.rs");
        assert_eq!(template.as_str(), "src/{module}/**/{name}.rs");
    }
}

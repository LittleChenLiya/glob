// Copyright 2026 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Explicit structural indexes for compiled [`Pattern`](crate::Pattern)s.
//!
//! A [`Pattern`] is normally seen as an opaque compiled matcher. The types in
//! this module expose *how* the pattern source text is parsed: the source is
//! partitioned into a sequence of [`PatternSegment`]s that exactly covers the
//! pattern, one syntactic construct per segment:
//!
//! - [`PatternSegmentKind::Literal`] is plain literal text;
//! - [`PatternSegmentKind::Escaped`] is a backslash escape such as `\*`,
//!   holding the decoded character (`*`);
//! - [`PatternSegmentKind::CharClass`] is a `[...]` / `[!...]` character class
//!   holding its decoded elements (a `*` inside a class is *not* a wildcard);
//! - [`PatternSegmentKind::Star`], [`PatternSegmentKind::DoubleStar`] and
//!   [`PatternSegmentKind::Question`] are the wildcard segments.
//!
//! Every segment carries its original range in the pattern source. The ranges
//! are **UTF-8 byte offsets** (not `char` offsets), so they can be used
//! directly to slice the pattern string; this also defines how non-ASCII
//! ("Unicode") text is addressed.
//!
//! The index is produced by the same grammar parser that compiles the
//! pattern for matching -- it is never reconstructed by guessing at the
//! syntax with a regular expression. In particular, metacharacters lose
//! their special meaning both inside character classes and after a
//! backslash.
//!
//! A [`PatternIndex`] is a snapshot of a pattern at the time it was built. It
//! can be used to edit a specific literal segment with
//! [`Pattern::replace_literal_segment`](crate::Pattern::replace_literal_segment),
//! which first verifies that the current pattern still matches the index
//! baseline and then guarantees, after recompiling, that every non-target
//! segment keeps exactly the same structure (only byte ranges may shift).

use std::error::Error;
use std::fmt;
use std::ops::Range;

use crate::PatternError;

/// One decoded element of a character class such as `[a-c*]`.
///
/// Ranges order characters by their Unicode scalar value, exactly as they are
/// interpreted when matching.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CharClassItem {
    /// A single character, e.g. `x` or `*` in `[x*]`.
    Char(char),
    /// An inclusive range of Unicode scalar values, e.g. the `0-9` in
    /// `[0-9a-f]`.
    CharRange(char, char),
}

/// The syntactic kind of a [`PatternSegment`].
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum PatternSegmentKind {
    /// Plain literal text. The held string is the decoded literal value,
    /// which for this variant is identical to the covered source slice.
    Literal(String),
    /// A backslash-escaped metacharacter, e.g. `\*` or `\[`. The held value
    /// is the decoded character, so `\*` decodes to `*` and is matched
    /// literally (it is *not* a wildcard).
    Escaped(char),
    /// A single `*` wildcard segment.
    Star,
    /// A recursive `**` wildcard segment.
    DoubleStar,
    /// A single `?` wildcard segment.
    Question,
    /// A character class `[...]`, or its negation `[!...]`.
    ///
    /// A `*` appearing in `items` is a plain member of the class, never a
    /// wildcard.
    CharClass {
        /// Whether this is a negated class (`[!...]`).
        negated: bool,
        /// The decoded elements (single characters and ranges) of the class,
        /// in source order.
        items: Vec<CharClassItem>,
    },
}

impl PatternSegmentKind {
    /// Return the decoded literal value if this segment stands for exactly
    /// one literal character or run of literal text.
    ///
    /// This returns `Some` for [`PatternSegmentKind::Literal`] and
    /// [`PatternSegmentKind::Escaped`] only; the returned value is the
    /// decoded text (a one-character string for an escape).
    pub fn as_literal(&self) -> Option<String> {
        match self {
            PatternSegmentKind::Literal(text) => Some(text.clone()),
            PatternSegmentKind::Escaped(c) => Some(c.to_string()),
            _ => None,
        }
    }
}

/// One segment of a parsed pattern.
///
/// Segments are produced in source order and their [`range`](Self::range)s
/// partition the whole pattern string without gaps or overlaps.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct PatternSegment {
    pub(crate) range: Range<usize>,
    pub(crate) kind: PatternSegmentKind,
}

// `Range<usize>` only offers equality/hashing, so ordering compares the
// bounds explicitly (segments are naturally ordered by their source range).
impl PartialOrd for PatternSegment {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PatternSegment {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.range
            .start
            .cmp(&other.range.start)
            .then_with(|| self.range.end.cmp(&other.range.end))
            .then_with(|| self.kind.cmp(&other.kind))
    }
}

impl PatternSegment {
    /// The original range of this segment in the pattern source.
    ///
    /// The bounds are **UTF-8 byte offsets** into the string the pattern was
    /// built from, so the range can be used directly to slice the source.
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    /// The syntactic kind of this segment, including its decoded literal
    /// value where applicable.
    pub fn kind(&self) -> &PatternSegmentKind {
        &self.kind
    }

    /// The decoded literal text covered by this segment, if it is literal
    /// text or a backslash escape; returns `None` for wildcards and
    /// character classes.
    pub fn as_literal(&self) -> Option<String> {
        self.kind.as_literal()
    }
}

/// A structural index of a compiled [`Pattern`].
///
/// Obtain one with [`Pattern::index`](crate::Pattern::index). It records the
/// pattern source it was built from ([`baseline`](Self::baseline)) together
/// with the parsed [`segments`](Self::segments). It can later be used to edit
/// a specific literal segment through
/// [`Pattern::replace_literal_segment`](crate::Pattern::replace_literal_segment);
/// that operation refuses the index if the pattern has since changed.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PatternIndex {
    pub(crate) baseline: String,
    pub(crate) segments: Vec<PatternSegment>,
}

impl PatternIndex {
    pub(crate) fn new(baseline: String, segments: Vec<PatternSegment>) -> Self {
        Self { baseline, segments }
    }

    /// The pattern source string this index was built from.
    pub fn baseline(&self) -> &str {
        &self.baseline
    }

    /// The parsed segments, in source order. Their ranges partition the
    /// whole baseline string.
    pub fn segments(&self) -> &[PatternSegment] {
        &self.segments
    }

    /// Number of segments in the index.
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// Whether the index has no segments (only for the empty pattern).
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Return the segment at `index`, or `None` if out of bounds.
    pub fn get(&self, index: usize) -> Option<&PatternSegment> {
        self.segments.get(index)
    }
}

/// An error returned by
/// [`Pattern::replace_literal_segment`](crate::Pattern::replace_literal_segment).
#[derive(Debug)]
#[non_exhaustive]
pub enum SegmentEditError {
    /// The pattern passed in is no longer the one the index was compiled
    /// from: its source text or its parsed structure differs from the
    /// baseline. The stale index must not be used.
    BaselineMismatch,
    /// The requested segment index does not exist in the index.
    SegmentOutOfBounds {
        /// The requested segment index.
        index: usize,
        /// Number of segments in the index.
        len: usize,
    },
    /// The targeted segment is not literal text (it is a wildcard, escape
    /// or character class), so it cannot be replaced as a literal segment.
    NotALiteralSegment {
        /// The segment index that was targeted.
        index: usize,
    },
    /// The replacement text does not parse to exactly one plain literal
    /// segment (for example, it contains a `*` or `[`).
    ReplacementNotLiteral {
        /// The rejected replacement text.
        replacement: String,
    },
    /// The replacement made the pattern invalid.
    InvalidPattern(PatternError),
    /// After recompiling the edited pattern, a non-target segment did not
    /// keep the same structure it had in the baseline.
    StructureChanged {
        /// The index of the non-target segment whose structure changed.
        segment: usize,
    },
}

impl fmt::Display for SegmentEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SegmentEditError::BaselineMismatch => write!(
                f,
                "the pattern has changed since the PatternIndex was built; \
                 the stale index cannot be used"
            ),
            SegmentEditError::SegmentOutOfBounds { index, len } => write!(
                f,
                "segment index {index} is out of bounds for a PatternIndex with {len} segments"
            ),
            SegmentEditError::NotALiteralSegment { index } => write!(
                f,
                "segment {index} is not a literal text segment and cannot be replaced as literal text"
            ),
            SegmentEditError::ReplacementNotLiteral { replacement } => write!(
                f,
                "replacement {replacement:?} does not consist of a single literal segment"
            ),
            SegmentEditError::InvalidPattern(err) => {
                write!(f, "the edited pattern is invalid: {err}")
            }
            SegmentEditError::StructureChanged { segment } => write!(
                f,
                "recompiling the edited pattern changed the structure of non-target segment {segment}"
            ),
        }
    }
}

impl Error for SegmentEditError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            SegmentEditError::InvalidPattern(err) => Some(err),
            _ => None,
        }
    }
}

/// Append a literal run covering char indices `start..end`, coalescing with
/// an immediately adjacent literal segment.
///
/// Shared helper used by the pattern parser; ranges are translated from
/// `char` indices to UTF-8 byte offsets with `byte_of`.
pub(crate) fn push_literal(
    segments: &mut Vec<PatternSegment>,
    original: &str,
    byte_of: &[usize],
    start: usize,
    end: usize,
) {
    if start == end {
        return;
    }
    let range = byte_of[start]..byte_of[end];
    let text = &original[range.clone()];
    if let Some(last) = segments.last_mut() {
        if let PatternSegmentKind::Literal(last_text) = &mut last.kind {
            if last.range.end == range.start {
                last_text.push_str(text);
                last.range.end = range.end;
                return;
            }
        }
    }
    segments.push(PatternSegment {
        range,
        kind: PatternSegmentKind::Literal(text.to_string()),
    });
}

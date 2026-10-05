//! Editing a literal segment of a glob pattern using a structure index.
//!
//! This example demonstrates how to:
//!
//! 1. Compile a pattern and build its structure index.
//! 2. Verify that the index still matches the pattern's baseline.
//! 3. Edit a literal segment while leaving non-target segments unchanged.
//! 4. Detect that an old index is stale after the baseline changes.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example pattern_edit
//! ```

use glob::Pattern;

fn main() {
    // The pattern from the acceptance criteria.
    let pattern = Pattern::new(r"a\*/[x*]/b?").expect("valid pattern");
    let index = pattern.index();

    println!("baseline: {}", index.baseline());
    println!("segments: {}", index.len());
    for (i, seg) in index.segments().iter().enumerate() {
        println!(
            "  [{}] {:?} range={}..{} decoded={:?}",
            i,
            seg.kind(),
            seg.start(),
            seg.end(),
            seg.decoded()
        );
    }

    // The index verifies against the original pattern.
    assert!(index.verify(&pattern), "index should verify against baseline");

    // Edit the first literal segment ("a") to "a2".
    let edited = index.edit_literal(0, "a2").expect("edit succeeds");
    println!("\nedited pattern: {}", edited.as_str());

    // The escaped asterisk is still a literal after the edit.
    assert!(edited.matches(r"a2*/x/b?"), "escaped asterisk remains literal");
    assert!(!edited.matches(r"aXX/x/b?"), "non-matching path rejected");

    // Re-index the edited pattern and confirm non-target segments kept
    // their structure.
    let edited_index = edited.index();
    let edited_segs = edited_index.segments();
    assert_eq!(edited_segs[0].decoded(), "a2", "target segment changed");
    assert_eq!(edited_segs[1].decoded(), "*", "escape segment unchanged");
    assert_eq!(edited_segs[3].decoded(), "[x*]", "char class unchanged");
    assert_eq!(edited_segs[5].decoded(), "?", "wildcard unchanged");

    // Now change the baseline. The old index must be rejected.
    let changed = Pattern::new(r"x\*/[x*]/b?").expect("valid pattern");
    assert!(!index.verify(&changed), "old index rejected after baseline change");

    // The old index still verifies against the original baseline.
    assert!(index.verify(&pattern), "index still valid for original baseline");

    println!("\nAll checks passed.");
}

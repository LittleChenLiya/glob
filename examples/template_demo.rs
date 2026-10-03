//! In-memory demonstration of [`PatternTemplate`].
//!
//! This example compiles a single template once, binds several sets of
//! external values to it, and matches the resulting patterns -- all without
//! touching the filesystem.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example template_demo
//! ```

use glob::PatternTemplate;

fn main() {
    // The template is compiled once and reused for every binding.
    let template = PatternTemplate::new("src/{module}/**/{name}.rs").expect("template compiles");

    // Bind external (untrusted) values. The name contains glob metacharacters;
    // they are escaped, so the bound pattern matches the literal name only.
    let bound = template
        .bind(&[("module", "server"), ("name", "a[1]*")])
        .expect("values bind");

    println!("bound pattern: {}", bound);
    for path in [
        "src/server/a[1]*.rs",
        "src/server/sub/dir/a[1]*.rs",
        "src/server/a1xyz.rs",
        "src/server/sub/a1xyz.rs",
    ] {
        println!("  matches {:?}: {}", path, bound.matches(path));
    }

    // Rebinding produces an independent pattern; the previous values do not
    // leak into the new one. The template supplies the `.rs` suffix, so the
    // name is the stem only.
    let bound2 = template
        .bind(&[("module", "client"), ("name", "lib")])
        .expect("values bind");
    println!("\nrebound pattern: {}", bound2);
    println!(
        "  matches src/client/lib.rs: {}",
        bound2.matches("src/client/lib.rs")
    );
    println!(
        "  matches src/server/a[1]*.rs: {}",
        bound2.matches("src/server/a[1]*.rs")
    );

    // Invalid external input is rejected before a pattern is generated.
    for (module, name) in [("../escape", "x"), ("a/b", "x"), ("", "x"), ("m", "")] {
        match template.bind(&[("module", module), ("name", name)]) {
            Ok(_) => println!(
                "\nunexpectedly accepted module={:?} name={:?}",
                module, name
            ),
            Err(e) => println!("\nrejected module={:?} name={:?}: {}", module, name, e),
        }
    }

    // Missing and unknown parameters are also reported up front.
    match template.bind(&[("module", "server")]) {
        Ok(_) => println!("\nunexpectedly accepted a missing parameter"),
        Err(e) => println!("\nmissing parameter: {}", e),
    }
    match template.bind(&[("module", "server"), ("name", "x"), ("extra", "y")]) {
        Ok(_) => println!("\nunexpectedly accepted an unknown parameter"),
        Err(e) => println!("\nunknown parameter: {}", e),
    }
}

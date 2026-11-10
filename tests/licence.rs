//! Apache 2.0 section 4(a): whoever distributes the Work must give the recipient a copy of the
//! licence. The installer shows the terms on screen; these tests make sure a copy also lands
//! on disk, inside the bundle, so it is there after the installer window has closed.

use std::fs;

#[test]
fn the_licence_at_the_root_is_apache_2_0() {
    let text = fs::read_to_string("LICENSE").expect("LICENSE at the workspace root");
    assert!(
        text.contains("Apache License") && text.contains("Version 2.0"),
        "LICENSE is not the Apache License, Version 2.0 that Cargo.toml and the README declare"
    );
}

/// Ignored so a plain `cargo test` needs no bundle; CI bundles first and runs `--include-ignored`.
#[test]
#[ignore = "needs `cargo xtask bundle` first; CI runs it with --include-ignored"]
fn the_bundled_driver_carries_a_copy_of_the_licence() {
    let bundled = std::path::Path::new("target/SendaLink.driver/Contents/Resources/LICENSE");
    let copy = fs::read(bundled).unwrap_or_else(|e| {
        panic!(
            "no licence at {} ({e}) — Apache 2.0 section 4(a) requires that a copy ship with \
             the Work; `cargo xtask bundle` puts it there",
            bundled.display()
        )
    });
    let original = fs::read("LICENSE").expect("LICENSE at the workspace root");
    assert!(
        copy == original,
        "the bundled LICENSE differs from the one at the workspace root"
    );
}

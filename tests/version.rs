//! **`version.json` is the only place this product's version is written.** These tests are what
//! keeps that true.
//!
//! The version is written once, in `version.json`. `Info.plist` carries the `${VERSION}`
//! placeholder and `xtask bundle` substitutes the real value, the same shape the release
//! pipeline uses. `Cargo.toml`'s version is pinned at 0.0.0 and unused, because cargo cannot
//! read a version out of a file and nothing here consumes `CARGO_PKG_VERSION`.
//!
//! A second copy would be a silent one: nothing propagates a bump between copies, so a release
//! announcing one version while the installed driver's Get Info showed another would need only
//! one forgotten edit, and nothing would fail. The user would simply be told two different things
//! by one product.
//!
//! These tests therefore assert an *absence*: that no second home has grown.

use std::fs;

const PLACEHOLDER: &str = "${VERSION}";

/// The one home, parsed the way `xtask` parses it.
fn declared_version() -> String {
    let text = fs::read_to_string("version.json").expect("version.json");
    text.split("\"version\"")
        .nth(1)
        .and_then(|rest| rest.split('"').nth(1))
        .expect("a version string in version.json")
        .to_string()
}

/// A version that is not three dot-separated numbers is one a git tag cannot match and the
/// release pipeline will refuse — caught here rather than four minutes into a release.
#[test]
fn the_declared_version_is_a_plain_three_part_number() {
    let v = declared_version();
    let parts: Vec<_> = v.split('.').collect();
    assert_eq!(
        parts.len(),
        3,
        "version.json says {v:?}, not MAJOR.MINOR.PATCH"
    );
    for p in parts {
        assert!(
            !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()),
            "version.json says {v:?}, and {p:?} is not a number"
        );
    }
}

/// **`Info.plist` must ask, never state.** A literal version here is a second home, and a silent
/// one: the plist is what macOS shows in Get Info for the installed driver, so a stale literal
/// tells the user something the release pipeline contradicts.
#[test]
fn info_plist_asks_for_the_version_rather_than_stating_one() {
    let plist = fs::read_to_string("Info.plist").expect("Info.plist");
    for key in ["CFBundleVersion", "CFBundleShortVersionString"] {
        let value = plist
            .split(&format!("<key>{key}</key>"))
            .nth(1)
            .and_then(|rest| rest.split("<string>").nth(1))
            .and_then(|rest| rest.split("</string>").next())
            .unwrap_or_else(|| panic!("{key} in Info.plist"))
            .trim();
        assert_eq!(
            value, PLACEHOLDER,
            "Info.plist's {key} states {value:?} instead of asking with {PLACEHOLDER}. \
             version.json is the only place the version is written; `xtask bundle` substitutes."
        );
    }
}

/// **`Cargo.toml`'s version is a sentinel and must stay one.** Cargo cannot derive a version from
/// a file, so the only way for it not to be a second home is for it to mean nothing. Nothing here
/// reads `CARGO_PKG_VERSION`; if that changes, this test is the place the change gets noticed.
#[test]
fn cargo_toml_is_not_a_second_home_for_the_version() {
    let cargo = fs::read_to_string("Cargo.toml").expect("Cargo.toml");
    let version = cargo
        .lines()
        .skip_while(|l| l.trim() != "[package]")
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|l| l.split('"').next())
        .expect("a version in [package]");
    assert_eq!(
        version, "0.0.0",
        "Cargo.toml's version is {version:?}. It is deliberately meaningless — version.json is \
         the only place this product's version is written. If something now needs \
         CARGO_PKG_VERSION, derive it rather than reintroducing a copy someone has to remember."
    );
}

/// The end-to-end property, asserted against the artefact rather than the intention: whatever
/// `version.json` says is what a user sees in Get Info.
///
/// Ignored by default because a plain `cargo test` must not require `cargo xtask bundle` to
/// have been run first. CI bundles and then runs `cargo test -- --include-ignored`, so it is
/// checked against a real artefact there, and it fails rather than skips if the bundle is
/// missing.
#[test]
#[ignore = "needs `cargo xtask bundle` first; CI runs it with --include-ignored"]
fn the_bundled_driver_carries_the_version_that_was_declared() {
    let bundled = std::path::Path::new("target/SendaLink.driver/Contents/Info.plist");
    let plist = fs::read_to_string(bundled).unwrap_or_else(|e| {
        panic!(
            "no bundle at {} ({e}) — run `cargo xtask bundle`",
            bundled.display()
        )
    });
    let want = declared_version();
    assert!(
        !plist.contains(PLACEHOLDER),
        "the bundled Info.plist still contains {PLACEHOLDER} — xtask did not substitute"
    );
    for key in ["CFBundleVersion", "CFBundleShortVersionString"] {
        let value = plist
            .split(&format!("<key>{key}</key>"))
            .nth(1)
            .and_then(|rest| rest.split("<string>").nth(1))
            .and_then(|rest| rest.split("</string>").next())
            .unwrap_or_else(|| panic!("{key} in the bundled Info.plist"))
            .trim();
        assert_eq!(
            value, want,
            "version.json says {want}, the bundled {key} says {value} — a user would be told two \
             different things by one product"
        );
    }
}

//! The version is written once, in `version.json`. `Info.plist` carries a
//! `${VERSION}` placeholder that `xtask bundle` substitutes, and `Cargo.toml`'s
//! version is a 0.0.0 sentinel. These tests assert that no second copy has grown.

use std::fs;

const PLACEHOLDER: &str = "${VERSION}";

fn declared_version() -> String {
    let text = fs::read_to_string("version.json").expect("version.json");
    text.split("\"version\"")
        .nth(1)
        .and_then(|rest| rest.split('"').nth(1))
        .expect("a version string in version.json")
        .to_string()
}

/// Anything else is a version a git tag cannot match.
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

/// Ignored so a plain `cargo test` needs no bundle; CI bundles first and runs `--include-ignored`.
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

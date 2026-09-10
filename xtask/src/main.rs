//! `cargo xtask bundle`: builds `target/SendaLink.driver` from the release
//! dylib, `Info.plist` and the device icon.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    if arg != "bundle" {
        eprintln!("usage: cargo xtask bundle");
        std::process::exit(2);
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf();

    let status = Command::new("cargo")
        .args(["build", "--release", "-p", "senda-link"])
        .current_dir(&root)
        .status()
        .expect("cargo build");
    assert!(status.success(), "build failed");

    // Cargo names the artefact libsenda_link.dylib; Info.plist's CFBundleExecutable is SendaLink.
    let dylib = root.join("target/release/libsenda_link.dylib");
    let bundle = root.join("target/SendaLink.driver");
    let macos = bundle.join("Contents/MacOS");
    let resources = bundle.join("Contents/Resources");

    let _ = std::fs::remove_dir_all(&bundle);
    std::fs::create_dir_all(&macos).expect("create bundle dirs");
    std::fs::create_dir_all(&resources).expect("create resources dir");
    std::fs::copy(&dylib, macos.join("SendaLink")).expect("copy binary");
    // Substituted, not copied: version.json is the only place the version is written.
    let version = product_version(&root);
    let plist = std::fs::read_to_string(root.join("Info.plist")).expect("read plist");
    assert!(
        plist.contains(VERSION_PLACEHOLDER),
        "Info.plist no longer contains {VERSION_PLACEHOLDER}, so nothing would put a version in \
         the bundle. It is derived from version.json, never written by hand."
    );
    std::fs::write(
        bundle.join("Contents/Info.plist"),
        plist.replace(VERSION_PLACEHOLDER, &version),
    )
    .expect("write plist");
    // Must sit directly under Contents/Resources for CFBundleCopyResourceURL to find it.
    std::fs::copy(
        root.join("resources/DeviceIcon.icns"),
        resources.join("DeviceIcon.icns"),
    )
    .expect("copy icon");

    println!("bundled: {} ({version})", bundle.display());
}

const VERSION_PLACEHOLDER: &str = "${VERSION}";

/// Parsed by hand: the file is four lines and the crate's point is zero dependencies.
fn product_version(root: &std::path::Path) -> String {
    let path = root.join("version.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    let version = text
        .split("\"version\"")
        .nth(1)
        .and_then(|rest| rest.split('"').nth(1))
        .unwrap_or_else(|| panic!("no \"version\" string in {}", path.display()));
    let parts: Vec<&str> = version.split('.').collect();
    assert!(
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
        "version.json says {version:?}, which is not MAJOR.MINOR.PATCH — a release tag cannot \
         match it"
    );
    version.to_string()
}

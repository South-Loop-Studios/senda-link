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

    // [lib] name in Cargo.toml is "senda_link" (must match tests/abi.rs's
    // `use senda_link::...`), so the cargo-built artefact is libsenda_link.dylib.
    // We rename it to "SendaLink" below to match Info.plist's CFBundleExecutable.
    let dylib = root.join("target/release/libsenda_link.dylib");
    let bundle = root.join("target/SendaLink.driver");
    let macos = bundle.join("Contents/MacOS");
    let resources = bundle.join("Contents/Resources");

    let _ = std::fs::remove_dir_all(&bundle);
    std::fs::create_dir_all(&macos).expect("create bundle dirs");
    std::fs::create_dir_all(&resources).expect("create resources dir");
    std::fs::copy(&dylib, macos.join("SendaLink")).expect("copy binary");
    // **Info.plist is substituted, not copied.** `version.json` is the only place this
    // product's version is written; the plist asks for it with `${VERSION}`, the same way
    // the release pipeline does, and this is what answers.
    //
    // A copied plist would be a second home for the version — and a silent one, because
    // nothing would propagate a bump or compare the two. A release announcing one version
    // while the driver's Get Info showed another would need only one forgotten edit.
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
    // `kAudioDevicePropertyIcon` resolves this via
    // `CFBundleCopyResourceURL(bundle, "DeviceIcon", "icns", NULL)` — see
    // `ffi::plugin::icon_resource_url`. It must land directly under
    // `Contents/Resources`, not a subdirectory.
    std::fs::copy(
        root.join("resources/DeviceIcon.icns"),
        resources.join("DeviceIcon.icns"),
    )
    .expect("copy icon");

    println!("bundled: {} ({version})", bundle.display());
}

/// What `Info.plist` asks for, and the release pipeline asks for, and neither answers.
const VERSION_PLACEHOLDER: &str = "${VERSION}";

/// This product's version, from the one file that states it.
///
/// Parsed rather than pulled in with serde: this is a build tool for a crate whose whole selling
/// point is zero dependencies, and the file is four lines. A malformed one panics with the reason
/// rather than bundling something unversioned.
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

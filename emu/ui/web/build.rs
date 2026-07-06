//! Sets the `AZLOCALDEV_VERSION` compile-time env var, which is the *only* source the app
//! reads its version from (see `src/update.rs`) - never `CARGO_PKG_VERSION`/`Cargo.toml`.
//! The release workflow sets the real `AZLOCALDEV_VERSION` env var (from the GitHub Release
//! tag that triggered the build) before invoking `cargo build`, so the tag is the single
//! source of truth for what a release build reports/compares itself against. Local dev
//! builds (where that env var isn't set) fall back to a `0.0.0-dev+<short-sha>` placeholder
//! so it's still obvious at a glance that it's not a real release.
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=AZLOCALDEV_VERSION");

    let version = std::env::var("AZLOCALDEV_VERSION").unwrap_or_else(|_| {
        let sha = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string());
        match sha {
            Some(sha) if !sha.is_empty() => format!("0.0.0-dev+{sha}"),
            _ => "0.0.0-dev".to_string(),
        }
    });

    println!("cargo:rustc-env=AZLOCALDEV_VERSION={version}");
}

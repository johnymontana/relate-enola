//! Build script for the `bundled-enola` feature (PLAN.md Phase 3).
//!
//! Without the feature it does nothing: ordinary builds and tests need no
//! upstream asset. With it, `ENOLA_BIN` must name the Enola binary for the
//! build target, and its SHA-256 must equal that target's `binary_sha256` in
//! `enola.lock.toml`, so a release can only ever embed the pinned, verified
//! binary. `scripts/fetch-enola.sh <target>` downloads, verifies and prints it.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=enola.lock.toml");
    println!("cargo:rerun-if-env-changed=ENOLA_BIN");
    if std::env::var_os("CARGO_FEATURE_BUNDLED_ENOLA").is_none() {
        return;
    }

    let target = std::env::var("TARGET").expect("cargo sets TARGET");
    let key = lock_key(&target).unwrap_or_else(|| {
        panic!("bundled-enola: no Enola build for target {target} (supported: darwin/linux, x86_64/aarch64)")
    });
    let bin = std::env::var_os("ENOLA_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!(
                "bundled-enola needs ENOLA_BIN=<path to the enola binary for {key}>; \
             run `scripts/fetch-enola.sh {key}` to download and verify it"
            )
        });
    let bin = std::fs::canonicalize(&bin).unwrap_or_else(|e| {
        panic!(
            "bundled-enola: ENOLA_BIN={} cannot be read: {e}",
            bin.display()
        )
    });
    println!("cargo:rerun-if-changed={}", bin.display());

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let lock =
        std::fs::read_to_string(manifest_dir.join("enola.lock.toml")).expect("enola.lock.toml");
    let version = top_level(&lock, "version").expect("enola.lock.toml has no version");
    let expected = target_field(&lock, key, "binary_sha256")
        .unwrap_or_else(|| panic!("enola.lock.toml has no binary_sha256 for [targets.{key}]"));
    let actual = sha256_hex(&bin);
    if actual != expected {
        panic!(
            "bundled-enola: {} has SHA-256 {actual}, but enola.lock.toml pins {expected} for {key}; \
             refusing to embed an unverified Enola",
            bin.display()
        );
    }

    println!("cargo:rustc-env=ENOLA_BUNDLED_PATH={}", bin.display());
    println!("cargo:rustc-env=ENOLA_BUNDLED_SHA256={actual}");
    println!("cargo:rustc-env=ENOLA_BUNDLED_VERSION={version}");
    println!("cargo:rustc-env=ENOLA_BUNDLED_TARGET={key}");
}

/// The `[targets.*]` key of `enola.lock.toml` for a Rust target triple.
fn lock_key(target: &str) -> Option<&'static str> {
    let os = if target.contains("apple-darwin") {
        "darwin"
    } else if target.contains("linux") {
        "linux"
    } else {
        return None;
    };
    let arch = if target.starts_with("x86_64") {
        "amd64"
    } else if target.starts_with("aarch64") {
        "arm64"
    } else {
        return None;
    };
    Some(match (os, arch) {
        ("darwin", "arm64") => "darwin-arm64",
        ("darwin", "amd64") => "darwin-amd64",
        ("linux", "arm64") => "linux-arm64",
        _ => "linux-amd64",
    })
}

fn unquote(v: &str) -> String {
    v.split('#')
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .to_string()
}

fn top_level(lock: &str, key: &str) -> Option<String> {
    lock.lines()
        .take_while(|l| !l.trim_start().starts_with('['))
        .find_map(|l| {
            l.split_once('=')
                .filter(|(k, _)| k.trim() == key)
                .map(|(_, v)| unquote(v))
        })
}

fn target_field(lock: &str, target: &str, key: &str) -> Option<String> {
    let header = format!("[targets.{target}]");
    let mut inside = false;
    for line in lock.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            inside = t == header;
            continue;
        }
        if inside {
            if let Some((k, v)) = t.split_once('=') {
                if k.trim() == key {
                    return Some(unquote(v));
                }
            }
        }
    }
    None
}

fn sha256_hex(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

//! Every `ureq` agent in the workspace must take its TLS setting from `craft_ai_http`.
//!
//! Reads the source of every crate and checks that each `Agent::config_builder()` is followed by
//! `.tls_config(craft_ai_http::tls_config())`. A new adapter that forgets it fails here instead of
//! failing on a customer's machine behind a re-signing proxy.

use std::fs;
use std::path::{Path, PathBuf};

const BUILDER: &str = "ureq::Agent::config_builder()";
const REQUIRED: &str = ".tls_config(craft_ai_http::tls_config())";

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The builder call and whatever follows it, spaces and line breaks removed.
fn after_builder(text: &str) -> Vec<String> {
    text.match_indices(BUILDER)
        .map(|(at, _)| {
            let rest = &text[at + BUILDER.len()..];
            rest.chars()
                .filter(|c| !c.is_whitespace())
                .take(REQUIRED.len())
                .collect()
        })
        .collect()
}

#[test]
fn every_agent_uses_the_shared_tls_setting() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for entry in fs::read_dir(&crates).into_iter().flatten().flatten() {
        // Only library and binary sources. Tests build agents to talk to local servers, and
        // this crate's own tests build one to check the setting itself.
        if entry.file_name() == "ai-http" {
            continue;
        }
        rust_files(&entry.path().join("src"), &mut files);
    }
    let mut sites = 0;
    let mut missing = Vec::new();
    for file in files {
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        for following in after_builder(&text) {
            sites += 1;
            if !following.starts_with(REQUIRED) {
                missing.push(file.display().to_string());
            }
        }
    }
    assert!(
        sites >= 10,
        "found only {sites} agents; the scan is looking in the wrong place"
    );
    assert!(
        missing.is_empty(),
        "agents without the shared TLS setting: {missing:?}"
    );
}

#[test]
fn the_scan_recognises_a_site_with_and_without_the_setting() {
    let good = "let c = ureq::Agent::config_builder()\n    .tls_config(craft_ai_http::tls_config())\n    .build();";
    let bad = "let c = ureq::Agent::config_builder()\n    .timeout_global(None)\n    .build();";
    assert!(after_builder(good)[0].starts_with(REQUIRED));
    assert!(!after_builder(bad)[0].starts_with(REQUIRED));
}

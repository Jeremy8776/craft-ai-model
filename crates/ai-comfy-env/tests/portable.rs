//! Portable ComfyUI unpacking with tiny local archives. No network or user installation.
use std::io::Cursor;
use std::sync::atomic::AtomicBool;

use craft_ai_comfy_env::portable::{ExtractLimits, extract_7z, headless_launch, safe_relative};
use sevenz_rust2::{ArchiveEntry, ArchiveWriter};

fn archive(entries: &[(&str, &[u8])], path: &std::path::Path) {
    let mut writer = ArchiveWriter::create(path).unwrap();
    for (name, bytes) in entries {
        writer
            .push_archive_entry(ArchiveEntry::new_file(name), Some(Cursor::new(*bytes)))
            .unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn ordinary_archive_unpacks_to_a_fresh_folder_and_counts_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("build.7z");
    archive(
        &[
            ("ComfyUI_windows_portable/ComfyUI/main.py", b"hello"),
            ("ComfyUI_windows_portable/python_embeded/python.exe", b"py"),
        ],
        &src,
    );
    let dest = dir.path().join("new");
    let result = extract_7z(
        &src,
        &dest,
        &ExtractLimits::default(),
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(result.files, 2);
    assert_eq!(result.bytes, 7);
    assert_eq!(
        std::fs::read(dest.join("ComfyUI_windows_portable/ComfyUI/main.py")).unwrap(),
        b"hello"
    );
}

#[test]
fn suspicious_names_never_escape_or_replace_anything() {
    for name in [
        "../escape",
        "/absolute",
        "C:/drive",
        "x/../escape",
        "x\\escape",
        "con.txt",
        "a//b",
        "a./b",
        "x/aux",
        "x/a?.exe",
        "x/a*.exe",
        "x/a|b",
        "x/<bad>",
        ".",
    ] {
        assert!(safe_relative(name).is_none(), "{name}");
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("evil.7z");
    archive(&[("../escape", b"BAD")], &src);
    assert!(
        extract_7z(
            &src,
            &dir.path().join("new"),
            &ExtractLimits::default(),
            &AtomicBool::new(false),
            &mut |_| {}
        )
        .is_err()
    );
    assert!(!dir.path().join("escape").exists());
}

#[test]
fn a_bad_entry_in_an_earlier_block_never_allows_later_files() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("evil.7z");
    archive(
        &[("../escape", b"bad"), ("good/file", b"should never arrive")],
        &src,
    );
    let dest = dir.path().join("new");
    assert!(
        extract_7z(
            &src,
            &dest,
            &ExtractLimits::default(),
            &AtomicBool::new(false),
            &mut |_| {}
        )
        .is_err()
    );
    assert!(!dest.join("good/file").exists());
}

#[test]
fn limits_and_cancel_stop_extraction() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("build.7z");
    archive(&[("a", b"12345"), ("b", b"12345")], &src);
    let limits = ExtractLimits {
        max_files: 1,
        max_bytes: 100,
        max_file: 100,
    };
    assert!(
        extract_7z(
            &src,
            &dir.path().join("one"),
            &limits,
            &AtomicBool::new(false),
            &mut |_| {}
        )
        .is_err()
    );
    assert!(!dir.path().join("one/b").exists());
    let limits = ExtractLimits {
        max_files: 100,
        max_bytes: 3,
        max_file: 100,
    };
    assert!(
        extract_7z(
            &src,
            &dir.path().join("two"),
            &limits,
            &AtomicBool::new(false),
            &mut |_| {}
        )
        .is_err()
    );
    assert!(
        extract_7z(
            &src,
            &dir.path().join("three"),
            &ExtractLimits::default(),
            &AtomicBool::new(true),
            &mut |_| {}
        )
        .is_err()
    );
}

#[test]
fn names_differing_only_in_case_cannot_overwrite_on_windows() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("build.7z");
    archive(&[("A", b"one"), ("a", b"two")], &src);
    assert!(
        extract_7z(
            &src,
            &dir.path().join("new"),
            &ExtractLimits::default(),
            &AtomicBool::new(false),
            &mut |_| {}
        )
        .is_err()
    );
}

#[test]
fn a_non_official_build_is_rejected_before_any_file_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let fake = craft_ai_features::PortableBuild {
        asset: "ComfyUI_windows_portable_nvidia.7z".into(),
        url: "http://127.0.0.1:1/evil.7z".into(),
        size_bytes: 1,
        sha256: "f".repeat(64),
    };
    let root = dir.path().join("app-data");
    let result = craft_ai_comfy_env::portable::install_portable(
        &fake,
        &root,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    assert!(result.is_err());
    assert!(!root.exists());
}

#[test]
fn launch_only_accepts_connections_from_this_computer() {
    let s = headless_launch(std::path::Path::new("C:/app-data"), 8134, false);
    assert!(
        s.program
            .ends_with("ComfyUI_windows_portable/python_embeded/python.exe")
    );
    assert!(s.args.windows(2).any(|w| w == ["--listen", "127.0.0.1"]));
    assert!(s.args.windows(2).any(|w| w == ["--port", "8134"]));
    assert!(s.args.contains(&"--disable-auto-launch".to_owned()));
    assert!(!s.args.contains(&"--cpu".to_owned()));
}

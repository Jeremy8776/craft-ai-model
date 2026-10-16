#![allow(dead_code)]
#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use craft_ai_comfy_env::{DownloadConfig, Downloader, Phase, Progress, verify_file};
use craft_ai_core::{ErrorKind, SecretString};
use craft_ai_features::{Licence, ModelFile, Source};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use support::{Reply, Server, http_bytes};

fn body() -> Vec<u8> {
    (0..2_621_440u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect()
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn model(bytes: &[u8], url: &str) -> ModelFile {
    ModelFile {
        folder: "sam3".into(),
        filename: "m.safetensors".into(),
        size_bytes: bytes.len() as u64,
        sha256: sha(bytes),
        source: Source::Url { url: url.into() },
        licence: Licence {
            name: "test".into(),
            url: None,
            accept_required: false,
            note: None,
        },
    }
}

fn downloader() -> Downloader {
    Downloader::new(DownloadConfig {
        allow_loopback: true,
        retry_delay: std::time::Duration::ZERO,
        auth_host: "127.0.0.1".into(),
        ..DownloadConfig::default()
    })
}

fn origin(s: &Server) -> String {
    s.url.trim_end_matches("/v1").to_owned()
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

/// Serves `data`, honouring a `Range: bytes=N-` header when asked to.
fn range_server(data: Vec<u8>, honour_range: bool) -> Server {
    Server::start(
        move |r| match r.header("range").and_then(|v| v.strip_prefix("bytes=")) {
            Some(range) if honour_range => {
                let from: usize = range.trim_end_matches('-').parse().unwrap();
                let tail = &data[from..];
                let cr = format!("bytes {from}-{}/{}", data.len() - 1, data.len());
                http_bytes(206, &[("Content-Range", cr.as_str())], tail)
            }
            _ => http_bytes(200, &[], &data),
        },
    )
}

fn run(
    d: &Downloader,
    file: &ModelFile,
    dir: &Path,
    token: Option<&SecretString>,
    cancel: &AtomicBool,
) -> (
    Result<std::path::PathBuf, craft_ai_core::AiError>,
    Vec<Progress>,
) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let result = d.fetch(file, dir, token, cancel, &mut |p| {
        sink.lock().unwrap().push(p)
    });
    let progress = seen.lock().unwrap().clone();
    (result, progress)
}

#[test]
fn a_download_is_checked_and_lands_under_its_real_name() {
    let data = body();
    let server = range_server(data.clone(), true);
    let dir = tempfile::tempdir().unwrap();
    let file = model(&data, &format!("{}/m", origin(&server)));
    let (result, progress) = run(
        &downloader(),
        &file,
        dir.path(),
        None,
        &AtomicBool::new(false),
    );
    let path = result.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), data);
    assert_eq!(names(dir.path()), ["m.safetensors"]);
    // Progress only moves forward, reaches the total, then the check runs to the total too.
    let down: Vec<_> = progress
        .iter()
        .filter(|p| p.phase == Phase::Downloading)
        .collect();
    assert!(down.len() >= 2 && down.windows(2).all(|w| w[0].done < w[1].done));
    assert_eq!(down.last().unwrap().done, data.len() as u64);
    assert_eq!(progress.last().unwrap().phase, Phase::Verifying);
    assert_eq!(progress.last().unwrap().done, data.len() as u64);
}

#[test]
fn a_file_already_in_place_is_not_downloaded_again() {
    let data = body();
    let server = range_server(data.clone(), true);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("m.safetensors"), &data).unwrap();
    let file = model(&data, &format!("{}/m", origin(&server)));
    run(
        &downloader(),
        &file,
        dir.path(),
        None,
        &AtomicBool::new(false),
    )
    .0
    .unwrap();
    assert_eq!(server.count(), 0);
}

#[test]
fn an_interrupted_download_resumes_from_its_part_file() {
    let data = body();
    let server = range_server(data.clone(), true);
    let dir = tempfile::tempdir().unwrap();
    let half = data.len() / 2;
    std::fs::write(dir.path().join("m.safetensors.part"), &data[..half]).unwrap();
    let file = model(&data, &format!("{}/m", origin(&server)));
    let path = run(
        &downloader(),
        &file,
        dir.path(),
        None,
        &AtomicBool::new(false),
    )
    .0
    .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), data);
    let requests = server.recorded();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].header("range"),
        Some(format!("bytes={half}-").as_str())
    );
}

#[test]
fn a_server_that_ignores_the_range_makes_it_start_again() {
    let data = body();
    let server = range_server(data.clone(), false);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("m.safetensors.part"), &data[..1000]).unwrap();
    let file = model(&data, &format!("{}/m", origin(&server)));
    let path = run(
        &downloader(),
        &file,
        dir.path(),
        None,
        &AtomicBool::new(false),
    )
    .0
    .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), data);
}

#[test]
fn a_connection_that_drops_midway_is_resumed_automatically() {
    let data = body();
    let hits = Arc::new(Mutex::new(0u32));
    let counter = Arc::clone(&hits);
    let served = data.clone();
    let server = Server::start(move |r| {
        let mut n = counter.lock().unwrap();
        *n += 1;
        let from: usize = r
            .header("range")
            .and_then(|v| v.strip_prefix("bytes="))
            .map_or(0, |v| v.trim_end_matches('-').parse().unwrap());
        let cr = format!("bytes {from}-{}/{}", served.len() - 1, served.len());
        let tail = &served[from..];
        if *n == 1 {
            // Promise everything, deliver 1 MiB, then the connection closes.
            let mut raw = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                tail.len()
            )
            .into_bytes();
            raw.extend_from_slice(&tail[..1024 * 1024]);
            Reply::Raw(raw)
        } else {
            http_bytes(206, &[("Content-Range", cr.as_str())], tail)
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let file = model(&data, &format!("{}/m", origin(&server)));
    let path = run(
        &downloader(),
        &file,
        dir.path(),
        None,
        &AtomicBool::new(false),
    )
    .0
    .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), data);
    assert_eq!(*hits.lock().unwrap(), 2);
    assert_eq!(names(dir.path()), ["m.safetensors"]);
}

#[test]
fn a_connection_that_never_delivers_gives_up_and_keeps_nothing_misleading() {
    let data = body();
    let server = Server::start(move |_| {
        Reply::Raw(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2621440\r\nConnection: close\r\n\r\n".to_vec(),
        )
    });
    let dir = tempfile::tempdir().unwrap();
    let file = model(&data, &format!("{}/m", origin(&server)));
    let err = run(
        &downloader(),
        &file,
        dir.path(),
        None,
        &AtomicBool::new(false),
    )
    .0
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Network);
    assert!(!dir.path().join("m.safetensors").exists());
}

#[test]
fn a_wrong_file_is_deleted_not_installed() {
    let data = body();
    let mut wrong = data.clone();
    wrong[100] ^= 0xFF;
    let server = range_server(wrong, true);
    let dir = tempfile::tempdir().unwrap();
    let file = model(&data, &format!("{}/m", origin(&server)));
    let err = run(
        &downloader(),
        &file,
        dir.path(),
        None,
        &AtomicBool::new(false),
    )
    .0
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Provider);
    assert!(err.message.contains("checksum"));
    assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
}

#[test]
fn cancelling_keeps_the_part_file_for_later() {
    let data = body();
    let server = range_server(data.clone(), true);
    let dir = tempfile::tempdir().unwrap();
    let file = model(&data, &format!("{}/m", origin(&server)));
    let cancel = AtomicBool::new(false);
    let result = downloader().fetch(&file, dir.path(), None, &cancel, &mut |p| {
        if p.phase == Phase::Downloading {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(names(dir.path()), ["m.safetensors.part"]);
    // A later run finishes it.
    let path = run(
        &downloader(),
        &file,
        dir.path(),
        None,
        &AtomicBool::new(false),
    )
    .0
    .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), data);
}

#[test]
fn the_token_goes_to_the_named_host_and_not_to_where_it_redirects() {
    let data = body();
    let storage = range_server(data.clone(), true);
    let storage_port = storage
        .url
        .split(':')
        .nth(2)
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .to_owned();
    // `localhost` and `127.0.0.1` are the same machine but different hosts to the rules.
    let target = format!("http://localhost:{storage_port}/blob");
    let front = Server::start(move |_| http_bytes(302, &[("Location", target.as_str())], b""));
    let dir = tempfile::tempdir().unwrap();
    let file = model(&data, &format!("{}/m", origin(&front)));
    let token = SecretString::new("hf_needle_token");
    run(
        &downloader(),
        &file,
        dir.path(),
        Some(&token),
        &AtomicBool::new(false),
    )
    .0
    .unwrap();
    assert_eq!(
        front.recorded()[0].header("authorization"),
        Some("Bearer hf_needle_token")
    );
    assert!(storage.recorded()[0].header("authorization").is_none());
}

#[test]
fn refusals_are_named_and_leave_no_files() {
    let data = body();
    for (status, kind) in [
        (401, ErrorKind::Auth),
        (403, ErrorKind::Auth),
        (404, ErrorKind::Provider),
        (500, ErrorKind::Provider),
    ] {
        let server = Server::start(move |_| http_bytes(status, &[], b"no"));
        let dir = tempfile::tempdir().unwrap();
        let file = model(&data, &format!("{}/m", origin(&server)));
        let err = run(
            &downloader(),
            &file,
            dir.path(),
            None,
            &AtomicBool::new(false),
        )
        .0
        .unwrap_err();
        assert_eq!(err.kind, kind, "{status}");
        assert!(names(dir.path()).is_empty(), "{status}");
    }
}

#[test]
fn a_source_of_the_wrong_size_is_refused_before_writing() {
    let data = body();
    let server = range_server(vec![0u8; data.len() + 10], true);
    let dir = tempfile::tempdir().unwrap();
    let file = model(&data, &format!("{}/m", origin(&server)));
    let err = run(
        &downloader(),
        &file,
        dir.path(),
        None,
        &AtomicBool::new(false),
    )
    .0
    .unwrap_err();
    assert!(err.message.contains("different file size"));
    assert!(names(dir.path()).is_empty() || names(dir.path()) == ["m.safetensors.part"]);
    assert!(!dir.path().join("m.safetensors").exists());
}

#[test]
fn internal_and_plain_http_sources_are_refused_by_default() {
    let data = body();
    let server = range_server(data.clone(), true);
    let dir = tempfile::tempdir().unwrap();
    let file = model(&data, &format!("{}/m", origin(&server)));
    let strict = Downloader::new(DownloadConfig::default());
    let err = run(&strict, &file, dir.path(), None, &AtomicBool::new(false))
        .0
        .unwrap_err();
    assert!(err.message.contains("refused"));
    assert_eq!(server.count(), 0);
}

#[test]
fn file_names_that_could_escape_the_folder_are_refused() {
    let data = body();
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "../evil.safetensors",
        "a/b.safetensors",
        "a\\b.safetensors",
        ".hidden",
        "",
        "C:x",
    ] {
        let mut file = model(&data, "https://example.com/m");
        file.filename = name.into();
        let err = run(
            &downloader(),
            &file,
            dir.path(),
            None,
            &AtomicBool::new(false),
        )
        .0
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidRequest, "{name}");
    }
    assert!(names(dir.path()).is_empty());
}

#[test]
fn verify_catches_a_changed_or_truncated_file() {
    let data = body();
    let dir = tempfile::tempdir().unwrap();
    let file = model(&data, "https://example.com/m");
    let path = dir.path().join("m.safetensors");
    std::fs::write(&path, &data).unwrap();
    let cancel = AtomicBool::new(false);
    verify_file(&path, &file, &cancel, &mut |_| {}).unwrap();
    let mut bad = data.clone();
    bad[5] ^= 1;
    std::fs::write(&path, &bad).unwrap();
    assert!(verify_file(&path, &file, &cancel, &mut |_| {}).is_err());
    std::fs::write(&path, &data[..data.len() - 1]).unwrap();
    assert!(verify_file(&path, &file, &cancel, &mut |_| {}).is_err());
    assert!(verify_file(&dir.path().join("none"), &file, &cancel, &mut |_| {}).is_err());
}

#[test]
fn verify_refuses_a_wrong_sized_file_without_reading_it() {
    let data = body();
    let dir = tempfile::tempdir().unwrap();
    let file = model(&data, "https://example.com/m");
    let path = dir.path().join("m.safetensors");
    let mut longer = data.clone();
    longer.extend_from_slice(b"extra");
    std::fs::write(&path, &longer).unwrap();
    let mut calls = 0;
    assert!(verify_file(&path, &file, &AtomicBool::new(false), &mut |_| calls += 1).is_err());
    assert_eq!(calls, 0, "a file of the wrong size is not worth hashing");
}

#[test]
fn verify_can_be_cancelled() {
    let data = body();
    let dir = tempfile::tempdir().unwrap();
    let file = model(&data, "https://example.com/m");
    let path = dir.path().join("m.safetensors");
    std::fs::write(&path, &data).unwrap();
    let cancel = AtomicBool::new(false);
    let err = verify_file(&path, &file, &cancel, &mut |_| {
        cancel.store(true, Ordering::Relaxed)
    })
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled);
}

#[test]
fn a_part_file_bigger_than_the_model_is_thrown_away_and_the_download_starts_again() {
    let data = body();
    let server = range_server(data.clone(), true);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("m.safetensors.part"),
        vec![9u8; data.len() + 10],
    )
    .unwrap();
    let file = model(&data, &format!("{}/m", origin(&server)));
    let path = run(
        &downloader(),
        &file,
        dir.path(),
        None,
        &AtomicBool::new(false),
    )
    .0
    .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), data);
    assert!(
        server.recorded()[0].header("range").is_none(),
        "no range from a part file that cannot be right"
    );
}

#[test]
fn a_server_that_resumes_at_the_wrong_place_is_refused_by_name() {
    let data = body();
    let served = data.clone();
    let server = Server::start(move |r| {
        let from: usize = r
            .header("range")
            .and_then(|v| v.strip_prefix("bytes="))
            .map_or(0, |v| v.trim_end_matches('-').parse().unwrap());
        // Claims to continue at 0 whatever was asked for.
        let cr = format!("bytes 0-{}/{}", served.len() - 1, served.len());
        http_bytes(206, &[("Content-Range", cr.as_str())], &served[from..])
    });
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("m.safetensors.part"), &data[..1000]).unwrap();
    let file = model(&data, &format!("{}/m", origin(&server)));
    let err = run(
        &downloader(),
        &file,
        dir.path(),
        None,
        &AtomicBool::new(false),
    )
    .0
    .unwrap_err();
    assert!(err.message.contains("wrong place"), "{}", err.message);
}

#[test]
fn a_source_that_keeps_sending_past_the_expected_size_is_stopped() {
    let data = body();
    let mut longer = data.clone();
    longer.extend_from_slice(&[1u8; 5000]);
    // No Content-Length, so only the running count can notice.
    let server = Server::start(move |_| {
        let mut raw = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        raw.extend_from_slice(&longer);
        Reply::Raw(raw)
    });
    let dir = tempfile::tempdir().unwrap();
    let file = model(&data, &format!("{}/m", origin(&server)));
    let err = run(
        &downloader(),
        &file,
        dir.path(),
        None,
        &AtomicBool::new(false),
    )
    .0
    .unwrap_err();
    assert!(
        err.message.contains("more than the expected size"),
        "{}",
        err.message
    );
    assert!(!dir.path().join("m.safetensors").exists());
}

#[test]
fn a_source_that_redirects_forever_is_given_up_on() {
    let server = Server::start(|_| http_bytes(302, &[("Location", "/again")], b""));
    let dir = tempfile::tempdir().unwrap();
    let data = body();
    let file = model(&data, &format!("{}/m", origin(&server)));
    let err = run(
        &downloader(),
        &file,
        dir.path(),
        None,
        &AtomicBool::new(false),
    )
    .0
    .unwrap_err();
    assert!(
        err.message.contains("redirects too often"),
        "{}",
        err.message
    );
    assert_eq!(
        server.count(),
        6,
        "the first request and five redirects, no more"
    );
}

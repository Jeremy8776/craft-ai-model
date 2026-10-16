#![allow(dead_code)]
//! The downloader's extra rules for user-chosen sources: an allow-list of hosts that applies
//! to every hop, a download whose checksum nobody published (observed, not verified), and a size
//! probe. Scripted localhost servers only.
#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use std::path::Path;
use std::sync::atomic::AtomicBool;

use craft_ai_comfy_env::{DownloadConfig, Downloader};
use craft_ai_core::{ErrorKind, SecretString};
use craft_ai_features::{Licence, ModelFile, Source};
use sha2::{Digest, Sha256};
use support::{Reply, Server, http_bytes};

const TOKEN: &str = "tok-needle-123";

fn bytes() -> Vec<u8> {
    (0..300_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect()
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn file(data: &[u8], url: &str, sha256: &str) -> ModelFile {
    ModelFile {
        folder: "loras".into(),
        filename: "m.safetensors".into(),
        size_bytes: data.len() as u64,
        sha256: sha256.into(),
        source: Source::Url { url: url.into() },
        licence: Licence {
            name: "t".into(),
            url: None,
            accept_required: false,
            note: None,
        },
    }
}

fn config(allowed: &[&str]) -> DownloadConfig {
    DownloadConfig {
        allow_loopback: true,
        retry_delay: std::time::Duration::ZERO,
        auth_host: "127.0.0.1".into(),
        allowed_hosts: Some(allowed.iter().map(|h| (*h).to_owned()).collect()),
        ..DownloadConfig::default()
    }
}

fn origin(s: &Server) -> String {
    s.url.trim_end_matches("/v1").to_owned()
}

fn as_localhost(s: &Server) -> String {
    origin(s).replace("127.0.0.1", "localhost")
}

fn fetch(
    d: &Downloader,
    f: &ModelFile,
    dir: &Path,
    token: Option<&SecretString>,
) -> Result<std::path::PathBuf, craft_ai_core::AiError> {
    d.fetch(f, dir, token, &AtomicBool::new(false), &mut |_| {})
}

fn names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect()
}

#[test]
fn a_first_address_off_the_allowed_hosts_is_refused_before_any_request() {
    let data = bytes();
    let server = Server::start({
        let data = data.clone();
        move |_| http_bytes(200, &[], &data)
    });
    let dir = tempfile::tempdir().unwrap();
    let f = file(&data, &format!("{}/f", as_localhost(&server)), &sha(&data));
    let e = fetch(
        &Downloader::new(config(&["127.0.0.1"])),
        &f,
        dir.path(),
        None,
    )
    .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Provider);
    assert!(e.message.contains("not one of the hosts"), "{}", e.message);
    assert_eq!(server.count(), 0);
    assert!(names(dir.path()).is_empty());
}

#[test]
fn a_redirect_off_the_allowed_hosts_is_not_followed() {
    let elsewhere = Server::start(|_| http_bytes(200, &[], b"stolen"));
    let target = format!("{}/f", as_localhost(&elsewhere));
    let server = Server::start(move |_| {
        Reply::JsonWith(302, vec![("Location", target.clone())], String::new())
    });
    let data = bytes();
    let dir = tempfile::tempdir().unwrap();
    let f = file(&data, &format!("{}/f", origin(&server)), &sha(&data));
    let e = fetch(
        &Downloader::new(config(&["127.0.0.1"])),
        &f,
        dir.path(),
        None,
    )
    .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Provider);
    assert_eq!(
        elsewhere.count(),
        0,
        "the redirect target must not be contacted"
    );
    assert!(names(dir.path()).is_empty());
}

#[test]
fn a_redirect_to_an_allowed_storage_host_works_and_carries_no_token() {
    let data = bytes();
    let storage = Server::start({
        let data = data.clone();
        move |_| http_bytes(200, &[], &data)
    });
    let target = format!("{}/blob", as_localhost(&storage));
    let front = Server::start(move |_| {
        Reply::JsonWith(302, vec![("Location", target.clone())], String::new())
    });
    let dir = tempfile::tempdir().unwrap();
    let f = file(&data, &format!("{}/f", origin(&front)), &sha(&data));
    let d = Downloader::new(config(&["127.0.0.1", "localhost"]));
    let token = SecretString::new(TOKEN);
    let path = fetch(&d, &f, dir.path(), Some(&token)).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), data);
    assert_eq!(
        front.recorded()[0].header("authorization"),
        Some(&*format!("Bearer {TOKEN}"))
    );
    assert_eq!(storage.recorded()[0].header("authorization"), None);
}

#[test]
fn without_an_allow_list_the_existing_behaviour_is_unchanged() {
    let data = bytes();
    let server = Server::start({
        let data = data.clone();
        move |_| http_bytes(200, &[], &data)
    });
    let dir = tempfile::tempdir().unwrap();
    let f = file(&data, &format!("{}/f", as_localhost(&server)), &sha(&data));
    let mut c = config(&[]);
    c.allowed_hosts = None;
    assert!(fetch(&Downloader::new(c), &f, dir.path(), None).is_ok());
}

#[test]
fn an_unverified_download_reports_the_checksum_it_observed() {
    let data = bytes();
    let server = Server::start({
        let data = data.clone();
        move |_| http_bytes(200, &[], &data)
    });
    let dir = tempfile::tempdir().unwrap();
    // The recipe's own checksum field is ignored here, whatever it holds.
    let f = file(&data, &format!("{}/f", origin(&server)), "");
    let d = Downloader::new(config(&["127.0.0.1"]));
    let (path, seen) = d
        .fetch_unverified(&f, dir.path(), None, &AtomicBool::new(false), &mut |_| {})
        .unwrap();
    assert_eq!(seen, sha(&data));
    assert_eq!(std::fs::read(&path).unwrap(), data);
    assert_eq!(names(dir.path()), ["m.safetensors"]);
}

#[test]
fn an_unverified_download_still_enforces_the_size_it_was_promised() {
    let data = bytes();
    let server = Server::start({
        let data = data.clone();
        move |_| http_bytes(200, &[], &data)
    });
    let dir = tempfile::tempdir().unwrap();
    let mut f = file(&data, &format!("{}/f", origin(&server)), "");
    f.size_bytes -= 1;
    let d = Downloader::new(config(&["127.0.0.1"]));
    assert!(
        d.fetch_unverified(&f, dir.path(), None, &AtomicBool::new(false), &mut |_| {})
            .is_err()
    );
    assert!(!names(dir.path()).contains(&"m.safetensors".to_owned()));
}

#[test]
fn the_exact_size_comes_from_a_one_byte_range_request() {
    let data = bytes();
    let ranged = Server::start({
        let data = data.clone();
        move |r| {
            assert_eq!(r.header("range"), Some("bytes=0-0"));
            let cr = format!("bytes 0-0/{}", data.len());
            http_bytes(206, &[("Content-Range", cr.as_str())], &data[..1])
        }
    });
    let d = Downloader::new(config(&["127.0.0.1"]));
    assert_eq!(
        d.probe_size(&format!("{}/f", origin(&ranged)), None)
            .unwrap(),
        data.len() as u64
    );

    // A server that ignores the range and sends everything: the length is still readable.
    let whole = Server::start({
        let data = data.clone();
        move |_| http_bytes(200, &[], &data)
    });
    assert_eq!(
        d.probe_size(&format!("{}/f", origin(&whole)), None)
            .unwrap(),
        data.len() as u64
    );

    // No length at all: refused, not guessed.
    let unknown =
        Server::start(|_| Reply::Raw(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nabc".to_vec()));
    assert!(
        d.probe_size(&format!("{}/f", origin(&unknown)), None)
            .is_err()
    );
    // Nothing to measure: a file of no bytes is not a model.
    let empty = Server::start(|_| http_bytes(200, &[], b""));
    assert!(
        d.probe_size(&format!("{}/f", origin(&empty)), None)
            .is_err()
    );
    // A refusal is reported as one.
    let denied = Server::start(|_| Reply::Json(403, "{}".into()));
    assert_eq!(
        d.probe_size(&format!("{}/f", origin(&denied)), None)
            .unwrap_err()
            .kind,
        ErrorKind::Auth
    );
}

#[test]
fn the_size_probe_obeys_the_same_host_and_token_rules() {
    let data = bytes();
    let elsewhere = Server::start(|_| http_bytes(200, &[], b"x"));
    let target = format!("{}/f", as_localhost(&elsewhere));
    let front = Server::start(move |_| {
        Reply::JsonWith(302, vec![("Location", target.clone())], String::new())
    });
    let d = Downloader::new(config(&["127.0.0.1"]));
    let token = SecretString::new(TOKEN);
    assert!(
        d.probe_size(&format!("{}/f", origin(&front)), Some(&token))
            .is_err()
    );
    assert_eq!(elsewhere.count(), 0);
    assert_eq!(
        front.recorded()[0].header("authorization"),
        Some(&*format!("Bearer {TOKEN}"))
    );
    let _ = data;
}

#[test]
fn a_whole_download_is_read_into_memory_only_up_to_its_limit() {
    let data = bytes();
    let d = Downloader::new(config(&["127.0.0.1"]));
    let sized = Server::start({
        let data = data.clone();
        move |_| http_bytes(200, &[], &data)
    });
    let url = format!("{}/f", origin(&sized));
    assert_eq!(d.get_bytes(&url, data.len() as u64, None).unwrap(), data);
    assert!(d.get_bytes(&url, data.len() as u64 - 1, None).is_err());

    // No stated length: the cap still holds as the bytes arrive.
    let mut raw = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
    raw.extend_from_slice(&data);
    let streamed = Server::start(move |_| Reply::Raw(raw.clone()));
    let url = format!("{}/f", origin(&streamed));
    assert_eq!(
        d.get_bytes(&url, data.len() as u64, None).unwrap().len(),
        data.len()
    );
    assert!(d.get_bytes(&url, data.len() as u64 - 1, None).is_err());

    // A refusal and an off-list address are errors, not empty bodies.
    let denied = Server::start(|_| Reply::Json(403, "{}".into()));
    let e = d
        .get_bytes(&format!("{}/f", origin(&denied)), 10, None)
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Auth);
    assert!(
        d.get_bytes(&format!("{}/f", as_localhost(&sized)), 10_000_000, None)
            .is_err()
    );
}

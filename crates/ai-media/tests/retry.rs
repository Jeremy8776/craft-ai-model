//! p10: a busy media host is tried again a few times, an expired or refused link is not, and the
//! store never waits longer than its retry budget. Scripted localhost servers only; the pauses are
//! set to zero except where a test is about the pause itself.
#![allow(dead_code)]
#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use craft_ai_core::{AiError, ErrorKind, Payload, PayloadHandle};
use craft_ai_media::{MediaStore, MediaStoreConfig};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use support::{Reply, Server, http_bytes};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR-fake-body";

type Answer = Box<dyn Fn() -> Reply + Send + Sync>;

fn config(dir: &Path) -> MediaStoreConfig {
    let mut cfg = MediaStoreConfig::new(dir);
    cfg.allow_loopback = true;
    cfg.retry_delay = Duration::ZERO;
    cfg
}

fn link(server: &Server) -> PayloadHandle {
    let uri = format!("{}/media/out.png", server.url.trim_end_matches("/v1"));
    match Payload::handle(&uri, "image/png", None).unwrap() {
        Payload::Handle(handle) => handle,
        _ => unreachable!(),
    }
}

fn save(cfg: MediaStoreConfig, server: &Server) -> Result<PayloadHandle, AiError> {
    MediaStore::new(cfg).unwrap().save(&link(server))
}

fn files(dir: &Path) -> usize {
    std::fs::read_dir(dir).unwrap().count()
}

/// A host that answers `bad` this many times, then serves the picture.
fn busy_then_ok(bad: usize, answer: impl Fn() -> Reply + Send + Sync + 'static) -> Server {
    let seen = Arc::new(AtomicUsize::new(0));
    Server::start(move |_| {
        if seen.fetch_add(1, Ordering::SeqCst) < bad {
            answer()
        } else {
            http_bytes(200, &[], PNG)
        }
    })
}

#[test]
fn a_busy_host_is_tried_again_until_it_answers() {
    for status in [408u16, 500, 502, 503, 504] {
        let dir = tempfile::tempdir().unwrap();
        let server = busy_then_ok(2, move || http_bytes(status, &[], b"busy"));
        let saved = save(config(dir.path()), &server).unwrap_or_else(|e| panic!("{status}: {e}"));
        assert_eq!(saved.media_type.as_str(), "image/png", "{status}");
        assert_eq!(
            server.count(),
            3,
            "{status}: two busy answers, then the file"
        );
        assert_eq!(files(dir.path()), 1, "{status}");
    }
}

#[test]
fn a_host_that_says_slow_down_is_asked_again_when_it_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let server = busy_then_ok(1, || http_bytes(429, &[("Retry-After", "0")], b""));
    save(config(dir.path()), &server).unwrap();
    assert_eq!(server.count(), 2);
}

#[test]
fn a_cut_transfer_is_tried_again_and_leaves_no_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    let server = busy_then_ok(1, || {
        // Promises 1000 bytes, sends 20, and closes.
        let mut head =
            b"HTTP/1.1 200 X\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n".to_vec();
        head.extend_from_slice(&PNG[..20.min(PNG.len())]);
        Reply::Raw(head)
    });
    save(config(dir.path()), &server).unwrap();
    assert_eq!(server.count(), 2);
    assert_eq!(files(dir.path()), 1, "only the finished copy is kept");
}

#[test]
fn an_unreachable_host_is_tried_again_and_then_reported() {
    let dir = tempfile::tempdir().unwrap();
    let uri = format!(
        "{}/media/out.png",
        Server::dead_url().trim_end_matches("/v1")
    );
    let handle = match Payload::handle(&uri, "image/png", None).unwrap() {
        Payload::Handle(handle) => handle,
        _ => unreachable!(),
    };
    let error = MediaStore::new(config(dir.path()))
        .unwrap()
        .save(&handle)
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(error.message.contains("after 3 tries"), "{error:?}");
}

#[test]
fn links_that_will_not_get_better_are_asked_for_once() {
    let cases: Vec<(&str, Answer)> = vec![
        ("bad request", Box::new(|| http_bytes(400, &[], b""))),
        ("unauthorised", Box::new(|| http_bytes(401, &[], b""))),
        ("expired", Box::new(|| http_bytes(403, &[], b"expired"))),
        ("gone", Box::new(|| http_bytes(404, &[], b""))),
        ("removed", Box::new(|| http_bytes(410, &[], b""))),
        (
            "an error page",
            Box::new(|| http_bytes(200, &[], b"<!DOCTYPE html><html>sorry</html>")),
        ),
        (
            "the wrong kind of media",
            Box::new(|| http_bytes(200, &[], b"\0\0\0\x18ftypisom-video-body-here")),
        ),
        (
            "a redirect off https to a refused address",
            Box::new(|| {
                Reply::JsonWith(
                    302,
                    vec![("Location", "http://10.0.0.1/a.png".into())],
                    String::new(),
                )
            }),
        ),
    ];
    for (why, answer) in cases {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(move |_| answer());
        let result = save(config(dir.path()), &server);
        assert!(result.is_err(), "{why}");
        assert_eq!(server.count(), 1, "{why}: asked once, not again");
        assert_eq!(files(dir.path()), 0, "{why}");
    }
}

#[test]
fn a_file_over_the_limit_is_not_fetched_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.max_bytes = 10;
    let server = Server::start(|_| http_bytes(200, &[], PNG));
    assert!(save(cfg, &server).is_err());
    assert_eq!(server.count(), 1);
}

#[test]
fn the_number_of_tries_is_bounded_and_can_be_one() {
    for (asked, made) in [(0u8, 1usize), (1, 1), (2, 2), (3, 3), (5, 5), (99, 5)] {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.max_attempts = asked;
        let server = Server::start(|_| http_bytes(503, &[], b""));
        let error = save(cfg, &server).unwrap_err();
        assert_eq!(server.count(), made, "asked for {asked}");
        assert_eq!(error.kind, ErrorKind::Network);
        assert_eq!(
            error.message.contains("tries"),
            made > 1,
            "{asked}: {error:?}"
        );
        assert!(error.message.contains("503"), "{error:?}");
    }
}

#[test]
fn the_pause_doubles_between_tries() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.retry_delay = Duration::from_millis(60);
    let server = Server::start(|_| http_bytes(503, &[], b""));
    let started = Instant::now();
    save(cfg, &server).unwrap_err();
    // 60 ms before the second try and 120 ms before the third: at least 180 ms in all.
    assert!(
        started.elapsed() >= Duration::from_millis(170),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(server.count(), 3);
}

#[test]
fn a_wait_that_would_overrun_the_budget_is_not_taken() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.retry_delay = Duration::from_millis(400);
    cfg.retry_budget = Duration::from_millis(100);
    let server = Server::start(|_| http_bytes(503, &[], b""));
    let started = Instant::now();
    save(cfg, &server).unwrap_err();
    assert_eq!(server.count(), 1, "gave up instead of waiting");
    assert!(started.elapsed() < Duration::from_millis(300));
}

#[test]
fn a_host_that_asks_for_a_long_wait_is_not_waited_for() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(|_| http_bytes(429, &[("Retry-After", "3600")], b""));
    let started = Instant::now();
    let error = save(config(dir.path()), &server).unwrap_err();
    assert_eq!(server.count(), 1);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(error.kind, ErrorKind::RateLimited);
    assert_eq!(error.retry_after_secs, Some(3600), "the caller can show it");
}

#[test]
fn every_try_goes_to_the_same_link_with_no_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let server = busy_then_ok(2, || http_bytes(503, &[], b""));
    save(config(dir.path()), &server).unwrap();
    for request in server.recorded() {
        assert_eq!(request.path, "/media/out.png");
        assert!(request.header("authorization").is_none());
        assert!(request.header("cookie").is_none());
    }
}

#[test]
fn the_last_answer_is_reported_with_what_the_host_asked() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(|_| http_bytes(429, &[("Retry-After", "0")], b""));
    let error = save(config(dir.path()), &server).unwrap_err();
    assert_eq!(server.count(), 3);
    assert_eq!(error.kind, ErrorKind::RateLimited);
    assert_eq!(error.retry_after_secs, Some(0));
    assert!(error.message.contains("after 3 tries"), "{error:?}");
}

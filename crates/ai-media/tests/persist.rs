#![allow(dead_code)]
#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use craft_ai_core::{
    AiError, ErrorKind, Event, JobId, JobSnapshot, JobState, ModelId, ModelInfo, Output, Payload,
    Provider, Request,
};
use craft_ai_media::{MediaStore, MediaStoreConfig, PersistingProvider};
use std::path::Path;
use std::sync::{Arc, Mutex};
use support::{Reply, Server, http_bytes};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR-fake-body";
const JPEG: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F', b'I', b'F', 0, 1, 2, 3, 4, 5,
];
const MP4: &[u8] = b"\0\0\0\x18ftypisom\0\0\0\0isomiso2-fake";

/// Answers every poll with the scripted outputs, once.
struct Fake {
    outputs: Mutex<Vec<Payload>>,
}

impl Fake {
    fn scripted(outputs: Vec<Payload>) -> Arc<dyn Provider> {
        Arc::new(Self {
            outputs: Mutex::new(outputs),
        })
    }
}

impl Provider for Fake {
    fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(Vec::new())
    }
    fn submit(&self, _: &ModelId, _: &Request) -> Result<JobId, AiError> {
        JobId::new("job-1").map_err(|e| AiError::invalid(e.to_string()))
    }
    fn poll(&self, _: &JobId) -> Result<JobSnapshot, AiError> {
        let mut snap = JobSnapshot::new(JobState::Succeeded);
        snap.events.push(Event::Notice {
            message: "from the provider".into(),
        });
        for payload in std::mem::take(&mut *self.outputs.lock().unwrap()) {
            snap.events.push(Event::Output(Output {
                payload,
                label: None,
            }));
        }
        Ok(snap)
    }
    fn cancel(&self, _: &JobId) -> Result<(), AiError> {
        Ok(())
    }
}

fn origin(server: &Server) -> String {
    server.url.trim_end_matches("/v1").to_owned()
}

fn config(dir: &Path) -> MediaStoreConfig {
    let mut cfg = MediaStoreConfig::new(dir);
    cfg.allow_loopback = true;
    cfg.retry_delay = std::time::Duration::ZERO;
    cfg.max_attempts = 1;
    cfg
}

fn run(cfg: MediaStoreConfig, outputs: Vec<Payload>) -> JobSnapshot {
    let store = Arc::new(MediaStore::new(cfg).unwrap());
    let provider = PersistingProvider::new(Fake::scripted(outputs), store);
    provider.poll(&JobId::new("job-1").unwrap()).unwrap()
}

fn handle(uri: &str, media: &str) -> Payload {
    Payload::handle(uri, media, None).unwrap()
}

fn files(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect()
}

fn notices(snap: &JobSnapshot) -> Vec<&str> {
    snap.events
        .iter()
        .filter_map(|e| match e {
            Event::Notice { message } => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

fn saved_path(snap: &JobSnapshot) -> std::path::PathBuf {
    match &snap.outputs().next().unwrap().payload {
        Payload::Handle(h) => {
            assert!(h.uri.starts_with("file:"), "{}", h.uri);
            url::Url::parse(&h.uri).unwrap().to_file_path().unwrap()
        }
        other => panic!("not a handle: {other:?}"),
    }
}

#[test]
fn a_hosted_image_is_copied_and_the_link_replaced() {
    let server = Server::start(|_| http_bytes(200, &[("Content-Type", "image/png")], PNG));
    let dir = tempfile::tempdir().unwrap();
    let uri = format!("{}/result.png?sig=secret-token", origin(&server));
    let snap = run(config(dir.path()), vec![handle(&uri, "image/png")]);

    let path = saved_path(&snap);
    assert_eq!(std::fs::read(&path).unwrap(), PNG);
    assert_eq!(path.extension().unwrap(), "png");
    assert!(path.starts_with(dir.path()));
    let Payload::Handle(h) = &snap.outputs().next().unwrap().payload else {
        panic!("expected a handle");
    };
    assert_eq!(h.media_type.as_str(), "image/png");
    assert_eq!(h.size_bytes, Some(PNG.len() as u64));
    // The provider's own events survive, and nothing was added.
    assert_eq!(notices(&snap), ["from the provider"]);
    assert_eq!(snap.state, JobState::Succeeded);
}

#[test]
fn the_transfer_carries_no_credentials() {
    let server = Server::start(|_| http_bytes(200, &[], MP4));
    let dir = tempfile::tempdir().unwrap();
    run(
        config(dir.path()),
        vec![handle(&format!("{}/v.mp4", origin(&server)), "video/mp4")],
    );
    let requests = server.recorded();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    for name in ["authorization", "cookie", "x-key", "x-api-key"] {
        assert!(requests[0].header(name).is_none(), "{name}");
    }
}

#[test]
fn the_file_type_comes_from_the_bytes_not_the_declaration() {
    let server = Server::start(|_| http_bytes(200, &[("Content-Type", "image/png")], JPEG));
    let dir = tempfile::tempdir().unwrap();
    let snap = run(
        config(dir.path()),
        vec![handle(&format!("{}/a", origin(&server)), "image/png")],
    );
    let path = saved_path(&snap);
    assert_eq!(path.extension().unwrap(), "jpg");
    let Payload::Handle(h) = &snap.outputs().next().unwrap().payload else {
        panic!("expected a handle");
    };
    assert_eq!(h.media_type.as_str(), "image/jpeg");
}

#[test]
fn a_different_kind_of_media_is_refused_and_leaves_no_file() {
    let server = Server::start(|_| http_bytes(200, &[], PNG));
    let dir = tempfile::tempdir().unwrap();
    let uri = format!("{}/a", origin(&server));
    let snap = run(config(dir.path()), vec![handle(&uri, "video/mp4")]);
    assert!(files(dir.path()).is_empty());
    assert_eq!(notices(&snap).len(), 2);
    // The original link is kept so the app can still use it before it expires.
    assert!(matches!(&snap.outputs().next().unwrap().payload, Payload::Handle(h) if h.uri == uri));
}

#[test]
fn error_pages_and_expired_links_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let html = Server::start(|_| http_bytes(200, &[], b"<!DOCTYPE html><html>nope</html>"));
    let snap = run(
        config(dir.path()),
        vec![handle(&format!("{}/a", origin(&html)), "image/png")],
    );
    assert!(
        notices(&snap)
            .iter()
            .any(|n| n.contains("did not return media"))
    );

    let gone = Server::start(|_| http_bytes(403, &[], b"expired"));
    let snap = run(
        config(dir.path()),
        vec![handle(&format!("{}/a", origin(&gone)), "image/png")],
    );
    assert!(notices(&snap).iter().any(|n| n.contains("403")));
    assert!(files(dir.path()).is_empty());
}

#[test]
fn the_size_limit_holds_with_and_without_a_content_length() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.max_bytes = 20;
    let big: Vec<u8> = PNG
        .iter()
        .copied()
        .chain(std::iter::repeat_n(0, 64))
        .collect();

    let declared = Server::start({
        let big = big.clone();
        move |_| http_bytes(200, &[], &big)
    });
    let snap = run(
        cfg.clone(),
        vec![handle(&format!("{}/a", origin(&declared)), "image/png")],
    );
    assert!(notices(&snap).iter().any(|n| n.contains("larger than")));

    // No Content-Length: the connection just ends, so only the running count can stop it.
    let unlabelled = Server::start(move |_| {
        let mut raw = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        raw.extend_from_slice(&big);
        Reply::Raw(raw)
    });
    let snap = run(
        cfg,
        vec![handle(&format!("{}/a", origin(&unlabelled)), "image/png")],
    );
    assert!(notices(&snap).iter().any(|n| n.contains("larger than")));
    assert!(files(dir.path()).is_empty(), "{:?}", files(dir.path()));
}

#[test]
fn redirects_are_followed_a_few_times_with_no_credentials() {
    let server = Server::start(|r| match r.path.as_str() {
        "/a" => http_bytes(302, &[("Location", "/b")], b""),
        "/b" => http_bytes(302, &[("Location", "/c.png")], b""),
        _ => http_bytes(200, &[], PNG),
    });
    let dir = tempfile::tempdir().unwrap();
    let snap = run(
        config(dir.path()),
        vec![handle(&format!("{}/a", origin(&server)), "image/png")],
    );
    assert_eq!(std::fs::read(saved_path(&snap)).unwrap(), PNG);
    let requests = server.recorded();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|r| r.header("authorization").is_none()));
}

#[test]
fn redirect_loops_and_unsafe_targets_stop() {
    let dir = tempfile::tempdir().unwrap();
    let looping = Server::start(|_| http_bytes(302, &[("Location", "/a")], b""));
    let snap = run(
        config(dir.path()),
        vec![handle(&format!("{}/a", origin(&looping)), "image/png")],
    );
    assert!(
        notices(&snap)
            .iter()
            .any(|n| n.contains("redirects too often"))
    );
    assert_eq!(looping.count(), 4);

    for target in [
        "http://10.0.0.5/meta",
        "https://169.254.169.254/latest",
        "ftp://example.com/a",
        "https://user:pw@example.com/a",
    ] {
        let server = Server::start(move |_| http_bytes(302, &[("Location", target)], b""));
        let snap = run(
            config(dir.path()),
            vec![handle(&format!("{}/a", origin(&server)), "image/png")],
        );
        assert!(
            notices(&snap).iter().any(|n| n.contains("refused")),
            "{target}: {:?}",
            notices(&snap)
        );
        assert_eq!(server.count(), 1, "{target}");
    }
    assert!(files(dir.path()).is_empty());
}

#[test]
fn loopback_links_are_refused_unless_allowed_and_no_request_is_made() {
    let server = Server::start(|_| http_bytes(200, &[], PNG));
    let dir = tempfile::tempdir().unwrap();
    let snap = run(
        MediaStoreConfig::new(dir.path()),
        vec![handle(&format!("{}/a", origin(&server)), "image/png")],
    );
    assert_eq!(server.count(), 0);
    assert!(notices(&snap).iter().any(|n| n.contains("refused")));
    let snap = run(
        MediaStoreConfig::new(dir.path()),
        vec![handle("https://127.0.0.1:1/a", "image/png")],
    );
    assert!(notices(&snap).iter().any(|n| n.contains("this computer")));
}

#[test]
fn a_failed_copy_never_prints_the_link() {
    let server = Server::start(|_| http_bytes(404, &[], b""));
    let dir = tempfile::tempdir().unwrap();
    let snap = run(
        config(dir.path()),
        vec![handle(
            &format!("{}/a.png?X-Signature=needle-token", origin(&server)),
            "image/png",
        )],
    );
    for n in notices(&snap) {
        assert!(!n.contains("needle-token"), "{n}");
        assert!(!n.contains("127.0.0.1"), "{n}");
    }
}

#[test]
fn local_and_inline_outputs_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let local = handle("file:///already/here.png", "image/png");
    let inline = Payload::inline("image/png", PNG.to_vec()).unwrap();
    let snap = run(
        config(dir.path()),
        vec![local.clone(), inline.clone(), Payload::text("words")],
    );
    let payloads: Vec<_> = snap.outputs().map(|o| o.payload.clone()).collect();
    assert_eq!(payloads, [local, inline, Payload::text("words")]);
    assert_eq!(notices(&snap), ["from the provider"]);
    assert!(files(dir.path()).is_empty());
}

#[test]
fn several_outputs_each_get_their_own_file() {
    let server = Server::start(|r| {
        if r.path == "/v" {
            http_bytes(200, &[], MP4)
        } else {
            http_bytes(200, &[], PNG)
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let o = origin(&server);
    let snap = run(
        config(dir.path()),
        vec![
            handle(&format!("{o}/i"), "image/png"),
            handle(&format!("{o}/v"), "video/mp4"),
            handle(&format!("{o}/i"), "image/png"),
        ],
    );
    assert_eq!(snap.outputs().count(), 3);
    assert_eq!(files(dir.path()).len(), 3);
}

#[test]
fn the_directory_must_exist_and_be_absolute() {
    assert!(MediaStore::new(MediaStoreConfig::new("relative/dir")).is_err());
    let dir = tempfile::tempdir().unwrap();
    assert!(MediaStore::new(MediaStoreConfig::new(dir.path().join("missing"))).is_err());
    let mut cfg = MediaStoreConfig::new(dir.path());
    cfg.max_bytes = 0;
    assert_eq!(
        MediaStore::new(cfg).unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
}

#[test]
fn an_unreachable_host_is_a_notice_not_a_failed_job() {
    let dead = Server::dead_url();
    let dir = tempfile::tempdir().unwrap();
    let snap = run(
        config(dir.path()),
        vec![handle(
            &format!("{}/a", dead.trim_end_matches("/v1")),
            "image/png",
        )],
    );
    assert_eq!(snap.state, JobState::Succeeded);
    assert!(
        notices(&snap)
            .iter()
            .any(|n| n.contains("could not download"))
    );
}

#[test]
fn a_huge_declared_length_is_refused_before_any_body_is_read() {
    // The header promises 10 MiB but only a PNG header follows. Refusing on the header gives
    // the size message; reading on would give a transfer error or a saved fragment instead.
    let server = Server::start(|_| {
        let mut raw =
            b"HTTP/1.1 200 OK\r\nContent-Length: 10485760\r\nConnection: close\r\n\r\n".to_vec();
        raw.extend_from_slice(PNG);
        Reply::Raw(raw)
    });
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.max_bytes = 1024 * 1024;
    let snap = run(
        cfg,
        vec![handle(&format!("{}/a", origin(&server)), "image/png")],
    );
    assert!(notices(&snap).iter().any(|n| n.contains("larger than")));
    assert!(files(dir.path()).is_empty());
}

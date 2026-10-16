//! The queue flow against a scripted server: submit, check until ready, return the link.

mod fixtures;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use craft_ai_core::conformance::{
    run_cancel_paced, run_lifecycle_paced, run_rejects, run_unknown_job,
};
use craft_ai_core::{ErrorKind, InputRole, JobState, Payload, Provider, SecretString};
use craft_ai_declarative::{DeclarativeConfig, DeclarativeProvider};
use fixtures::support::{Reply, Server};
use fixtures::{KEY, image_request, model, queue_json, spec};
use serde_json::{Value, json};

const LINK: &str = "https://cdn.example.com/out.png";

/// A server that finishes a job after `waits` status checks.
fn queue_server(waits: usize) -> (Server, Arc<AtomicUsize>) {
    let checks = Arc::new(AtomicUsize::new(0));
    let seen = checks.clone();
    let server = Server::start(move |r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/v1/account") => Reply::Json(200, "{}".into()),
        ("POST", "/v1/images") => Reply::Json(
            200,
            r#"{"id":"run-1","polling_url":"https://attacker.example/x"}"#.into(),
        ),
        ("GET", "/v1/result?id=run-1") => {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            let body = if n < waits {
                json!({"status": if n == 0 { "Pending" } else { "Working" }})
            } else {
                json!({"status": "Ready", "result": {"sample": LINK}})
            };
            Reply::Json(200, body.to_string())
        }
        ("DELETE", "/v1/jobs/run-1") => Reply::Json(200, "{}".into()),
        _ => Reply::Json(404, "{}".into()),
    });
    (server, checks)
}

fn provider_with(server: &Server, edit: impl FnOnce(&mut Value)) -> DeclarativeProvider {
    let mut v = queue_json(&server.url);
    edit(&mut v);
    DeclarativeProvider::new(DeclarativeConfig::new(
        spec(&v),
        Some(SecretString::new(KEY)),
    ))
    .unwrap()
}

fn pause() {
    std::thread::sleep(Duration::from_millis(2));
}

#[test]
fn the_probe_the_submit_and_the_checks_follow_the_spec_and_pass_conformance() {
    let (server, _) = queue_server(2);
    let p = provider_with(&server, |_| {});
    assert_eq!(
        p.models().unwrap().len(),
        1,
        "probe passes, then the spec's models"
    );
    let done = run_lifecycle_paced(&p, &model(), &image_request("a red door"), &pause).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    let out = done.events.iter().find_map(|e| match e {
        craft_ai_core::Event::Output(o) => Some(o),
        _ => None,
    });
    let Some(Payload::Handle(h)) = out.map(|o| &o.payload) else {
        panic!("a link was expected: {:?}", done.events);
    };
    assert_eq!((h.uri.as_str(), h.media_type.as_str()), (LINK, "image/png"));

    let seen = server.recorded();
    assert!(
        seen.iter().all(|r| r.header("x-api-key") == Some(KEY)),
        "the key rides in the named header"
    );
    assert!(seen.iter().all(|r| r.header("authorization").is_none()));
    let submit = seen.iter().find(|r| r.method == "POST").unwrap();
    assert_eq!(
        submit.body_json(),
        json!({"prompt": "a red door", "model": "img-1"})
    );
    assert!(
        seen.iter().all(|r| r.path.starts_with("/v1/")),
        "every call stays on the base: the server's polling_url was ignored"
    );
}

#[test]
fn a_failing_probe_fails_the_connection_test_and_a_passing_one_was_really_sent() {
    let (server, _) = queue_server(0);
    provider_with(&server, |_| {}).models().unwrap();
    assert!(
        server.recorded().iter().any(|r| r.path == "/v1/account"),
        "the probe was sent"
    );
    let denied = Server::start(|_| Reply::Json(401, "{}".into()));
    let e = provider_with(&denied, |_| {}).models().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Auth);
}

#[test]
fn a_redirect_on_a_read_is_not_followed_either() {
    let elsewhere = Server::start(|_| Reply::Json(200, "{}".into()));
    let target = elsewhere.url.clone();
    let server = Server::start(move |_| {
        Reply::JsonWith(302, vec![("Location", target.clone())], "{}".into())
    });
    let e = provider_with(&server, |_| {}).models().unwrap_err();
    assert!(!e.message.contains(KEY));
    assert_eq!(server.count(), 1);
    assert_eq!(
        elsewhere.count(),
        0,
        "nothing was fetched from the Location"
    );
}

#[test]
fn a_cancel_call_the_service_refuses_leaves_the_job_running() {
    let server = Server::start(|r| match r.method.as_str() {
        "POST" => Reply::Json(200, r#"{"id":"run-1"}"#.into()),
        "DELETE" => Reply::Json(500, "{}".into()),
        _ => Reply::Json(200, r#"{"status":"Working"}"#.into()),
    });
    let p = provider_with(&server, |_| {});
    let id = p.submit(&model(), &image_request("x")).unwrap();
    assert!(p.cancel(&id).is_err());
    assert!(matches!(
        p.poll(&id).unwrap().state,
        JobState::Running { .. }
    ));
}

#[test]
fn a_cancel_route_stops_the_job_and_conformance_agrees() {
    let (server, _) = queue_server(1000);
    let p = provider_with(&server, |_| {});
    run_cancel_paced(&p, &model(), &image_request("x"), &pause).unwrap();
    assert!(
        server
            .recorded()
            .iter()
            .any(|r| r.method == "DELETE" && r.path == "/v1/jobs/run-1")
    );
}

#[test]
fn without_a_cancel_route_a_running_job_says_so_and_sends_nothing() {
    let (server, _) = queue_server(1000);
    let p = provider_with(&server, |v| v["flow"]["cancel"] = Value::Null);
    let id = p.submit(&model(), &image_request("x")).unwrap();
    let before = server.count();
    let e = p.cancel(&id).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unsupported);
    assert_eq!(server.count(), before);
}

#[test]
fn refused_requests_and_unknown_jobs_follow_the_shared_rules() {
    let (server, _) = queue_server(0);
    let p = provider_with(&server, |_| {});
    run_unknown_job(&p).unwrap();
    let before = server.count();
    let mask = image_request("x").with_input(
        InputRole::Mask,
        Payload::inline("image/png", vec![1]).unwrap(),
    );
    run_rejects(&p, &model(), &mask, ErrorKind::Unsupported).unwrap();
    let with_option = image_request("x").with_param("seed", json!(1));
    run_rejects(&p, &model(), &with_option, ErrorKind::Unsupported).unwrap();
    assert_eq!(
        p.submit(&model(), &image_request("")).unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
    assert_eq!(
        server.count(),
        before,
        "a refused request causes no traffic"
    );
}

#[test]
fn a_job_that_never_finishes_is_ended_by_its_poll_budget() {
    let (server, _) = queue_server(1000);
    let p = provider_with(&server, |v| v["limits"] = json!({"max_polls": 3}));
    let id = p.submit(&model(), &image_request("x")).unwrap();
    for _ in 0..3 {
        assert!(!p.poll(&id).unwrap().state.is_terminal());
    }
    let ended = p.poll(&id).unwrap();
    let JobState::Failed { error } = &ended.state else {
        panic!("expected a failure, got {:?}", ended.state);
    };
    assert!(
        error.message.contains("gave up waiting"),
        "{}",
        error.message
    );
    let checks = server.count();
    assert!(p.poll(&id).unwrap().state.is_terminal());
    assert_eq!(server.count(), checks, "a finished job is not polled again");
}

#[test]
fn a_status_word_outside_the_table_is_an_error_never_a_guess() {
    let server = Server::start(|r| match r.method.as_str() {
        "POST" => Reply::Json(200, r#"{"id":"run-1"}"#.into()),
        _ => Reply::Json(200, r#"{"status":"Mystery"}"#.into()),
    });
    let p = provider_with(&server, |_| {});
    let id = p.submit(&model(), &image_request("x")).unwrap();
    let e = p.poll(&id).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Provider);
    assert!(e.message.contains("status"), "{}", e.message);
    assert!(
        !e.message.contains("Mystery"),
        "reply text stays out of errors"
    );
}

#[test]
fn the_failed_word_fails_the_job_with_the_word_the_spec_gave() {
    let server = Server::start(|r| match r.method.as_str() {
        "POST" => Reply::Json(200, r#"{"id":"run-1"}"#.into()),
        _ => Reply::Json(
            200,
            r#"{"status":"Error","detail":"needle-secret-key"}"#.into(),
        ),
    });
    let p = provider_with(&server, |_| {});
    let id = p.submit(&model(), &image_request("x")).unwrap();
    let JobState::Failed { error } = p.poll(&id).unwrap().state else {
        panic!("expected a failure");
    };
    assert!(error.message.contains("Error"), "{}", error.message);
    assert!(!error.message.contains(KEY));
}

#[test]
fn a_job_id_the_service_returns_must_be_short_and_plain() {
    for bad in [
        json!("a\nb"),
        json!("x".repeat(129)),
        json!(""),
        json!({"id": 1}),
        json!("a b"),
        json!(".."),
        json!("."),
    ] {
        let reply = json!({ "id": bad }).to_string();
        let server = Server::start(move |_| Reply::Json(200, reply.clone()));
        let p = provider_with(&server, |_| {});
        let e = p.submit(&model(), &image_request("x")).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Provider, "{bad}");
        assert!(e.message.contains("job id"), "{}", e.message);
    }
    let server = Server::start(|_| Reply::Json(200, r#"{"id":12345}"#.into()));
    assert!(
        provider_with(&server, |_| {})
            .submit(&model(), &image_request("x"))
            .is_ok(),
        "a number is a fine id"
    );
}

#[test]
fn a_result_link_must_be_https_and_public() {
    for bad in [
        "http://cdn.example.com/a.png",
        "https://127.0.0.1/a.png",
        "https://u:p@cdn.example.com/a.png",
        "https://cdn.example.com/a.png#frag",
        "not a url",
    ] {
        let body = json!({"status": "Ready", "result": {"sample": bad}}).to_string();
        let server = Server::start(move |r| match r.method.as_str() {
            "POST" => Reply::Json(200, r#"{"id":"run-1"}"#.into()),
            _ => Reply::Json(200, body.clone()),
        });
        let p = provider_with(&server, |_| {});
        let id = p.submit(&model(), &image_request("x")).unwrap();
        let JobState::Failed { error } = p.poll(&id).unwrap().state else {
            panic!("{bad}: an unusable link must fail the job");
        };
        assert!(error.message.contains("link"), "{bad}: {}", error.message);
    }
}

#![allow(dead_code)]
#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use craft_ai_bfl::{BflConfig, BflProvider};
use craft_ai_core::{
    ErrorKind, InputRole, JobState, ModelId, Payload, Provider, Request, SecretString,
    capability::CapabilityId,
};
use serde_json::json;
use support::{Reply, Server};

fn image_request() -> Request {
    Request::new(CapabilityId::parse("image.generate").unwrap())
        .with_input(InputRole::Prompt, Payload::text("a mountain"))
}

#[test]
fn key_probe_then_image_submit_poll_and_output() {
    let server = Server::start(|r| {
        match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/v1/credits") => Reply::Json(200, r#"{"credits":1.0}"#.into()),
        ("POST", "/v1/flux-3-image") => {
            assert_eq!(r.header("x-key"), Some("test-key"));
            assert_eq!(r.body_json(), json!({"prompt":"a mountain"}));
            Reply::Json(200, r#"{"id":"run-1","polling_url":"ignored"}"#.into())
        }
        ("GET", "/v1/get_result?id=run-1") => Reply::Json(200, r#"{"id":"run-1","status":"Ready","result":{"sample":"https://cdn.bfl.ai/result.png"}}"#.into()),
        _ => Reply::Json(404, "{}".into()),
    }
    });
    let provider =
        BflProvider::new(BflConfig::new(&server.url, SecretString::new("test-key"))).unwrap();
    assert!(
        provider
            .models()
            .unwrap()
            .iter()
            .any(|m| m.id.as_str() == "flux-3-image")
    );
    let id = provider
        .submit(&ModelId::new("flux-3-image").unwrap(), &image_request())
        .unwrap();
    let done = provider.poll(&id).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    let out = done.outputs().next().unwrap();
    assert!(matches!(&out.payload, Payload::Handle(h) if h.uri == "https://cdn.bfl.ai/result.png"));
    assert!(provider.poll(&id).unwrap().events.is_empty());
    assert!(
        server
            .recorded()
            .iter()
            .all(|r| r.header("x-key") == Some("test-key"))
    );
}

#[test]
fn video_uses_distinct_route_and_rejects_unsupported_inputs_before_network() {
    let server = Server::start(|r| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/v1/flux-3-video") => {
            assert_eq!(r.body_json(), json!({"prompt":"a mountain","mode":"t2v"}));
            Reply::Json(200, r#"{"id":"vid-1","polling_url":"ignored"}"#.into())
        }
        ("GET", "/v1/get_result?id=vid-1") => Reply::Json(
            200,
            r#"{"id":"vid-1","status":"Ready","result":{"video":"https://cdn.bfl.ai/out.mp4"}}"#
                .into(),
        ),
        _ => Reply::Json(404, "{}".into()),
    });
    let p = BflProvider::new(BflConfig::new(&server.url, SecretString::new("test-key"))).unwrap();
    let model = ModelId::new("flux-3-video").unwrap();
    let invalid = Request::new(CapabilityId::parse("video.generate").unwrap())
        .with_input(InputRole::Prompt, Payload::text("hello"))
        .with_input(
            InputRole::Mask,
            Payload::inline("image/png", vec![1]).unwrap(),
        );
    assert_eq!(
        p.submit(&model, &invalid).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(server.count(), 0);
    let request = Request::new(CapabilityId::parse("video.generate").unwrap())
        .with_input(InputRole::Prompt, Payload::text("a mountain"));
    let id = p.submit(&model, &request).unwrap();
    let done = p.poll(&id).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert!(
        matches!(&done.outputs().next().unwrap().payload, Payload::Handle(h) if h.media_type.as_str() == "video/mp4")
    );
}

#[test]
fn never_follows_redirects_or_leaks_key_in_errors() {
    let server = Server::start(|_| {
        Reply::JsonWith(
            302,
            vec![("Location", "https://attacker.example".into())],
            "{}".into(),
        )
    });
    let p = BflProvider::new(BflConfig::new(
        &server.url,
        SecretString::new("needle-secret"),
    ))
    .unwrap();
    let e = p.models().unwrap_err();
    assert!(!e.message.contains("needle-secret"));
    assert!(!format!("{p:?}").contains("needle-secret"));
    assert_eq!(server.count(), 1);
}

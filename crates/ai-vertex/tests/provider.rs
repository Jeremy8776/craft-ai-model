#![allow(dead_code)]
#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;
use craft_ai_core::{
    ErrorKind, InputRole, JobState, ModelId, Payload, Provider, Request, SecretString,
    capability::CapabilityId,
};
use craft_ai_vertex::{VertexConfig, VertexProvider};
use serde_json::json;
use support::{Reply, Server};

#[test]
fn image_generation_uses_express_key_and_returns_inline_image() {
    let s = Server::start(|r| {
        assert_eq!(r.method, "POST");
        assert_eq!(
            r.path,
            "/v1/publishers/google/models/gemini-2.5-flash-image:generateContent?key=test-key"
        );
        assert_eq!(
            r.body_json(),
            json!({"contents":[{"role":"user","parts":[{"text":"a fox"}]}],"generationConfig":{"responseModalities":["TEXT","IMAGE"]}})
        );
        Reply::Json(200, r#"{"candidates":[{"content":{"parts":[{"text":"done"},{"inlineData":{"mimeType":"image/png","data":"AQID"}}]}}]}"#.into())
    });
    let p = VertexProvider::new(VertexConfig::new(
        s.url.clone(),
        SecretString::new("test-key"),
    ))
    .unwrap();
    let request = Request::new(CapabilityId::parse("image.generate").unwrap())
        .with_input(InputRole::Prompt, Payload::text("a fox"));
    let job = p
        .submit(&ModelId::new("gemini-2.5-flash-image").unwrap(), &request)
        .unwrap();
    let done = p.poll(&job).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert!(
        matches!(&done.outputs().next().unwrap().payload, Payload::Inline { data, .. } if data == &[1,2,3])
    );
    assert!(p.poll(&job).unwrap().events.is_empty());
}

#[test]
fn refuses_unsupported_before_paid_call_and_never_follows_redirects() {
    let s = Server::start(|_| {
        Reply::JsonWith(
            307,
            vec![("Location", "https://attacker.example".into())],
            "{}".into(),
        )
    });
    let p = VertexProvider::new(VertexConfig::new(
        s.url.clone(),
        SecretString::new("needle-secret"),
    ))
    .unwrap();
    let request = Request::new(CapabilityId::parse("image.generate").unwrap())
        .with_input(InputRole::Prompt, Payload::text("a fox"))
        .with_input(
            InputRole::Mask,
            Payload::inline("image/png", vec![1]).unwrap(),
        );
    assert_eq!(
        p.submit(&ModelId::new("gemini-2.5-flash-image").unwrap(), &request)
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(s.count(), 0);
    assert!(!format!("{p:?}").contains("needle-secret"));
    let e = p.models().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unsupported);
    assert_eq!(s.count(), 0);
}

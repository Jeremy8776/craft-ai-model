#![allow(dead_code)]
#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;
use craft_ai_core::{
    ErrorKind, InputRole, JobState, ModelId, Payload, Provider, Request, SecretString,
    capability::CapabilityId,
};
use craft_ai_seedance::{SeedanceConfig, SeedanceProvider};
use serde_json::json;
use support::{Reply, Server};

#[test]
fn probes_key_and_runs_text_to_video() {
    let s = Server::start(|r| {
        match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/v3/contents/generations/tasks?page_num=1&page_size=1") => Reply::Json(200, r#"{"items":[]}"#.into()),
        ("POST", "/v3/contents/generations/tasks") => {
            assert_eq!(r.header("authorization"), Some("Bearer test-key"));
            assert_eq!(r.body_json(), json!({"model":"dreamina-seedance-2-0-260128","content":[{"type":"text","text":"a dance"}]}));
            Reply::Json(200, r#"{"id":"cgt-123"}"#.into())
        }
        ("GET", "/v3/contents/generations/tasks/cgt-123") => Reply::Json(200, r#"{"id":"cgt-123","status":"succeeded","content":{"video_url":"https://example.com/video.mp4"}}"#.into()),
        _ => Reply::Json(404, "{}".into()),
    }
    });
    let p = SeedanceProvider::new(SeedanceConfig::new(
        s.url.replace("/v1", "/v3"),
        SecretString::new("test-key"),
    ))
    .unwrap();
    assert_eq!(p.models().unwrap().len(), 1);
    let request = Request::new(CapabilityId::parse("video.generate").unwrap())
        .with_input(InputRole::Prompt, Payload::text("a dance"));
    let job = p
        .submit(
            &ModelId::new("dreamina-seedance-2-0-260128").unwrap(),
            &request,
        )
        .unwrap();
    let done = p.poll(&job).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert!(
        matches!(&done.outputs().next().unwrap().payload, Payload::Handle(h) if h.uri == "https://example.com/video.mp4")
    );
    assert!(p.poll(&job).unwrap().events.is_empty());
}

#[test]
fn rejects_mask_and_redirect_without_sending_credentials_elsewhere() {
    let s = Server::start(|_| {
        Reply::JsonWith(
            307,
            vec![("Location", "https://attacker.example".into())],
            "{}".into(),
        )
    });
    let p = SeedanceProvider::new(SeedanceConfig::new(
        s.url.replace("/v1", "/v3"),
        SecretString::new("needle-secret"),
    ))
    .unwrap();
    let request = Request::new(CapabilityId::parse("video.generate").unwrap())
        .with_input(InputRole::Prompt, Payload::text("a dance"))
        .with_input(
            InputRole::Mask,
            Payload::inline("image/png", vec![1]).unwrap(),
        );
    assert_eq!(
        p.submit(
            &ModelId::new("dreamina-seedance-2-0-260128").unwrap(),
            &request
        )
        .unwrap_err()
        .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(s.count(), 0);
    let err = p.models().unwrap_err();
    assert!(!err.message.contains("needle-secret"));
    assert!(!format!("{p:?}").contains("needle-secret"));
    assert_eq!(s.count(), 1);
}

#[test]
fn a_hosted_connection_must_use_the_official_byteplus_modelark_address() {
    let key = || SecretString::new("k");
    let build = |address: &str| SeedanceProvider::new(SeedanceConfig::new(address, key()));
    assert!(build("https://ark.ap-southeast.bytepluses.com/api/v3").is_ok());
    assert!(build("https://ark.ap-southeast.bytepluses.com/api/v3/").is_ok());
    assert!(build("http://127.0.0.1:9/v3").is_ok());
    for refused in [
        // The Volcengine China service is no longer the Seedance connection.
        "https://ark.cn-beijing.volces.com/api/v3",
        "https://ark.ap-southeast.bytepluses.com.evil.example/api/v3",
        "http://ark.ap-southeast.bytepluses.com/api/v3",
        "https://ark.ap-southeast.bytepluses.com/api/v3?x=1",
        "https://ark.ap-southeast.bytepluses.com/api/v4",
        // A local test server may be used, but never with a query string or fragment.
        "http://127.0.0.1:9/v3?x=1",
        "http://127.0.0.1:9/v3#x",
    ] {
        assert!(build(refused).is_err(), "{refused} should be refused");
    }
}

#[test]
fn the_old_volcengine_model_name_is_not_served() {
    let s = Server::start(|_| Reply::Json(404, "{}".into()));
    let p = SeedanceProvider::new(SeedanceConfig::new(
        s.url.replace("/v1", "/v3"),
        SecretString::new("k"),
    ))
    .unwrap();
    let request = Request::new(CapabilityId::parse("video.generate").unwrap())
        .with_input(InputRole::Prompt, Payload::text("a dance"));
    let err = p
        .submit(
            &ModelId::new("doubao-seedance-2-0-260128").unwrap(),
            &request,
        )
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert_eq!(s.count(), 0);
}

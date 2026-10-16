#![allow(dead_code)]
#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use craft_ai_artcraft::{ArtCraftConfig, ArtCraftProvider};
use craft_ai_core::{
    ErrorKind, InputRole, JobState, ModelId, Payload, Provider, Request, SecretString,
    capability::CapabilityId,
};
use support::{Reply, Server};

fn request(capability: &str) -> Request {
    Request::new(CapabilityId::parse(capability).unwrap())
        .with_input(InputRole::Prompt, Payload::text("a quiet coastline"))
}

#[test]
fn video_uses_api_key_only_idempotency_and_polling() {
    let server = Server::start(|r| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/v1/omni_api/generate/video") => {
            assert_eq!(r.header("authorization"), Some("Bearer test-key"));
            assert!(r.header("cookie").is_none());
            let body = r.body_json();
            assert_eq!(body["model"], "seedance_2p0");
            assert_eq!(body["prompt"], "a quiet coastline");
            assert!(
                body["idempotency_token"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
            );
            Reply::Json(
                200,
                r#"{"success":true,"inference_job_token":"jinf_video1"}"#.into(),
            )
        }
        ("GET", "/v1/omni_api/job_status/job/jinf_video1") => {
            assert_eq!(r.header("authorization"), Some("Bearer test-key"));
            Reply::Json(200, r#"{"success":true,"state":{"job_token":"jinf_video1","status":{"status":"complete_success","progress_percentage":100},"maybe_result":{"media_links":{"cdn_url":"https://cdn.example.com/result.mp4"}}}}"#.into())
        }
        _ => Reply::Json(404, "{}".into()),
    });
    let p = ArtCraftProvider::new(ArtCraftConfig::new(
        server.url.trim_end_matches("/v1"),
        SecretString::new("test-key"),
    ))
    .unwrap();
    let id = p
        .submit(
            &ModelId::new("seedance_2p0").unwrap(),
            &request("video.generate"),
        )
        .unwrap();
    let done = p.poll(&id).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert!(
        matches!(&done.outputs().next().unwrap().payload, Payload::Handle(h) if h.uri == "https://cdn.example.com/result.mp4")
    );
    assert!(p.poll(&id).unwrap().events.is_empty());
}

#[test]
fn image_route_is_distinct_and_bad_inputs_make_no_traffic() {
    let server = Server::start(|r| {
        match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/v1/omni_api/generate/image") => {
            assert_eq!(r.body_json()["model"], "nano_banana");
            Reply::Json(200, r#"{"success":true,"inference_job_token":"jinf_image1"}"#.into())
        }
        ("GET", "/v1/omni_api/job_status/job/jinf_image1") => Reply::Json(200, r#"{"success":true,"state":{"job_token":"jinf_image1","status":{"status":"complete_success","progress_percentage":100},"maybe_result":{"media_links":{"cdn_url":"https://cdn.example.com/result.png"}}}}"#.into()),
        _ => Reply::Json(404, "{}".into()),
    }
    });
    let p = ArtCraftProvider::new(ArtCraftConfig::new(
        server.url.trim_end_matches("/v1"),
        SecretString::new("test-key"),
    ))
    .unwrap();
    let invalid = request("image.generate").with_input(
        InputRole::Mask,
        Payload::inline("image/png", vec![1]).unwrap(),
    );
    assert_eq!(
        p.submit(&ModelId::new("nano_banana").unwrap(), &invalid)
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(server.count(), 0);
    let id = p
        .submit(
            &ModelId::new("nano_banana").unwrap(),
            &request("image.generate"),
        )
        .unwrap();
    assert_eq!(p.poll(&id).unwrap().state, JobState::Succeeded);
}

#[test]
fn no_free_probe_does_not_misrepresent_an_api_key_as_verified() {
    let server = Server::start(|_| Reply::Json(500, "{}".into()));
    let p = ArtCraftProvider::new(ArtCraftConfig::new(
        server.url.trim_end_matches("/v1"),
        SecretString::new("needle-secret"),
    ))
    .unwrap();
    let err = p.models().unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert_eq!(server.count(), 0);
    assert!(!format!("{p:?}{err:?}").contains("needle-secret"));
}

#[test]
fn full_registry_refuses_before_any_second_paid_request() {
    let server = Server::start(|_| {
        Reply::Json(
            200,
            r#"{"success":true,"inference_job_token":"jinf_one"}"#.into(),
        )
    });
    let mut cfg = ArtCraftConfig::new(
        server.url.trim_end_matches("/v1"),
        SecretString::new("test-key"),
    );
    cfg.max_active_jobs = 1;
    let p = ArtCraftProvider::new(cfg).unwrap();
    let model = ModelId::new("nano_banana").unwrap();
    p.submit(&model, &request("image.generate")).unwrap();
    let err = p.submit(&model, &request("image.generate")).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Quota);
    assert_eq!(server.count(), 1);
}

#[test]
fn payment_refusal_and_redirect_never_leak_key() {
    for (status, expected) in [(402, ErrorKind::Quota), (302, ErrorKind::Provider)] {
        let server = Server::start(move |_| {
            Reply::JsonWith(
                status,
                vec![("Location", "https://attacker.example".into())],
                "{}".into(),
            )
        });
        let p = ArtCraftProvider::new(ArtCraftConfig::new(
            server.url.trim_end_matches("/v1"),
            SecretString::new("needle-secret"),
        ))
        .unwrap();
        let err = p
            .submit(
                &ModelId::new("nano_banana").unwrap(),
                &request("image.generate"),
            )
            .unwrap_err();
        assert_eq!(err.kind, expected);
        assert!(!err.message.contains("needle-secret"));
        assert_eq!(server.count(), 1);
    }
}

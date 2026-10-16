#![allow(dead_code)]
#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;
use craft_ai_core::{
    ErrorKind, InputRole, JobState, ModelId, Payload, Provider, Request, capability::CapabilityId,
};
use craft_ai_sam3::{Sam3Config, Sam3Provider};
use serde_json::json;
use support::{Reply, Server};

#[test]
fn detects_boxes_with_local_image_and_text() {
    let s = Server::start(|r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/v1/health") => Reply::Json(200, r#"{"model":"sam3","ready":true}"#.into()),
        ("POST", "/v1/detect") => {
            assert_eq!(
                r.body_json(),
                json!({"prompt":"a cat","image":{"mime_type":"image/png","data":"AQID"}})
            );
            Reply::Json(
                200,
                r#"{"width":100,"height":50,"boxes":[[1.0,2.0,30.0,40.0]],"scores":[0.8]}"#.into(),
            )
        }
        _ => Reply::Json(404, "{}".into()),
    });
    let p = Sam3Provider::new(Sam3Config::new(s.url)).unwrap();
    assert_eq!(
        p.models().unwrap()[0].capabilities[0].capability.as_str(),
        "image.detect"
    );
    let req = Request::new(CapabilityId::parse("image.detect").unwrap())
        .with_input(InputRole::Prompt, Payload::text("a cat"))
        .with_input(
            InputRole::Source,
            Payload::inline("image/png", vec![1, 2, 3]).unwrap(),
        );
    let job = p.submit(&ModelId::new("sam3").unwrap(), &req).unwrap();
    let out = p.poll(&job).unwrap();
    assert_eq!(out.state, JobState::Succeeded);
    assert!(
        matches!(&out.outputs().next().unwrap().payload, Payload::Inline { media_type, .. } if media_type.as_str() == "application/json")
    );
    assert!(p.poll(&job).unwrap().events.is_empty());
}

#[test]
fn rejects_remote_urls_and_mask_before_traffic() {
    assert!(Sam3Provider::new(Sam3Config::new("https://example.com/v1")).is_err());
    let s = Server::start(|_| Reply::Json(200, "{}".into()));
    let p = Sam3Provider::new(Sam3Config::new(s.url.clone())).unwrap();
    let req = Request::new(CapabilityId::parse("image.detect").unwrap()).with_input(
        InputRole::Mask,
        Payload::inline("image/png", vec![1]).unwrap(),
    );
    assert_eq!(
        p.submit(&ModelId::new("sam3").unwrap(), &req)
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(s.count(), 0);
}

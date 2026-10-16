//! Shared setup for the declarative provider tests: the scripted server and two specs.
#![allow(dead_code)]
#[path = "../../../ai-openai/tests/support/mod.rs"]
pub mod support;

use craft_ai_core::spec::Spec;
use craft_ai_core::{InputRole, ModelId, Payload, Request, capability::CapabilityId};
use serde_json::{Value, json};

pub const KEY: &str = "needle-secret-key";

pub fn model() -> ModelId {
    ModelId::new("img-1").unwrap()
}

pub fn image_request(prompt: &str) -> Request {
    Request::new(CapabilityId::parse("image.generate").unwrap())
        .with_input(InputRole::Prompt, Payload::text(prompt))
}

pub fn queue_json(base: &str) -> Value {
    json!({
        "spec_version": 1, "id": "example-images", "label": "Example Images", "base": base,
        "auth": {"kind": "header", "name": "x-api-key"},
        "probe": {"method": "GET", "path": "/account"},
        "models": [{"id": "img-1", "label": "Image 1", "capability": "image.generate",
                    "accepts": {"prompt": 1}, "requires": ["prompt"]}],
        "flow": {"kind": "queue",
            "submit": {"call": {"method": "POST", "path": "/images",
                                "body": {"prompt": "{prompt}", "model": "{model}"}},
                       "job_id_at": "/id"},
            "poll": {"call": {"method": "GET", "path": "/result", "query": {"id": "{job_id}"}},
                     "status_at": "/status",
                     "states": {"Pending": "queued", "Working": "running",
                                "Ready": "succeeded", "Error": "failed"},
                     "result": {"at": "/result/sample", "as": "url", "media_type": "image/png"}},
            "cancel": {"method": "DELETE", "path": "/jobs/{job_id}"}}
    })
}

pub fn direct_json(base: &str) -> Value {
    json!({
        "spec_version": 1, "id": "plain-images", "label": "Plain Images", "base": base,
        "models": [{"id": "img-1", "label": "Image 1", "capability": "image.generate",
                    "accepts": {"prompt": 1}, "requires": ["prompt"]}],
        "flow": {"kind": "request_response",
            "call": {"method": "POST", "path": "/images",
                     "body": {"model": "{model}", "prompt": "{prompt}"}},
            "result": {"at": "/data/0/b64_json", "as": "base64", "media_type": "image/png"}}
    })
}

pub fn spec(v: &Value) -> Spec {
    Spec::parse(&v.to_string()).unwrap()
}

/// The first bytes of a PNG, which is all the media check looks at.
pub fn png_bytes() -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    b.extend_from_slice(&[0, 0, 0, 13]);
    b
}

pub fn b64(bytes: &[u8]) -> String {
    craft_ai_core::payload::base64_encode(bytes)
}

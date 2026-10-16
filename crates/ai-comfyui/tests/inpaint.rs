#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;
use craft_ai_comfyui::{Binding, ComfyConfig, ComfyProvider, Workflow};
use craft_ai_core::capability::CapabilityId;
use craft_ai_core::model::InputRole;
use craft_ai_core::payload::Payload;
use craft_ai_core::{ModelId, Provider, Request};
use serde_json::json;
use support::{Reply, Server};

fn request(cap: &str) -> Request {
    Request::new(CapabilityId::parse(cap).unwrap())
        .with_input(InputRole::Prompt, Payload::text("a lighthouse"))
}

fn inpaint_workflow() -> Workflow {
    Workflow::import_inpaint(
        "repair",
        json!({
            "1":{"class_type":"CLIPTextEncode","inputs":{"text":"old"}},
            "2":{"class_type":"LoadImage","inputs":{"image":"old-source.png"}},
            "3":{"class_type":"LoadImageMask","inputs":{"image":"old-mask.png","channel":"red"}},
            "4":{"class_type":"SaveOutput","inputs":{"data":["5",0]}},
            "5":{"class_type":"Inpaint","inputs":{"image":["2",0],"mask":["3",0],"prompt":["1",0]}}
        }),
        Binding::new("1", "CLIPTextEncode", "text"),
        "4",
        Binding::new("2", "LoadImage", "image"),
        Binding::new("3", "LoadImageMask", "image"),
    )
    .unwrap()
}

fn inpaint_request() -> Request {
    let image = b"\x89PNG\r\n\x1a\nphoto".to_vec();
    request("image.inpaint")
        .with_input(
            InputRole::Source,
            Payload::inline("image/png", image.clone()).unwrap(),
        )
        .with_input(
            InputRole::Mask,
            Payload::inline("image/png", image).unwrap(),
        )
}

fn inpaint_installed() -> String {
    json!({
        "CLIPTextEncode":{"input":{"required":{"text":["STRING",{}]}}},
        "LoadImage":{"input":{"required":{"image":[[],{"image_upload":true}]}}},
        "LoadImageMask":{"input":{"required":{"image":[[],{"image_upload":true}],"channel":[["red","alpha"],{}]}}},
        "Inpaint":{"input":{"required":{"image":["IMAGE",{}],"mask":["MASK",{}],"prompt":["CONDITIONING",{}]}}},
        "SaveOutput":{"input":{"required":{"data":["IMAGE",{}]}}}
    }).to_string()
}

#[test]
fn inpaint_uploads_source_and_mask_then_queues_with_returned_names() {
    let s = Server::start(|r| match r.path.as_str() {
        "/system_stats" => Reply::Json(200, "{}".into()),
        "/object_info" => Reply::Json(200, inpaint_installed()),
        "/upload/image" => {
            assert_eq!(r.method, "POST");
            assert!(
                r.header("content-type")
                    .unwrap()
                    .starts_with("multipart/form-data; boundary=")
            );
            assert!(
                r.body
                    .windows(b"name=\"type\"".len())
                    .any(|w| w == b"name=\"type\"")
            );
            if r.body_text().contains("craft-source-") {
                Reply::Json(
                    200,
                    json!({"name":"returned-source.png","subfolder":"","type":"input"}).to_string(),
                )
            } else {
                Reply::Json(
                    200,
                    json!({"name":"returned-mask.png","subfolder":"","type":"input"}).to_string(),
                )
            }
        }
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"inpaint-job"}"#.into()),
        _ => Reply::Json(404, "{}".into()),
    });
    let p = ComfyProvider::new(
        ComfyConfig::new(s.url.trim_end_matches("/v1")).with_workflow(inpaint_workflow()),
    )
    .unwrap();
    assert_eq!(
        p.models().unwrap()[0].capabilities[0].capability.as_str(),
        "image.inpaint"
    );
    p.submit(&ModelId::new("repair").unwrap(), &inpaint_request())
        .unwrap();
    let recorded = s.recorded();
    assert_eq!(
        recorded
            .iter()
            .filter(|r| r.path == "/upload/image")
            .count(),
        2
    );
    let graph = &recorded
        .iter()
        .find(|r| r.path == "/prompt")
        .unwrap()
        .body_json()["prompt"];
    assert_eq!(graph["2"]["inputs"]["image"], "returned-source.png");
    assert_eq!(graph["3"]["inputs"]["image"], "returned-mask.png");
}

#[test]
fn upload_collision_filename_is_used_without_losing_the_job() {
    let s = Server::start(|r| match r.path.as_str() {
        "/object_info" => Reply::Json(200, inpaint_installed()),
        "/upload/image" => {
            let name = if r.body_text().contains("craft-source-") {
                "craft-source (1).png"
            } else {
                "craft-mask (2).png"
            };
            Reply::Json(
                200,
                json!({"name":name,"subfolder":"","type":"input"}).to_string(),
            )
        }
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"collision-job"}"#.into()),
        _ => Reply::Json(404, "{}".into()),
    });
    let p = ComfyProvider::new(
        ComfyConfig::new(s.url.trim_end_matches("/v1")).with_workflow(inpaint_workflow()),
    )
    .unwrap();
    p.submit(&ModelId::new("repair").unwrap(), &inpaint_request())
        .unwrap();
    let graph = &s
        .recorded()
        .iter()
        .find(|r| r.path == "/prompt")
        .unwrap()
        .body_json()["prompt"];
    assert_eq!(graph["2"]["inputs"]["image"], "craft-source (1).png");
}

#[test]
fn inpaint_bad_mask_or_upload_reply_never_queues_a_job() {
    let s = Server::start(|r| match r.path.as_str() {
        "/object_info" => Reply::Json(200, inpaint_installed()),
        "/upload/image" => Reply::Json(
            200,
            json!({"name":"../escape.png","type":"input","subfolder":""}).to_string(),
        ),
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"should-not-run"}"#.into()),
        _ => Reply::Json(404, "{}".into()),
    });
    let p = ComfyProvider::new(
        ComfyConfig::new(s.url.trim_end_matches("/v1")).with_workflow(inpaint_workflow()),
    )
    .unwrap();
    let invalid = request("image.inpaint").with_input(
        InputRole::Source,
        Payload::inline("image/png", b"\x89PNG\r\n\x1a\n".to_vec()).unwrap(),
    );
    assert!(
        p.submit(&ModelId::new("repair").unwrap(), &invalid)
            .is_err()
    );
    assert_eq!(s.count(), 0);
    assert!(
        p.submit(&ModelId::new("repair").unwrap(), &inpaint_request())
            .is_err()
    );
    assert!(!s.recorded().iter().any(|r| r.path == "/prompt"));
}

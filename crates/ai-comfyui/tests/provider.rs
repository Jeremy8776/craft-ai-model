#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;
use craft_ai_comfyui::{Binding, ComfyConfig, ComfyProvider, Workflow};
use craft_ai_core::capability::CapabilityId;
use craft_ai_core::conformance;
use craft_ai_core::model::InputRole;
use craft_ai_core::payload::Payload;
use craft_ai_core::{ErrorKind, JobState, ModelId, Provider, Request};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use support::{Reply, Server, http_bytes};

const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\nPNG data";
const MP4_BYTES: &[u8] = b"\0\0\0\x18ftypisom video bytes";

fn workflow(cap: &str) -> Workflow {
    Workflow::import(
        "local-workflow",
        cap,
        json!({
            "1":{"class_type":"CLIPTextEncode","inputs":{"text":"old"}},
            "2":{"class_type":"SaveOutput","inputs":{"data":["1",0]}}
        }),
        Binding::new("1", "CLIPTextEncode", "text"),
        "2",
    )
    .unwrap()
}
fn installed() -> String {
    json!({
        "CLIPTextEncode":{"input":{"required":{"text":["STRING",{}]}}},
        "SaveOutput":{"input":{"required":{"data":["IMAGE",{}]}}}
    })
    .to_string()
}
fn request(cap: &str) -> Request {
    Request::new(CapabilityId::parse(cap).unwrap())
        .with_input(InputRole::Prompt, Payload::text("a lighthouse"))
}
fn provider(s: &Server, cap: &str) -> ComfyProvider {
    ComfyProvider::new(ComfyConfig::new(s.url.trim_end_matches("/v1")).with_workflow(workflow(cap)))
        .unwrap()
}

#[test]
fn model_probe_refuses_a_workflow_with_missing_nodes() {
    let s = Server::start(|r| match r.path.as_str() {
        "/system_stats" => Reply::Json(200, "{}".into()),
        "/object_info" => Reply::Json(
            200,
            json!({"CLIPTextEncode":{"input":{"required":{"text":["STRING",{}]}}}}).to_string(),
        ),
        _ => Reply::Json(404, "{}".into()),
    });
    let p = provider(&s, "image.generate");
    assert_eq!(p.models().unwrap_err().kind, ErrorKind::Unsupported);
    assert!(!s.recorded().iter().any(|r| r.path == "/prompt"));
}

#[test]
fn submit_refuses_an_uninstalled_node_before_sending_prompt() {
    let s = Server::start(|r| match r.path.as_str() {
        "/object_info" => Reply::Json(
            200,
            json!({"CLIPTextEncode":{"input":{"required":{"text":["STRING",{}]}}}}).to_string(),
        ),
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"should-not-run"}"#.into()),
        _ => Reply::Json(404, "{}".into()),
    });
    let p = provider(&s, "image.generate");
    let err = p
        .submit(
            &ModelId::new("local-workflow").unwrap(),
            &request("image.generate"),
        )
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert!(!s.recorded().iter().any(|r| r.path == "/prompt"));
}

#[test]
fn image_workflow_submits_and_polls_only_its_output_node() {
    let s = Server::start(|r| match r.path.as_str() {
        "/system_stats" => Reply::Json(200, "{}".into()),
        "/object_info" => Reply::Json(200, installed()),
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"job-123"}"#.into()),
        "/history/job-123" => Reply::Json(
            200,
            json!({"job-123":{"status":{"completed":true},
            "outputs":{"2":{"images":[{"filename":"kite.png","subfolder":"","type":"output"}]},
            "99":{"images":[{"filename":"wrong.png","type":"output"}]}}}})
            .to_string(),
        ),
        "/view?filename=kite.png&type=output" => http_bytes(200, &[], PNG_BYTES),
        _ => Reply::Json(404, "{}".into()),
    });
    let p = provider(&s, "image.generate");
    assert_eq!(p.models().unwrap().len(), 1);
    let id = p
        .submit(
            &ModelId::new("local-workflow").unwrap(),
            &request("image.generate"),
        )
        .unwrap();
    let snapshot = p.poll(&id).unwrap();
    assert_eq!(snapshot.state, JobState::Succeeded);
    assert_eq!(snapshot.outputs().count(), 1);
    assert!(matches!(&snapshot.outputs().next().unwrap().payload,
        Payload::Inline { media_type, data } if media_type.as_str() == "image/png" && data == PNG_BYTES));
    assert!(p.poll(&id).unwrap().events.is_empty());
    let recorded = s.recorded();
    assert_eq!(
        recorded
            .iter()
            .filter(|r| r.path.starts_with("/view"))
            .count(),
        1
    );
    assert_eq!(
        recorded
            .iter()
            .find(|r| r.path == "/prompt")
            .unwrap()
            .body_json()["prompt"]["1"]["inputs"]["text"],
        "a lighthouse"
    );
}

#[test]
fn video_workflow_waits_then_fetches_video() {
    let s = Server::start(|r| match r.path.as_str() {
        "/object_info" => Reply::Json(200, installed()),
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"job-456"}"#.into()),
        "/history/job-456" => Reply::Json(
            200,
            json!({"job-456":{"status":{"completed":true},
            "outputs":{"2":{"videos":[{"filename":"movie.mp4","type":"output"}]}}}})
            .to_string(),
        ),
        "/view?filename=movie.mp4&type=output" => http_bytes(200, &[], MP4_BYTES),
        _ => Reply::Json(404, "{}".into()),
    });
    let p = provider(&s, "video.generate");
    let id = p
        .submit(
            &ModelId::new("local-workflow").unwrap(),
            &request("video.generate"),
        )
        .unwrap();
    let result = p.poll(&id).unwrap();
    assert_eq!(result.state, JobState::Succeeded);
    assert!(matches!(&result.outputs().next().unwrap().payload,
        Payload::Inline { media_type, data } if media_type.as_str() == "video/mp4" && data == MP4_BYTES));
}

#[test]
fn unsupported_request_makes_no_traffic() {
    let s = Server::start(|_| Reply::Json(200, "{}".into()));
    let p = provider(&s, "image.generate");
    let req = request("image.generate").with_input(
        InputRole::Mask,
        Payload::inline("image/png", vec![1]).unwrap(),
    );
    assert_eq!(
        p.submit(&ModelId::new("local-workflow").unwrap(), &req)
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(s.count(), 0);
}

#[test]
fn cancellation_targets_its_own_prompt_never_global() {
    let s = Server::start(|r| match r.path.as_str() {
        "/object_info" => Reply::Json(200, installed()),
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"job-789"}"#.into()),
        "/queue" | "/interrupt" => Reply::Json(200, "{}".into()),
        _ => Reply::Json(404, "{}".into()),
    });
    let p = ComfyProvider::new(
        ComfyConfig::new(s.url.trim_end_matches("/v1"))
            .with_workflow(workflow("image.generate"))
            .with_targeted_interrupt(true),
    )
    .unwrap();
    let id = p
        .submit(
            &ModelId::new("local-workflow").unwrap(),
            &request("image.generate"),
        )
        .unwrap();
    p.cancel(&id).unwrap();
    assert_eq!(p.poll(&id).unwrap().state, JobState::Cancelled);
    let r = s.recorded();
    assert_eq!(
        r.iter().find(|r| r.path == "/queue").unwrap().body_json(),
        json!({"delete":["job-789"]})
    );
    assert_eq!(
        r.iter()
            .find(|r| r.path == "/interrupt")
            .unwrap()
            .body_json(),
        json!({"prompt_id":"job-789"})
    );
}

#[test]
fn remote_http_and_missing_history_are_not_misreported_as_success() {
    assert!(
        ComfyProvider::new(
            ComfyConfig::new("http://not-local.example").with_workflow(workflow("image.generate"))
        )
        .is_err()
    );
    let s = Server::start(|r| match r.path.as_str() {
        "/object_info" => Reply::Json(200, installed()),
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"job-1"}"#.into()),
        "/history/job-1" => Reply::Json(200, "{}".into()),
        _ => Reply::Json(404, "{}".into()),
    });
    let p = provider(&s, "image.generate");
    let id = p
        .submit(
            &ModelId::new("local-workflow").unwrap(),
            &request("image.generate"),
        )
        .unwrap();
    assert_eq!(p.poll(&id).unwrap().state, JobState::Queued);
}

#[test]
fn model_probe_does_not_follow_redirect_to_another_server() {
    let destination = Server::start(|_| Reply::Json(200, "{}".into()));
    let redirected_url = destination.url.trim_end_matches("/v1").to_owned();
    let source = Server::start(move |_| {
        Reply::JsonWith(
            302,
            vec![("Location", format!("{redirected_url}/system_stats"))],
            String::new(),
        )
    });
    let p = provider(&source, "image.generate");
    assert!(p.models().is_err());
    assert_eq!(destination.count(), 0);
}

#[test]
fn cancellation_is_off_until_targeted_interrupt_is_confirmed_for_the_server() {
    let s = Server::start(|r| match r.path.as_str() {
        "/object_info" => Reply::Json(200, installed()),
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"job-old-server"}"#.into()),
        _ => Reply::Json(200, "{}".into()),
    });
    let p = provider(&s, "image.generate");
    let id = p
        .submit(
            &ModelId::new("local-workflow").unwrap(),
            &request("image.generate"),
        )
        .unwrap();
    assert_eq!(p.cancel(&id).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(s.count(), 2);
}

#[test]
fn cancelled_job_is_idempotent_and_conformant() {
    let s = Server::start(|r| match r.path.as_str() {
        "/object_info" => Reply::Json(200, installed()),
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"job-cancel"}"#.into()),
        "/history/job-cancel" => Reply::Json(200, "{}".into()),
        "/queue" | "/interrupt" => Reply::Json(200, "{}".into()),
        _ => Reply::Json(404, "{}".into()),
    });
    let p = ComfyProvider::new(
        ComfyConfig::new(s.url.trim_end_matches("/v1"))
            .with_workflow(workflow("image.generate"))
            .with_targeted_interrupt(true),
    )
    .unwrap();
    conformance::run_cancel(
        &p,
        &ModelId::new("local-workflow").unwrap(),
        &request("image.generate"),
    )
    .unwrap();
    assert_eq!(
        s.recorded()
            .iter()
            .filter(|r| r.path == "/interrupt")
            .count(),
        1
    );
}

#[test]
fn server_refusal_is_actionable_and_bounded() {
    let s = Server::start(|r| match r.path.as_str() {
        "/object_info" => Reply::Json(200, installed()),
        "/prompt" => Reply::Json(400, r#"{"error":"bad graph"}"#.into()),
        _ => Reply::Json(404, "{}".into()),
    });
    let p = provider(&s, "image.generate");
    assert_eq!(
        p.submit(
            &ModelId::new("local-workflow").unwrap(),
            &request("image.generate")
        )
        .unwrap_err()
        .kind,
        ErrorKind::InvalidRequest
    );
}

#[test]
fn history_gap_does_not_regress_a_running_job_to_queued() {
    let count = Arc::new(AtomicUsize::new(0));
    let count_in_server = Arc::clone(&count);
    let s = Server::start(move |r| match r.path.as_str() {
        "/object_info" => Reply::Json(200, installed()),
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"job-gap"}"#.into()),
        "/history/job-gap" => {
            if count_in_server.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::Json(
                    200,
                    json!({"job-gap":{"status":{"completed":false}}}).to_string(),
                )
            } else {
                Reply::Json(200, "{}".into())
            }
        }
        _ => Reply::Json(404, "{}".into()),
    });
    let p = provider(&s, "image.generate");
    let id = p
        .submit(
            &ModelId::new("local-workflow").unwrap(),
            &request("image.generate"),
        )
        .unwrap();
    assert_eq!(
        p.poll(&id).unwrap().state,
        JobState::Running { percent: None }
    );
    assert_eq!(
        p.poll(&id).unwrap().state,
        JobState::Running { percent: None }
    );
}

#[test]
fn provider_passes_shared_conformance_without_network_for_bad_input() {
    let s = Server::start(|r| match r.path.as_str() {
        "/object_info" => Reply::Json(200, installed()),
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"job-conformance"}"#.into()),
        "/history/job-conformance" => Reply::Json(
            200,
            json!({"job-conformance":{
            "status":{"completed":true},"outputs":{"2":{"images":[{
                "filename":"ok.png","type":"output"}]}}}})
            .to_string(),
        ),
        "/view?filename=ok.png&type=output" => http_bytes(200, &[], PNG_BYTES),
        _ => Reply::Json(404, "{}".into()),
    });
    let p = provider(&s, "image.generate");
    let m = ModelId::new("local-workflow").unwrap();
    assert_eq!(
        conformance::run_lifecycle(&p, &m, &request("image.generate"))
            .unwrap()
            .state,
        JobState::Succeeded
    );
    conformance::run_unknown_job(&p).unwrap();
    conformance::run_rejects(
        &p,
        &m,
        &request("image.generate").streaming(),
        ErrorKind::Unsupported,
    )
    .unwrap();
    assert_eq!(s.count(), 4);
}

#[test]
fn output_path_cannot_escape_to_other_local_files() {
    let s = Server::start(|r| match r.path.as_str() {
        "/object_info" => Reply::Json(200, installed()),
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"job-path"}"#.into()),
        "/history/job-path" => Reply::Json(
            200,
            json!({"job-path":{"status":{"completed":true},
            "outputs":{"2":{"images":[{"filename":"private..png","type":"output"}]}}}})
            .to_string(),
        ),
        _ => Reply::Json(404, "{}".into()),
    });
    let p = provider(&s, "image.generate");
    let id = p
        .submit(
            &ModelId::new("local-workflow").unwrap(),
            &request("image.generate"),
        )
        .unwrap();
    assert!(matches!(
        p.poll(&id).unwrap().state,
        JobState::Failed { .. }
    ));
    assert_eq!(
        s.recorded()
            .iter()
            .filter(|r| r.path.starts_with("/view"))
            .count(),
        0
    );
}

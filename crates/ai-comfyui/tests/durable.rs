#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use craft_ai_comfyui::{Binding, ComfyConfig, ComfyProvider, Workflow};
use craft_ai_core::capability::CapabilityId;
use craft_ai_core::model::InputRole;
use craft_ai_core::payload::Payload;
use craft_ai_core::{JobState, ModelId, Provider, Request};
use serde_json::json;
use support::{Reply, Server, http_bytes};

const VIDEO: &[u8] = b"\0\0\0\x18ftypisom video bytes";

fn workflow() -> Workflow {
    Workflow::import(
        "local-video",
        "video.generate",
        json!({
            "1":{"class_type":"CLIPTextEncode","inputs":{"text":"old"}},
            "2":{"class_type":"SaveOutput","inputs":{"data":["1",0]}}
        }),
        Binding::new("1", "CLIPTextEncode", "text"),
        "2",
    )
    .unwrap()
}

fn server(output: &'static [u8]) -> Server {
    Server::start(move |r| match r.path.as_str() {
        "/object_info" => Reply::Json(
            200,
            json!({
                "CLIPTextEncode":{"input":{"required":{"text":["STRING",{}]}}},
                "SaveOutput":{"input":{"required":{"data":["IMAGE",{}]}}}
            })
            .to_string(),
        ),
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"job-1"}"#.into()),
        "/history/job-1" => Reply::Json(
            200,
            json!({
                "job-1":{"status":{"completed":true},"outputs":{"2":{"videos":[
                    {"filename":"movie.mp4","type":"output"}
                ]}}}
            })
            .to_string(),
        ),
        "/view?filename=movie.mp4&type=output" => http_bytes(200, &[], output),
        _ => Reply::Json(404, "{}".into()),
    })
}

fn run(config: ComfyConfig) -> craft_ai_core::job::JobSnapshot {
    let provider = ComfyProvider::new(config.with_workflow(workflow())).unwrap();
    let request = Request::new(CapabilityId::parse("video.generate").unwrap())
        .with_input(InputRole::Prompt, Payload::text("a lighthouse"));
    let id = provider
        .submit(&ModelId::new("local-video").unwrap(), &request)
        .unwrap();
    provider.poll(&id).unwrap()
}

#[test]
fn completed_video_is_saved_as_a_durable_local_handle() {
    let dir = tempfile::tempdir().unwrap();
    let server = server(VIDEO);
    let result =
        run(ComfyConfig::new(server.url.trim_end_matches("/v1")).with_output_dir(dir.path()));
    assert_eq!(result.state, JobState::Succeeded);
    let output = result.outputs().next().unwrap();
    let Payload::Handle(handle) = &output.payload else {
        panic!("expected a local file handle")
    };
    assert_eq!(handle.media_type.as_str(), "video/mp4");
    assert_eq!(handle.size_bytes, Some(VIDEO.len() as u64));
    let path = url::Url::parse(&handle.uri)
        .unwrap()
        .to_file_path()
        .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), VIDEO);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn redirected_output_is_not_fetched_or_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let destination = Server::start(|_| Reply::Json(200, "stolen video".into()));
    let location = format!("{}/view", destination.url.trim_end_matches("/v1"));
    let source = Server::start(move |r| match r.path.as_str() {
        "/object_info" => Reply::Json(
            200,
            json!({
                "CLIPTextEncode":{"input":{"required":{"text":["STRING",{}]}}},
                "SaveOutput":{"input":{"required":{"data":["IMAGE",{}]}}}
            })
            .to_string(),
        ),
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"job-1"}"#.into()),
        "/history/job-1" => Reply::Json(
            200,
            json!({
                "job-1":{"status":{"completed":true},"outputs":{"2":{"videos":[
                    {"filename":"movie.mp4","type":"output"}
                ]}}}
            })
            .to_string(),
        ),
        "/view?filename=movie.mp4&type=output" => {
            Reply::JsonWith(302, vec![("Location", location.clone())], String::new())
        }
        _ => Reply::Json(404, "{}".into()),
    });
    let result =
        run(ComfyConfig::new(source.url.trim_end_matches("/v1")).with_output_dir(dir.path()));
    assert!(matches!(result.state, JobState::Failed { .. }));
    assert_eq!(destination.count(), 0);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn oversized_output_fails_without_leaving_a_partial_asset() {
    let dir = tempfile::tempdir().unwrap();
    let server = server(VIDEO);
    let result = run(ComfyConfig::new(server.url.trim_end_matches("/v1"))
        .with_output_dir(dir.path())
        .with_max_output_bytes(5));
    assert!(matches!(result.state, JobState::Failed { .. }));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

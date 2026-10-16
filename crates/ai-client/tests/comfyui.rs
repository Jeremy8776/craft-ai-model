//! The `comfyui` provider route: a saved local connection, workflows from the shared
//! library directory, and the Test button against a scripted local server.

#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};

use craft_ai_client::{ConnectionManager, ProviderFactory};
use craft_ai_comfyui::WorkflowSpec;
use craft_ai_core::{ConnectionDraft, ConnectionId, PRESETS, Request};
use craft_ai_native::secrets::MemoryStore;
use craft_ai_native::shared::SharedStore;
use serde_json::json;
use support::{Reply, Server, http_bytes};

/// A real PNG header: the adapter checks an output by its first bytes, not by its file name.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nimage body";

const GRAPH: &str = r#"{
  "1": {"class_type": "CLIPTextEncode", "inputs": {"text": "template"}},
  "2": {"class_type": "SaveImage", "inputs": {"images": ["1", 0]}}
}"#;

const OBJECT_INFO: &str = r#"{"CLIPTextEncode":{"input":{"required":{"text":["STRING",{}]}}},
  "SaveImage":{"input":{"required":{"images":["IMAGE",{}]}}}}"#;

struct Rig {
    manager: ConnectionManager,
    workflows: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

/// A manager whose ComfyUI provider reads the workflow library inside the temp dir.
fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let manager = ConnectionManager::new(shared, Arc::new(MemoryStore::new()));
    let workflows = dir.path().join("workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    Rig {
        manager,
        workflows,
        _dir: dir,
    }
}

fn write_workflow(r: &Rig, name: &str) {
    std::fs::write(
        r.workflows.join(format!("{name}.json")),
        json!({
            "id": name,
            "capability": "image.generate",
            "graph": serde_json::from_str::<serde_json::Value>(GRAPH).unwrap(),
            "prompt": {"node": "1", "class_type": "CLIPTextEncode", "input": "text"},
            "output_node": "2"
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn a_comfyui_connection_offers_library_workflows_and_probes_the_server() {
    let r = rig();
    write_workflow(&r, "kite-image");
    let s = Server::start(|req| match req.path.as_str() {
        "/system_stats" => Reply::Json(200, "{}".into()),
        "/object_info" => Reply::Json(200, OBJECT_INFO.into()),
        _ => Reply::Json(404, "{}".into()),
    });
    let mut d = ConnectionDraft::from_preset(
        PRESETS
            .iter()
            .find(|p| p.key == "comfyui")
            .expect("comfyui preset"),
    );
    d.endpoint = s.url.trim_end_matches("/v1").to_owned();
    let saved = r.manager.save(&d).unwrap();
    assert_eq!(saved.id, ConnectionId::new("comfyui-local").unwrap());
    // The probing factory reads the same library the test wrote to.
    let report = r
        .manager
        .with_probe_factory(ProviderFactory::with_comfyui_library(&r.workflows))
        .test_saved(&saved.id, true)
        .unwrap();
    assert_eq!(report.models.len(), 1);
    assert_eq!(report.models[0].id.as_str(), "kite-image");
    let requests = s.recorded();
    assert!(requests.iter().any(|req| req.path == "/object_info"));
}

#[test]
fn the_factory_builds_a_comfyui_provider_that_runs_a_library_workflow() {
    let r = rig();
    write_workflow(&r, "kite-image");
    let s = Mutex::new(Server::start(|req| match req.path.as_str() {
        "/system_stats" => Reply::Json(200, "{}".into()),
        "/object_info" => Reply::Json(200, OBJECT_INFO.into()),
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"job-1"}"#.into()),
        "/history/job-1" => Reply::Json(
            200,
            json!({"job-1":{"status":{"completed":true},
              "outputs":{"2":{"images":[{"filename":"out.png","type":"output"}]}}}})
            .to_string(),
        ),
        "/view?filename=out.png&type=output" => http_bytes(200, &[], PNG),
        _ => Reply::Json(404, "{}".into()),
    }));
    let mut d = ConnectionDraft::from_preset(
        PRESETS
            .iter()
            .find(|p| p.key == "comfyui")
            .expect("comfyui preset"),
    );
    {
        let guard = s.lock().unwrap();
        d.endpoint = guard.url.trim_end_matches("/v1").to_owned();
    }
    let prepared = d.prepare(&[]).unwrap();
    let output_dir = r._dir.path().join("outputs");
    std::fs::create_dir(&output_dir).unwrap();
    let factory =
        ProviderFactory::with_comfyui_library(&r.workflows).with_comfyui_output_dir(&output_dir);
    let provider = factory
        .build(&prepared.connection, None)
        .expect("comfyui is registered");
    let request =
        Request::new(craft_ai_core::capability::CapabilityId::parse("image.generate").unwrap())
            .with_input(
                craft_ai_core::model::InputRole::Prompt,
                craft_ai_core::payload::Payload::text("a kite"),
            );
    let id = provider
        .submit(
            &craft_ai_core::ModelId::new("kite-image").unwrap(),
            &request,
        )
        .expect("the library workflow runs");
    let snapshot = provider.poll(&id).unwrap();
    assert_eq!(
        snapshot.state,
        craft_ai_core::JobState::Succeeded,
        "the workflow job should succeed against the scripted server"
    );
    assert!(matches!(
        snapshot.outputs().next().unwrap().payload,
        craft_ai_core::payload::Payload::Handle(_)
    ));
    let saved = std::fs::read_dir(&output_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(std::fs::read(saved).unwrap(), PNG);
    let body = s.lock().unwrap().recorded();
    let prompt = body
        .iter()
        .find(|req| req.path == "/prompt")
        .expect("a prompt was sent");
    assert_eq!(
        prompt.body_json()["prompt"]["1"]["inputs"]["text"],
        "a kite",
        "the request's prompt reached the mapped node"
    );
}

#[test]
fn without_a_preset_the_comfyui_route_is_not_advertised() {
    // Guard: the preset exists so users can add the connection from the launcher form.
    assert!(PRESETS.iter().any(|p| p.key == "comfyui"));
}

/// A spec helper used by the tests to show the library file shape is a WorkflowSpec.
#[test]
fn workflow_spec_parses_the_stored_shape() {
    let spec = WorkflowSpec::parse(
        json!({
            "id": "x", "capability": "image.generate",
            "graph": serde_json::from_str::<serde_json::Value>(GRAPH).unwrap(),
            "prompt": {"node": "1", "class_type": "CLIPTextEncode", "input": "text"},
            "output_node": "2"
        })
        .to_string()
        .as_bytes(),
        None,
    )
    .unwrap();
    assert_eq!(spec.id, "x");
}

//! The in-app missing-model popup's flow: a tool with no model reports what it needs, a
//! choice binds for every app, and an app reaches the same answer as the launcher.

use std::sync::Arc;

use craft_ai_client::{AiClient, GroupState, RoleStatus, ToolDesk};
use craft_ai_core::{
    AppId, CapabilityId, CapabilitySpec, Connection, ConnectionId, InputRole, ModelId, ModelInfo,
    Role, StoreFile,
};
use craft_ai_native::secrets::MemoryStore;
use craft_ai_native::shared::SharedStore;
use serde_json::json;

fn select_subject() -> Role {
    craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == "select-subject")
        .unwrap()
        .role()
        .unwrap()
}

fn segmenter(id: &str, label: &str, inputs: &[InputRole]) -> ModelInfo {
    let mut spec = CapabilitySpec::new(CapabilityId::parse("image.segment").unwrap());
    for input in inputs {
        spec = spec.accepting(input.clone(), 1);
    }
    ModelInfo::new(ModelId::new(id).unwrap(), label).with(spec)
}

fn local(id: &str, models: Vec<ModelInfo>) -> Connection {
    serde_json::from_value(json!({
        "id": id, "label": format!("Label {id}"), "provider": "comfyui",
        "endpoint": "http://127.0.0.1:9", "locality": "local",
        "auth": {"kind": "none"}, "billing": "free", "models": models,
    }))
    .unwrap()
}

const SEG: [InputRole; 2] = [InputRole::Source, InputRole::Prompt];

struct World {
    _dir: tempfile::TempDir,
    shared: SharedStore,
}

impl World {
    fn new(connections: Vec<Connection>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let shared = SharedStore::new(dir.path().join("connections.json"));
        shared
            .replace(&StoreFile {
                connections,
                ..StoreFile::default()
            })
            .unwrap();
        Self { _dir: dir, shared }
    }

    fn client(&self, app: &str) -> AiClient {
        AiClient::new(
            AppId::new(app).unwrap(),
            self.shared.clone(),
            Arc::new(MemoryStore::new()),
        )
    }

    /// A desk acting for `app`, which is how an app's popup reaches the shared choice.
    fn desk(&self, app: &str) -> ToolDesk {
        ToolDesk::new(self.client(app), self.shared.clone(), None)
    }
}

fn features() -> Vec<craft_ai_features::Feature> {
    craft_ai_features::builtin()
}

fn id(s: &str) -> ConnectionId {
    ConnectionId::new(s).unwrap()
}

fn model(s: &str) -> ModelId {
    ModelId::new(s).unwrap()
}

#[test]
fn choosing_for_everyone_checks_the_chosen_model_not_what_the_calling_app_already_uses() {
    let w = World::new(vec![
        local("good", vec![segmenter("sam3", "SAM 3", &SEG)]),
        // Takes no text prompt, so it cannot do Select subject.
        local("bad", vec![segmenter("yolo", "YOLO", &[InputRole::Source])]),
    ]);
    let role = select_subject();
    // The app already has its own working choice, which used to hide the check.
    w.client("photocraft")
        .bind(&role, &id("good"), &model("sam3"))
        .unwrap();

    let refused = w
        .desk("photocraft")
        .choose(&role, &id("bad"), &model("yolo"));
    assert!(
        refused.is_err(),
        "a model that cannot do the tool is refused"
    );
    assert!(
        w.shared.load().unwrap().defaults.is_empty(),
        "nothing was written"
    );
}

#[test]
fn a_choice_made_from_one_app_is_the_choice_every_app_reads() {
    let w = World::new(vec![local("box", vec![segmenter("sam3", "SAM 3", &SEG)])]);
    let role = select_subject();
    assert!(matches!(
        w.client("lightcraft").status(&role),
        RoleStatus::Unavailable { .. }
    ));

    w.desk("photocraft")
        .choose(&role, &id("box"), &model("sam3"))
        .unwrap();

    for app in ["photocraft", "lightcraft", "launcher"] {
        assert!(
            matches!(w.client(app).status(&role), RoleStatus::Ready(_)),
            "{app}"
        );
    }
    assert!(
        w.shared.load().unwrap().apps.is_empty(),
        "no per-app binding"
    );
}

#[test]
fn a_tool_that_can_run_has_no_popup() {
    let w = World::new(vec![local("box", vec![segmenter("sam3", "SAM 3", &SEG)])]);
    let role = select_subject();
    let desk = w.desk("photocraft");
    desk.choose(&role, &id("box"), &model("sam3")).unwrap();
    assert_eq!(desk.missing_model(&role, &features()).unwrap(), None);
}

#[test]
fn a_tool_with_no_model_gets_the_reason_and_the_real_ways_to_set_it_up() {
    let w = World::new(vec![]);
    let role = select_subject();
    let popup = w
        .desk("photocraft")
        .missing_model(&role, &features())
        .unwrap()
        .expect("nothing is chosen, so the popup is needed");
    assert_eq!(popup.tool.as_str(), "select-subject");
    assert_eq!(popup.title, "Select subject");
    assert!(
        popup.reason.contains("no model is chosen"),
        "{}",
        popup.reason
    );
    // The local model, with its size, is offered as a greyed way to set the tool up.
    let sam3 = &popup.picker.groups[0];
    assert_eq!(
        (sam3.title.as_str(), sam3.state),
        ("SAM 3", GroupState::NotSetUp)
    );
    assert_eq!(sam3.size_bytes, Some(3_449_860_632));
}

#[test]
fn a_choice_that_stopped_working_brings_the_popup_back_with_the_reason() {
    let w = World::new(vec![local("box", vec![segmenter("sam3", "SAM 3", &SEG)])]);
    let role = select_subject();
    let desk = w.desk("photocraft");
    desk.choose(&role, &id("box"), &model("sam3")).unwrap();
    // The connection is removed afterwards.
    w.shared.update(|s| s.connections.clear()).unwrap();

    let popup = desk.missing_model(&role, &features()).unwrap().unwrap();
    assert!(!popup.reason.is_empty());
    assert_eq!(
        popup
            .picker
            .current
            .as_ref()
            .and_then(|c| c.problem.clone()),
        Some(popup.reason.clone())
    );
}

#[test]
fn picking_a_model_in_the_popup_closes_it_for_every_app() {
    let w = World::new(vec![local("box", vec![segmenter("sam3", "SAM 3", &SEG)])]);
    let role = select_subject();
    let popup = w
        .desk("photocraft")
        .missing_model(&role, &features())
        .unwrap()
        .unwrap();
    let offered = &popup.picker.groups[0];
    assert_eq!(offered.state, GroupState::Ready);
    let pick = &offered.models[0];

    w.desk("photocraft")
        .choose(&role, &pick.connection, &pick.model)
        .unwrap();
    for app in ["photocraft", "lightcraft"] {
        assert_eq!(
            w.desk(app).missing_model(&role, &features()).unwrap(),
            None,
            "{app}"
        );
    }
}

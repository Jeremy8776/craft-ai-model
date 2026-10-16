//! One binding, every app: a tool has one suite-wide id, the launcher's choice for it is
//! stored once, and every app that has the tool reads that same choice with no setup of its own.

use std::sync::Arc;

use craft_ai_client::{AiClient, RoleStatus, ToolDesk};
use craft_ai_core::{
    AppId, CapabilityId, CapabilitySpec, Connection, ConnectionId, InputRole, ModelId, ModelInfo,
    Role, StoreFile,
};
use craft_ai_native::secrets::MemoryStore;
use craft_ai_native::shared::SharedStore;
use serde_json::json;

/// The catalogue's own Select subject tool, so the test breaks if its id or inputs drift.
fn select_subject() -> Role {
    craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == "select-subject")
        .unwrap()
        .role()
        .unwrap()
}

/// A local model that takes the tool's inputs (source image and a text prompt).
fn segmenter() -> Connection {
    let model = ModelInfo::new(ModelId::new("sam3").unwrap(), "SAM 3").with(
        CapabilitySpec::new(CapabilityId::parse("image.segment").unwrap())
            .accepting(InputRole::Source, 1)
            .accepting(InputRole::Prompt, 1),
    );
    serde_json::from_value(json!({
        "id": "local-seg", "label": "On this computer", "provider": "comfyui",
        "endpoint": "http://127.0.0.1:9", "locality": "local",
        "auth": {"kind": "none"}, "billing": "free",
        "models": [model],
    }))
    .unwrap()
}

struct World {
    _dir: tempfile::TempDir,
    shared: SharedStore,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let shared = SharedStore::new(dir.path().join("connections.json"));
        shared
            .replace(&StoreFile {
                connections: vec![segmenter()],
                ..StoreFile::default()
            })
            .unwrap();
        Self { _dir: dir, shared }
    }

    fn app(&self, id: &str) -> AiClient {
        AiClient::new(
            AppId::new(id).unwrap(),
            self.shared.clone(),
            Arc::new(MemoryStore::new()),
        )
    }

    /// What the launcher does when a person picks SAM 3 for Select subject.
    fn launcher_picks_sam3(&self) {
        let desk = ToolDesk::new(self.app("launcher"), self.shared.clone(), None);
        desk.choose(
            &select_subject(),
            &ConnectionId::new("local-seg").unwrap(),
            &ModelId::new("sam3").unwrap(),
        )
        .unwrap();
    }
}

fn ready(status: RoleStatus) -> (String, String) {
    match status {
        RoleStatus::Ready(r) => (
            r.connection.as_str().to_owned(),
            r.model.as_str().to_owned(),
        ),
        RoleStatus::Unavailable { reason } => panic!("expected ready, got: {reason}"),
    }
}

#[test]
fn two_apps_resolve_the_one_stored_binding_with_no_setup_of_their_own() {
    let w = World::new();
    w.launcher_picks_sam3();
    let role = select_subject();

    let photocraft = ready(w.app("photocraft").status(&role));
    let lightcraft = ready(w.app("lightcraft").status(&role));

    assert_eq!(photocraft, ("local-seg".to_owned(), "sam3".to_owned()));
    assert_eq!(lightcraft, photocraft);

    // One choice stored once: a suite default, and nothing written for either app.
    let store = w.shared.load().unwrap();
    assert_eq!(store.defaults.len(), 1);
    assert!(store.apps.is_empty(), "no per-app binding was needed");
}

#[test]
fn a_tool_with_no_model_asks_for_a_model_in_every_app_and_is_never_called_broken() {
    let w = World::new();
    let role = select_subject();
    for app in ["photocraft", "lightcraft"] {
        match w.app(app).status(&role) {
            RoleStatus::Unavailable { reason } => {
                assert!(reason.contains("no model is chosen"), "{app}: {reason}");
                assert!(reason.contains("select-subject"), "{app}: {reason}");
            }
            RoleStatus::Ready(_) => panic!("{app}: nothing is bound yet"),
        }
    }

    // The launcher picks once and both apps, already running, now report ready.
    w.launcher_picks_sam3();
    for app in ["photocraft", "lightcraft"] {
        ready(w.app(app).status(&role));
    }
}

#[test]
fn an_app_s_own_choice_moves_only_that_app_while_the_launcher_default_stands() {
    let w = World::new();
    w.launcher_picks_sam3();
    let role = select_subject();

    // Another model that can do the same job, offered by a second connection.
    let mut other = segmenter();
    other.id = ConnectionId::new("other-seg").unwrap();
    w.shared.update(|s| s.connections.push(other)).unwrap();

    w.app("lightcraft")
        .bind(
            &role,
            &ConnectionId::new("other-seg").unwrap(),
            &ModelId::new("sam3").unwrap(),
        )
        .unwrap();

    assert_eq!(ready(w.app("lightcraft").status(&role)).0, "other-seg");
    assert_eq!(ready(w.app("photocraft").status(&role)).0, "local-seg");
}

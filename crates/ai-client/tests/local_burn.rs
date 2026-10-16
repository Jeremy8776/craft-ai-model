//! Burn is a third local backend for the same tool, beside ComfyUI and Candle. This is the slot:
//! the route is registered, bound, read and refused exactly as the other two are. No inference runs
//! here, and no Burn runtime exists yet, so every app reports "no adapter" for it until one does.
use std::path::PathBuf;
use std::sync::Arc;

use craft_ai_client::{AiClient, GroupState, RoleStatus, ToolDesk};
use craft_ai_core::{AppId, Role, StoreFile};
use craft_ai_features::Feature;
use craft_ai_native::secrets::MemoryStore;
use craft_ai_native::shared::SharedStore;

const SAM3_URL: &str = "http://127.0.0.1:8000";

fn feature() -> Feature {
    craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == "select-subject")
        .unwrap()
}

fn role() -> Role {
    feature().role().unwrap()
}

struct World {
    dir: tempfile::TempDir,
    shared: SharedStore,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let shared = SharedStore::new(dir.path().join("connections.json"));
        shared.replace(&StoreFile::default()).unwrap();
        Self { dir, shared }
    }
    fn desk(&self) -> ToolDesk {
        ToolDesk::new(self.client("launcher"), self.shared.clone(), None)
    }
    fn client(&self, app: &str) -> AiClient {
        AiClient::new(
            AppId::new(app).unwrap(),
            self.shared.clone(),
            Arc::new(MemoryStore::new()),
        )
    }
    fn weights(&self) -> PathBuf {
        let p = self.dir.path().join("sam3.safetensors");
        std::fs::write(&p, b"not really weights").unwrap();
        p
    }
}

#[test]
fn the_catalogue_lists_candle_then_burn_for_select_subject() {
    let f = feature();
    let providers: Vec<&str> = f
        .local
        .as_ref()
        .unwrap()
        .native
        .iter()
        .map(|n| n.provider.as_str())
        .collect();
    assert_eq!(providers, ["candle", "burn"]);
}

#[test]
fn burn_is_registered_beside_candle_without_taking_the_binding() {
    let w = World::new();
    let file = w.weights();
    let first = w.desk().enable_native(&feature(), &file).unwrap();
    let burn = w
        .desk()
        .enable_native_with(&feature(), "burn", &file)
        .unwrap();
    assert!(first.bound);
    assert!(!burn.bound, "a choice already made is left alone");
    assert_ne!(first.connection, burn.connection);
    let store = w.shared.load().unwrap();
    let providers: Vec<&str> = store
        .connections
        .iter()
        .map(|c| c.provider.as_str())
        .collect();
    assert_eq!(providers, ["candle", "burn"]);
    let burn_connection = store
        .connections
        .iter()
        .find(|c| c.provider == "burn")
        .unwrap();
    assert_eq!(
        burn_connection.models[0].local_file.as_deref(),
        file.to_str(),
        "both runtimes read the one weights file"
    );
}

#[test]
fn a_tool_bound_to_burn_is_read_as_burn_and_refused_by_the_other_adapters() {
    let w = World::new();
    let file = w.weights();
    w.desk()
        .enable_native_with(&feature(), "burn", &file)
        .unwrap();
    for app in ["photocraft", "lightcraft"] {
        let client = w.client(app);
        let route = client.local_route_for(&role()).unwrap().unwrap();
        assert_eq!(route.provider, "burn");
        assert_eq!(
            client.local_file_for(&role(), "burn").unwrap(),
            Some(file.clone())
        );
        let candle = client
            .local_file_for(&role(), "candle")
            .unwrap_err()
            .message;
        assert_eq!(
            candle,
            "The model you chose can't be run by this app yet. Choose another in Set up AI."
        );
        let comfy = client.endpoint_for(&role()).unwrap_err().message;
        assert_eq!(
            comfy,
            "The model you chose can't be run by this app yet. Choose another in Set up AI."
        );
    }
}

#[test]
fn an_app_without_a_burn_adapter_is_not_ready_and_the_picker_says_so() {
    let w = World::new();
    w.desk()
        .enable_native_with(&feature(), "burn", &w.weights())
        .unwrap();
    let client = w.client("photocraft");
    match client.status(&role()) {
        RoleStatus::Unavailable { reason } => assert_eq!(
            reason,
            "This app can't run the chosen model yet. Choose another in Set up AI."
        ),
        RoleStatus::Ready(_) => panic!("no Burn adapter is registered anywhere yet"),
    }
    let view = client.picker(&role(), &[]).unwrap();
    let group = view
        .groups
        .iter()
        .find(|g| g.title.contains("any graphics card"))
        .expect("the Burn route is listed, greyed");
    assert_eq!(group.state, GroupState::NeedsAdapter);
    assert_eq!(group.note, "This app can't use this option yet.");
    assert!(
        group.setup.is_none(),
        "no setup button can install an app adapter"
    );
}

#[test]
fn the_picker_shows_comfyui_candle_and_burn_as_three_different_choices() {
    let w = World::new();
    let file = w.weights();
    w.desk().enable_local(&feature(), SAM3_URL).unwrap();
    w.desk().enable_native(&feature(), &file).unwrap();
    w.desk()
        .enable_native_with(&feature(), "burn", &file)
        .unwrap();
    let view = w.client("lightcraft").picker(&role(), &[]).unwrap();
    let mut titles: Vec<&str> = view.groups.iter().map(|g| g.title.as_str()).collect();
    assert_eq!(titles.len(), 3, "{titles:?}");
    titles.sort_unstable();
    titles.dedup();
    assert_eq!(titles.len(), 3, "each must be told apart: {titles:?}");
    assert!(
        titles.iter().any(|t| t.contains("Nvidia or Apple")),
        "{titles:?}"
    );
    assert!(
        titles.iter().any(|t| t.contains("any graphics card")),
        "{titles:?}"
    );
}

#[test]
fn a_backend_the_recipe_does_not_list_is_refused_and_nothing_is_written() {
    let w = World::new();
    let file = w.weights();
    for provider in ["tract", "ort", "comfyui", "", "Burn"] {
        let result = w.desk().enable_native_with(&feature(), provider, &file);
        assert!(result.is_err(), "{provider:?}");
    }
    assert!(w.shared.load().unwrap().connections.is_empty());
}

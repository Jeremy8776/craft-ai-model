//! One SAM 3 weights file, two runtimes: the launcher registers the built-in (Candle) route next to the
//! ComfyUI one, both for the same tool, and an app reads the exact file it is meant to load.
use std::path::PathBuf;
use std::sync::Arc;

use craft_ai_client::{AiClient, ToolDesk};
use craft_ai_core::{AppId, Role, StoreFile};
use craft_ai_features::Feature;
use craft_ai_native::secrets::MemoryStore;
use craft_ai_native::shared::SharedStore;

const SAM3_URL: &str = "http://127.0.0.1:8000";

fn feature(id: &str) -> Feature {
    craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == id)
        .unwrap()
}

fn role() -> Role {
    feature("select-subject").role().unwrap()
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
    fn desk(&self, app: &str) -> ToolDesk {
        ToolDesk::new(self.client(app), self.shared.clone(), None)
    }
    fn client(&self, app: &str) -> AiClient {
        AiClient::new(
            AppId::new(app).unwrap(),
            self.shared.clone(),
            Arc::new(MemoryStore::new()),
        )
    }
    /// A weights file that exists (the content does not matter here).
    fn weights(&self, name: &str) -> PathBuf {
        let p = self.dir.path().join(name);
        std::fs::write(&p, b"not really weights").unwrap();
        p
    }
}

#[test]
fn the_native_route_is_registered_bound_and_carries_the_file() {
    let w = World::new();
    let file = w.weights("sam3.safetensors");
    let done = w
        .desk("launcher")
        .enable_native(&feature("select-subject"), &file)
        .unwrap();
    assert!(done.bound);
    assert_eq!(done.connection.as_str(), "local-select-subject-candle");
    assert_eq!(done.model.as_str(), "sam3");
    let route = w
        .client("lightcraft")
        .local_route_for(&role())
        .unwrap()
        .unwrap();
    assert_eq!(route.provider, "candle");
    assert_eq!(route.endpoint, None);
    assert_eq!(route.file.as_deref(), file.to_str());
}

#[test]
fn two_apps_read_the_same_file() {
    let w = World::new();
    let file = w.weights("sam3.safetensors");
    w.desk("launcher")
        .enable_native(&feature("select-subject"), &file)
        .unwrap();
    for app in ["lightcraft", "photocraft"] {
        assert_eq!(
            w.client(app).local_file_for(&role(), "candle").unwrap(),
            Some(file.clone()),
            "{app}"
        );
    }
}

#[test]
fn the_comfyui_route_has_no_file_and_is_not_a_native_one() {
    let w = World::new();
    w.desk("launcher")
        .enable_local(&feature("select-subject"), SAM3_URL)
        .unwrap();
    let client = w.client("lightcraft");
    assert_eq!(client.local_route_for(&role()).unwrap().unwrap().file, None);
    let error = client
        .local_file_for(&role(), "candle")
        .unwrap_err()
        .message;
    assert_eq!(
        error,
        "The model you chose can't be run by this app yet. Choose another in Set up AI."
    );
}

#[test]
fn a_native_route_is_not_offered_as_a_comfyui_address() {
    let w = World::new();
    w.desk("launcher")
        .enable_native(&feature("select-subject"), &w.weights("sam3.safetensors"))
        .unwrap();
    assert_eq!(
        w.client("photocraft")
            .endpoint_for(&role())
            .unwrap_err()
            .message,
        "The model you chose can't be run by this app yet. Choose another in Set up AI."
    );
}

#[test]
fn nothing_bound_is_not_an_error() {
    let w = World::new();
    assert_eq!(
        w.client("lightcraft")
            .local_file_for(&role(), "candle")
            .unwrap(),
        None
    );
}

#[test]
fn a_choice_already_made_is_left_alone_and_both_routes_are_registered() {
    let w = World::new();
    w.desk("launcher")
        .enable_local(&feature("select-subject"), SAM3_URL)
        .unwrap();
    let done = w
        .desk("launcher")
        .enable_native(&feature("select-subject"), &w.weights("sam3.safetensors"))
        .unwrap();
    assert!(!done.bound, "the person's existing choice stays");
    assert_eq!(
        w.client("lightcraft")
            .local_route_for(&role())
            .unwrap()
            .unwrap()
            .provider,
        "comfyui"
    );
    let store = w.shared.load().unwrap();
    assert_eq!(store.connections.len(), 2);
}

#[test]
fn registering_again_moves_the_file_and_keeps_one_connection() {
    let w = World::new();
    w.desk("launcher")
        .enable_native(&feature("select-subject"), &w.weights("old.safetensors"))
        .unwrap();
    let new = w.weights("new.safetensors");
    let done = w
        .desk("launcher")
        .enable_native(&feature("select-subject"), &new)
        .unwrap();
    assert!(!done.bound, "already bound by the first call");
    assert_eq!(w.shared.load().unwrap().connections.len(), 1);
    assert_eq!(
        w.client("lightcraft")
            .local_file_for(&role(), "candle")
            .unwrap(),
        Some(new)
    );
}

#[test]
fn a_file_that_cannot_be_the_weights_is_refused_and_nothing_is_written() {
    let w = World::new();
    let desk = w.desk("launcher");
    let f = feature("select-subject");
    let missing = w.dir.path().join("nope.safetensors");
    let folder = w.dir.path().join("folder.safetensors");
    std::fs::create_dir(&folder).unwrap();
    let wrong_type = w.weights("sam3.bin");
    for (what, path) in [
        ("missing", missing),
        ("folder", folder),
        ("wrong extension", wrong_type),
        ("relative", PathBuf::from("sam3.safetensors")),
    ] {
        assert!(desk.enable_native(&f, &path).is_err(), "{what}");
    }
    assert!(w.shared.load().unwrap().connections.is_empty());
}

#[test]
fn a_relative_path_is_refused_even_when_it_points_at_a_real_file() {
    let w = World::new();
    let name = format!("lc-relative-{}.safetensors", std::process::id());
    std::fs::write(&name, b"x").unwrap();
    let refused = w
        .desk("launcher")
        .enable_native(&feature("select-subject"), std::path::Path::new(&name))
        .is_err();
    std::fs::remove_file(&name).unwrap();
    assert!(refused);
    assert!(w.shared.load().unwrap().connections.is_empty());
}

#[test]
fn a_runtime_that_is_not_ready_to_use_is_refused() {
    let w = World::new();
    let mut f = feature("select-subject");
    f.local.as_mut().unwrap().usable = false;
    assert!(
        w.desk("launcher")
            .enable_native(&f, &w.weights("sam3.safetensors"))
            .is_err()
    );
    let mut f = feature("select-subject");
    f.local.as_mut().unwrap().native.clear();
    assert!(
        w.desk("launcher")
            .enable_native(&f, &w.weights("sam3.safetensors"))
            .is_err()
    );
    assert!(w.shared.load().unwrap().connections.is_empty());
}

#[test]
fn a_tool_without_a_built_in_runtime_is_refused() {
    let w = World::new();
    let file = w.weights("sam3.safetensors");
    let others = craft_ai_features::builtin()
        .into_iter()
        .filter(|f| f.id != "select-subject");
    for f in others {
        assert!(
            w.desk("launcher").enable_native(&f, &file).is_err(),
            "{}",
            f.id
        );
    }
    assert!(w.shared.load().unwrap().connections.is_empty());
}

#[test]
fn a_file_that_vanished_is_an_error_not_unbound() {
    let w = World::new();
    let file = w.weights("sam3.safetensors");
    w.desk("launcher")
        .enable_native(&feature("select-subject"), &file)
        .unwrap();
    std::fs::remove_file(&file).unwrap();
    let error = w
        .client("lightcraft")
        .local_file_for(&role(), "candle")
        .unwrap_err()
        .message;
    assert_eq!(
        error,
        "The file for the model you chose is no longer there. Open Set up AI to fix it."
    );
}

#[test]
fn the_picker_offers_the_two_runtimes_of_one_model_side_by_side() {
    let w = World::new();
    w.desk("launcher")
        .enable_local(&feature("select-subject"), SAM3_URL)
        .unwrap();
    w.desk("launcher")
        .enable_native(&feature("select-subject"), &w.weights("sam3.safetensors"))
        .unwrap();
    let view = w.client("lightcraft").picker(&role(), &[]).unwrap();
    let titles: Vec<&str> = view.groups.iter().map(|g| g.title.as_str()).collect();
    assert_eq!(titles.len(), 2, "{titles:?}");
    assert_ne!(
        titles[0], titles[1],
        "the person must be able to tell them apart"
    );
    assert!(titles.iter().any(|t| t.contains("built in")), "{titles:?}");
}

#[test]
fn old_stores_without_the_field_load_and_it_is_not_written_when_absent() {
    let json = r#"{"id":"sam3","label":"SAM 3"}"#;
    let info: craft_ai_core::ModelInfo = serde_json::from_str(json).unwrap();
    assert_eq!(info.local_file, None);
    assert!(!serde_json::to_string(&info).unwrap().contains("local_file"));
}

//! The suite's one binding selects a concrete local backend and model, not just a Ready light.
use std::sync::Arc;

use craft_ai_client::{AiClient, ProviderFactory, RoleStatus, ToolDesk};
use craft_ai_core::{
    AppId, Auth, Billing, CapabilitySpec, Connection, ConnectionId, Locality, ModelId, ModelInfo,
    Role, StoreFile,
};
use craft_ai_native::secrets::MemoryStore;
use craft_ai_native::shared::SharedStore;

fn role() -> Role {
    craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == "select-subject")
        .unwrap()
        .role()
        .unwrap()
}

fn connection(id: &str, provider: &str, model: &str, endpoint: Option<&str>) -> Connection {
    let r = role();
    let mut spec = CapabilitySpec::new(r.capability);
    for input in r.inputs {
        spec = spec.accepting(input, 1);
    }
    Connection {
        id: ConnectionId::new(id).unwrap(),
        label: id.to_owned(),
        provider: provider.to_owned(),
        endpoint: endpoint.map(str::to_owned),
        locality: Locality::Local,
        auth: Auth::None,
        billing: Billing::Free,
        models: vec![ModelInfo::new(ModelId::new(model).unwrap(), model).with(spec)],
        enabled: true,
        key_header: None,
    }
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
                connections: vec![
                    connection(
                        "comfy",
                        "comfyui",
                        "sam3-node",
                        Some("http://127.0.0.1:8000"),
                    ),
                    connection("native", "candle", "sam3-official", None),
                ],
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
        .with_factory(without_engines())
    }
    fn choose(&self, connection: &str, model: &str) {
        ToolDesk::new(self.client("launcher"), self.shared.clone(), None)
            .choose(
                &role(),
                &ConnectionId::new(connection).unwrap(),
                &ModelId::new(model).unwrap(),
            )
            .unwrap();
    }
}

#[test]
fn every_app_reads_the_exact_selected_local_backend_and_model() {
    let w = World::new();
    w.choose("comfy", "sam3-node");
    for app in ["photocraft", "lightcraft"] {
        let route = w.client(app).local_route_for(&role()).unwrap().unwrap();
        assert_eq!(route.provider, "comfyui");
        assert_eq!(route.connection.as_str(), "comfy");
        assert_eq!(route.model.as_str(), "sam3-node");
        assert_eq!(route.endpoint.as_deref(), Some("http://127.0.0.1:8000"));
    }
    w.choose("native", "sam3-official");
    for app in ["photocraft", "lightcraft"] {
        let route = w.client(app).local_route_for(&role()).unwrap().unwrap();
        assert_eq!(route.provider, "candle");
        assert_eq!(route.connection.as_str(), "native");
        assert_eq!(route.model.as_str(), "sam3-official");
        assert_eq!(route.endpoint, None);
    }
    assert!(w.shared.load().unwrap().apps.is_empty());
}

#[test]
fn a_selected_native_model_without_an_adapter_is_not_reported_ready() {
    let w = World::new();
    w.choose("native", "sam3-official");
    for app in ["photocraft", "lightcraft"] {
        let client = w.client(app);
        match client.status(&role()) {
            RoleStatus::Unavailable { reason } => assert_eq!(
                reason,
                "This app can't run the chosen model yet. Choose another in Set up AI."
            ),
            RoleStatus::Ready(_) => panic!("{app} has no native adapter registered"),
        }
        let view = client.picker(&role(), &[]).unwrap();
        let native = view
            .groups
            .iter()
            .find(|g| g.models.iter().any(|m| m.connection.as_str() == "native"))
            .unwrap();
        assert_ne!(native.state, craft_ai_client::GroupState::Ready);
        assert_eq!(native.note, "This app can't use this option yet.");
        assert!(
            native.setup.is_none(),
            "No setup button can install an app adapter"
        );
    }
}

#[test]
fn an_endpoint_cannot_masquerade_as_a_different_selected_backend() {
    let w = World::new();
    w.choose("native", "sam3-official");
    let error = w.client("photocraft").endpoint_for(&role()).unwrap_err();
    assert_eq!(
        error.message,
        "The model you chose can't be run by this app yet. Choose another in Set up AI."
    );
}

#[test]
fn a_broken_choice_is_an_error_not_an_unbound_local_route() {
    let w = World::new();
    w.choose("comfy", "sam3-node");
    w.shared
        .update(|s| s.connections.retain(|c| c.id.as_str() != "comfy"))
        .unwrap();
    let error = w.client("photocraft").local_route_for(&role()).unwrap_err();
    assert!(error.message.contains("comfy"), "{error:?}");
    assert_eq!(
        World::new()
            .client("photocraft")
            .local_route_for(&role())
            .unwrap(),
        None
    );
}

#[test]
fn an_app_override_changes_only_that_apps_local_route() {
    let w = World::new();
    w.choose("comfy", "sam3-node");
    w.client("lightcraft")
        .bind(
            &role(),
            &ConnectionId::new("native").unwrap(),
            &ModelId::new("sam3-official").unwrap(),
        )
        .unwrap();
    assert_eq!(
        w.client("lightcraft")
            .local_route_for(&role())
            .unwrap()
            .unwrap()
            .provider,
        "candle"
    );
    assert_eq!(
        w.client("photocraft")
            .local_route_for(&role())
            .unwrap()
            .unwrap()
            .provider,
        "comfyui"
    );
}

#[test]
fn a_remote_choice_is_not_exposed_as_a_local_route() {
    let w = World::new();
    w.shared
        .update(|s| {
            let c = s
                .connections
                .iter_mut()
                .find(|c| c.id.as_str() == "comfy")
                .unwrap();
            c.locality = Locality::Remote;
            c.endpoint = Some("https://example.com".to_owned());
        })
        .unwrap();
    w.choose("comfy", "sam3-node");
    let error = w.client("photocraft").local_route_for(&role()).unwrap_err();
    assert_eq!(
        error.message,
        "The model you chose runs online, not on this computer."
    );
}

/// A client of an app that has not enabled any built-in engine, whatever cargo features are on.
fn without_engines() -> ProviderFactory {
    let mut factory = ProviderFactory::standard();
    factory.unregister("candle");
    factory.unregister("burn");
    factory
}

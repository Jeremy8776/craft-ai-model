use craft_ai_client::{AiClient, ToolDesk};
use craft_ai_core::{AppId, CapabilityId, InputRole, Role, RoleId, StoreFile};
use craft_ai_native::secrets::MemoryStore;
use craft_ai_native::shared::SharedStore;
use std::sync::Arc;

fn role(id: &str) -> Role {
    Role {
        id: RoleId::new(id).unwrap(),
        label: "Describe photograph".into(),
        capability: CapabilityId::parse("image.describe").unwrap(),
        inputs: vec![InputRole::Source],
    }
}

fn desk(shared: &SharedStore) -> ToolDesk {
    let client = AiClient::new(
        AppId::new("launcher").unwrap(),
        shared.clone(),
        Arc::new(MemoryStore::new()),
    );
    ToolDesk::new(client, shared.clone(), None)
}

#[test]
fn user_declared_tool_is_shared_without_changing_builtin_catalogue() {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let tool = role("describe-photo");
    desk(&shared).register_tool(tool.clone()).unwrap();
    let second = desk(&SharedStore::new(shared.path()));
    assert_eq!(second.custom_tools().unwrap(), vec![tool]);
    assert_eq!(craft_ai_features::builtin().len(), 8);
    assert_eq!(
        shared.load().unwrap().schema_version,
        StoreFile::default().schema_version
    );
}

#[test]
fn removing_a_custom_tool_clears_its_suite_and_app_choices_atomically() {
    use craft_ai_core::store::AppSettings;
    use craft_ai_core::{Binding, ConnectionId, ModelId};
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let tool = role("describe-photo");
    let launcher = desk(&shared);
    launcher.register_tool(tool.clone()).unwrap();
    let binding = Binding {
        role: tool.id.clone(),
        connection: ConnectionId::new("service").unwrap(),
        model: ModelId::new("model").unwrap(),
    };
    shared
        .update(|s| {
            s.defaults.push(binding.clone());
            s.apps.insert(
                AppId::new("wordcraft").unwrap(),
                AppSettings {
                    overrides: vec![binding],
                },
            );
        })
        .unwrap();
    launcher.remove_tool(&tool.id).unwrap();
    let after = shared.load().unwrap();
    assert!(after.custom_tools.is_empty());
    assert!(after.defaults.is_empty());
    assert!(
        after.apps[&AppId::new("wordcraft").unwrap()]
            .overrides
            .is_empty()
    );
    assert!(desk(&shared).custom_tools().unwrap().is_empty());
}

#[test]
fn built_in_or_duplicate_id_cannot_replace_a_tool() {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let launcher = desk(&shared);
    launcher.register_tool(role("describe-photo")).unwrap();
    assert!(launcher.register_tool(role("describe-photo")).is_err());
    assert!(launcher.register_tool(role("select-subject")).is_err());
    assert_eq!(
        shared.load().unwrap().custom_tools,
        vec![role("describe-photo")]
    );
    assert!(
        launcher
            .remove_tool(&RoleId::new("select-subject").unwrap())
            .is_err()
    );
    assert_eq!(shared.load().unwrap().custom_tools.len(), 1);
}

#[test]
fn malformed_custom_role_never_reaches_disk() {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let mut malformed = role("describe-photo");
    malformed.label = "bad\nname".into();
    assert!(desk(&shared).register_tool(malformed).is_err());
    assert!(!shared.path().exists());
}

#[test]
fn custom_tool_uses_one_bound_model_across_apps_and_rejects_missing_input() {
    use craft_ai_core::{CapabilitySpec, Connection, ConnectionId, ModelId, ModelInfo};
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let tool = role("describe-photo");
    desk(&shared).register_tool(tool.clone()).unwrap();
    let model = ModelInfo::new(ModelId::new("describe-v1").unwrap(), "Describe v1")
        .with(CapabilitySpec::new(tool.capability.clone()).accepting(InputRole::Source, 1));
    let connection: Connection = serde_json::from_value(serde_json::json!({
        "id":"service", "label":"Example", "provider":"example",
        "endpoint":"http://127.0.0.1:9", "locality":"local", "auth":{"kind":"none"},
        "billing":"free", "models":[model]
    }))
    .unwrap();
    shared.update(|s| s.connections.push(connection)).unwrap();
    desk(&shared)
        .choose(
            &tool,
            &ConnectionId::new("service").unwrap(),
            &ModelId::new("describe-v1").unwrap(),
        )
        .unwrap();
    let store = shared.load().unwrap();
    for app in ["photocraft", "lightcraft"] {
        let resolved = store.resolve(&AppId::new(app).unwrap(), &tool).unwrap();
        assert_eq!(resolved.model.id.as_str(), "describe-v1");
    }
    assert!(store.apps.is_empty());
    let mut needs_mask = tool;
    needs_mask.inputs.push(InputRole::Mask);
    assert!(
        store
            .resolve(&AppId::new("photocraft").unwrap(), &needs_mask)
            .is_err()
    );
}

#[test]
fn launcher_cannot_bind_a_redefined_shared_tool() {
    use craft_ai_core::{CapabilitySpec, Connection, ConnectionId, ModelId, ModelInfo};
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let tool = role("describe-photo");
    let launcher = desk(&shared);
    launcher.register_tool(tool.clone()).unwrap();
    let model = ModelInfo::new(ModelId::new("m").unwrap(), "M")
        .with(CapabilitySpec::new(tool.capability.clone()).accepting(InputRole::Source, 1));
    let conn: Connection = serde_json::from_value(serde_json::json!({
        "id":"service", "label":"Service", "provider":"example",
        "endpoint":"http://127.0.0.1:9", "locality":"local", "auth":{"kind":"none"},
        "billing":"free", "models":[model]
    }))
    .unwrap();
    shared.update(|s| s.connections.push(conn)).unwrap();
    let mut redefined = tool;
    redefined.inputs.clear();
    assert!(
        launcher
            .choose(
                &redefined,
                &ConnectionId::new("service").unwrap(),
                &ModelId::new("m").unwrap()
            )
            .is_err()
    );
    assert!(shared.load().unwrap().defaults.is_empty());
}

#[test]
fn an_app_cannot_offer_models_for_a_redefined_custom_tool() {
    use craft_ai_core::{CapabilitySpec, Connection, ModelId, ModelInfo};
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let tool = role("describe-photo");
    desk(&shared).register_tool(tool.clone()).unwrap();
    let model = ModelInfo::new(ModelId::new("m").unwrap(), "M")
        .with(CapabilitySpec::new(tool.capability.clone()).accepting(InputRole::Source, 1));
    let conn: Connection = serde_json::from_value(serde_json::json!({
        "id":"service", "label":"Service", "provider":"example",
        "endpoint":"http://127.0.0.1:9", "locality":"local", "auth":{"kind":"none"},
        "billing":"free", "models":[model]
    }))
    .unwrap();
    shared.update(|s| s.connections.push(conn)).unwrap();
    let app = AiClient::new(
        AppId::new("photocraft").unwrap(),
        shared.clone(),
        Arc::new(MemoryStore::new()),
    );
    let mut redefined = tool;
    redefined.inputs.clear();
    assert!(app.choices(&redefined).is_err());
    assert!(app.picker(&redefined, &[]).is_err());
    assert!(
        app.bind(
            &redefined,
            &craft_ai_core::ConnectionId::new("service").unwrap(),
            &ModelId::new("m").unwrap()
        )
        .is_err()
    );
    assert!(shared.load().unwrap().apps.is_empty());
}

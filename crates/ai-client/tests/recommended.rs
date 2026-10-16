//! Saying yes to "use the recommended models for N features?": the launcher's defaults are set for
//! the features offered, and only those, and a changed mind or a missing model never overrides or
//! half-writes anything.

use std::sync::Arc;

use craft_ai_client::{AiClient, Choice, ToolDesk};
use craft_ai_core::picks::CatalogEntry;
use craft_ai_core::{
    Binding, CapabilityId, Catalog, Connection, ConnectionId, InputRole, ModelId, Role, RoleId,
    SecretRef, SecretString, StoreFile,
};
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use serde_json::json;

fn role(id: &str) -> Role {
    Role {
        id: RoleId::new(id).unwrap(),
        label: id.into(),
        capability: CapabilityId::parse("text.complete").unwrap(),
        inputs: vec![InputRole::Prompt],
    }
}

fn hosted(models: &[&str]) -> Connection {
    let models: Vec<_> = models
        .iter()
        .map(|m| craft_ai_openai::catalog::classify(m).unwrap())
        .collect();
    serde_json::from_value(json!({
        "id": "or", "label": "Connection or", "provider": "openrouter",
        "endpoint": "https://api.example.com", "locality": "remote",
        "auth": {"kind": "api_key", "secret": "or-key"}, "billing": "metered",
        "models": models,
    }))
    .unwrap()
}

struct World {
    _dir: tempfile::TempDir,
    shared: SharedStore,
    desk: ToolDesk,
    client: AiClient,
}

fn world(models: &[&str], defaults: Vec<Binding>) -> World {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    shared
        .replace(&StoreFile {
            connections: vec![hosted(models)],
            defaults,
            ..StoreFile::default()
        })
        .unwrap();
    let secrets = MemoryStore::new();
    secrets
        .set(&SecretRef::new("or-key").unwrap(), &SecretString::new("k"))
        .unwrap();
    let catalog = Catalog {
        version: 1,
        entries: vec![CatalogEntry {
            capability: "text.complete".into(),
            pick: "recommended".into(),
            provider: "openrouter".into(),
            model: "gpt-4o-mini".into(),
        }],
    };
    let secrets = Arc::new(secrets);
    let make = || {
        AiClient::new(ToolDesk::app().unwrap(), shared.clone(), secrets.clone())
            .with_catalog(catalog.clone())
    };
    let client = make();
    let desk = ToolDesk::new(make(), shared.clone(), None);
    World {
        _dir: dir,
        shared,
        desk,
        client,
    }
}

fn defaults_of(w: &World) -> Vec<(String, String)> {
    let mut v: Vec<_> = w
        .shared
        .load()
        .unwrap()
        .defaults
        .iter()
        .map(|b| (b.role.to_string(), b.model.to_string()))
        .collect();
    v.sort();
    v
}

fn offer(w: &World, roles: &[Role]) -> Vec<(RoleId, Choice)> {
    w.client.recommended(roles).unwrap()
}

#[test]
fn yes_sets_the_launcher_default_for_each_feature_offered_and_nothing_else() {
    let w = world(&["gpt-4o-mini", "gpt-4o"], Vec::new());
    let roles = [role("rewrite"), role("summarise")];
    let offered = offer(&w, &roles);
    assert_eq!(offered.len(), 2);
    assert!(defaults_of(&w).is_empty(), "offering binds nothing");

    let done = w.desk.accept_recommended(&roles, &offered).unwrap();
    assert_eq!(done.bound.len(), 2);
    assert!(done.skipped.is_empty());
    assert_eq!(
        defaults_of(&w),
        [
            ("rewrite".into(), "gpt-4o-mini".into()),
            ("summarise".into(), "gpt-4o-mini".into())
        ]
    );
    let store = w.shared.load().unwrap();
    assert!(
        store.apps.values().all(|a| a.overrides.is_empty()),
        "defaults, not one app's override"
    );
}

#[test]
fn a_feature_given_a_model_since_the_offer_keeps_the_persons_choice() {
    let w = world(&["gpt-4o-mini", "gpt-4o"], Vec::new());
    let roles = [role("rewrite"), role("summarise")];
    let offered = offer(&w, &roles);
    // While the question was on screen the person picked their own model for rewrite.
    w.desk
        .choose(
            &roles[0],
            &ConnectionId::new("or").unwrap(),
            &ModelId::new("gpt-4o").unwrap(),
        )
        .unwrap();

    let done = w.desk.accept_recommended(&roles, &offered).unwrap();
    assert_eq!(done.bound, [RoleId::new("summarise").unwrap()]);
    assert_eq!(done.skipped.len(), 1);
    assert_eq!(done.skipped[0].tool.as_str(), "rewrite");
    assert_eq!(
        defaults_of(&w),
        [
            ("rewrite".into(), "gpt-4o".into()),
            ("summarise".into(), "gpt-4o-mini".into())
        ]
    );
}

#[test]
fn a_model_that_went_away_is_skipped_with_a_reason_and_the_rest_still_bind() {
    let w = world(&["gpt-4o-mini"], Vec::new());
    let roles = [role("rewrite"), role("summarise")];
    let mut offered = offer(&w, &roles);
    // The service stopped offering the model for one feature after the offer was made.
    offered[0].1.model = ModelId::new("gone-model").unwrap();

    let done = w.desk.accept_recommended(&roles, &offered).unwrap();
    assert_eq!(done.bound.len(), 1);
    assert_eq!(done.skipped.len(), 1);
    assert!(!done.skipped[0].why.is_empty());
    assert_eq!(defaults_of(&w).len(), 1, "nothing half written");
}

#[test]
fn an_offer_for_a_feature_the_app_no_longer_has_is_skipped() {
    let w = world(&["gpt-4o-mini"], Vec::new());
    let offered = offer(&w, &[role("rewrite")]);
    let done = w
        .desk
        .accept_recommended(&[role("other")], &offered)
        .unwrap();
    assert!(done.bound.is_empty());
    assert_eq!(done.skipped.len(), 1);
    assert!(defaults_of(&w).is_empty());
}

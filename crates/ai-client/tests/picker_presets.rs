//! The presets at the top of the picker: which model earns which, and the line that says why.
//! A temporary store and no servers. Prices and dates are what a provider reported; a curated pick
//! is what the catalog names; nothing is invented, and a model that cannot be used is not offered.

use std::sync::Arc;

use craft_ai_client::{AiClient, GroupState, PickerView};
use craft_ai_core::picks::CatalogEntry;
use craft_ai_core::{
    AppId, CapabilityId, CapabilitySpec, Catalog, Connection, InputRole, ModelId, ModelInfo, Pick,
    Pricing, Role, SecretRef, SecretString, StoreFile,
};
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use serde_json::json;

fn fill() -> Role {
    craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == "generative-fill")
        .unwrap()
        .role()
        .unwrap()
}

/// An inpainting model with an optional price per image (millionths of a dollar) and release time.
fn painter(id: &str, price: Option<u64>, released: Option<u64>) -> ModelInfo {
    let spec = CapabilitySpec::new(CapabilityId::parse("image.inpaint").unwrap())
        .accepting(InputRole::Source, 1)
        .accepting(InputRole::Mask, 1)
        .accepting(InputRole::Prompt, 1);
    let mut m = ModelInfo::new(ModelId::new(id).unwrap(), id).with(spec);
    m.pricing = price.map(|p| Pricing {
        per_image_micros: Some(p),
        ..Pricing::default()
    });
    m.released = released;
    m
}

fn connection(id: &str, enabled: bool, models: Vec<ModelInfo>) -> Connection {
    serde_json::from_value(json!({
        "id": id, "label": format!("Label {id}"), "provider": "openai",
        "endpoint": "https://api.example.com/v1", "locality": "remote",
        "auth": {"kind": "api_key", "secret": format!("{id}-key")}, "billing": "metered",
        "enabled": enabled, "models": models,
    }))
    .unwrap()
}

fn catalog(entries: &[(&str, &str)]) -> Catalog {
    Catalog {
        version: 1,
        entries: entries
            .iter()
            .map(|(pick, model)| CatalogEntry {
                capability: "image.inpaint".into(),
                pick: (*pick).into(),
                provider: "openai".into(),
                model: (*model).into(),
            })
            .collect(),
    }
}

fn view(connections: Vec<Connection>, catalog: Catalog) -> PickerView {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let secrets = Arc::new(MemoryStore::new());
    for c in &connections {
        secrets
            .set(
                &SecretRef::new(format!("{}-key", c.id)).unwrap(),
                &SecretString::new("sekrit-value-1234"),
            )
            .unwrap();
    }
    shared
        .replace(&StoreFile {
            connections,
            ..StoreFile::default()
        })
        .unwrap();
    let options = craft_ai_features::setup_options(&craft_ai_features::builtin());
    AiClient::new(AppId::new("photocraft").unwrap(), shared, secrets)
        .with_catalog(catalog)
        .picker(&fill(), &options)
        .unwrap()
}

fn earned(v: &PickerView) -> Vec<(Pick, String)> {
    v.presets
        .iter()
        .map(|p| (p.pick, p.model.model.to_string()))
        .collect()
}

#[test]
fn no_catalog_and_no_provider_data_means_no_presets() {
    let v = view(
        vec![connection("a", true, vec![painter("one", None, None)])],
        Catalog::default(),
    );
    assert!(v.presets.is_empty());
    assert_eq!(v.groups[0].models.len(), 1, "the full list is still there");
}

#[test]
fn lowest_cost_and_newest_come_from_what_the_provider_reported() {
    let v = view(
        vec![connection(
            "a",
            true,
            vec![
                painter("dear", Some(80_000), Some(1_700_000_000)),
                painter("cheap", Some(40_000), Some(1_600_000_000)),
                painter("fresh", Some(60_000), Some(1_709_164_800)),
            ],
        )],
        Catalog::default(),
    );
    assert_eq!(
        earned(&v),
        [
            (Pick::LowestCost, "cheap".to_owned()),
            (Pick::Newest, "fresh".to_owned())
        ]
    );
    assert_eq!(
        v.presets[0].reason,
        "Lowest reported price: $0.04 per image."
    );
    assert_eq!(v.presets[1].reason, "Released 29 Feb 2024.");
}

#[test]
fn a_curated_pick_is_listed_with_the_catalog_as_its_reason() {
    let v = view(
        vec![connection(
            "a",
            true,
            vec![painter("one", None, None), painter("two", None, None)],
        )],
        catalog(&[("recommended", "two"), ("best_result", "one")]),
    );
    assert_eq!(
        earned(&v),
        [
            (Pick::Recommended, "two".to_owned()),
            (Pick::BestResult, "one".to_owned())
        ]
    );
    assert_eq!(
        v.presets[0].reason,
        "Our pick: a balance of quality, speed and price."
    );
}

#[test]
fn a_curated_pick_the_connection_does_not_offer_is_skipped() {
    let v = view(
        vec![connection("a", true, vec![painter("one", None, None)])],
        catalog(&[("recommended", "gone")]),
    );
    assert!(v.presets.is_empty());
}

#[test]
fn a_model_on_a_provider_that_cannot_run_is_not_offered_as_a_preset() {
    let v = view(
        vec![
            connection("off", false, vec![painter("cheap", Some(1_000), None)]),
            connection("on", true, vec![painter("dear", Some(90_000), None)]),
        ],
        Catalog::default(),
    );
    assert!(
        v.groups
            .iter()
            .any(|g| g.state == GroupState::SwitchedOff && !g.models.is_empty())
    );
    assert_eq!(earned(&v), [(Pick::LowestCost, "dear".to_owned())]);
}

#[test]
fn a_preset_row_is_the_same_row_the_full_list_has() {
    let v = view(
        vec![connection(
            "a",
            true,
            vec![painter("cheap", Some(40_000), None)],
        )],
        Catalog::default(),
    );
    assert_eq!(v.presets[0].model, v.groups[0].models[0]);
}

//! Model presets through `AiClient`: what each connection's models earn, and that offering the
//! recommended models never binds anything.

use std::sync::Arc;

use craft_ai_client::AiClient;
use craft_ai_core::picks::CatalogEntry;
use craft_ai_core::{
    AppId, Billing, Binding, CapabilityId, Catalog, Connection, ConnectionId, InputRole, ModelId,
    ModelInfo, Pick, PickedBy, Pricing, Role, RoleId, SecretRef, SecretString, StoreFile,
};
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use serde_json::json;

fn role(id: &str, capability: &str) -> Role {
    Role {
        id: RoleId::new(id).unwrap(),
        label: id.into(),
        capability: CapabilityId::parse(capability).unwrap(),
        inputs: vec![InputRole::Prompt],
    }
}

fn rewrite() -> Role {
    role("rewrite", "text.complete")
}

/// A text model with a price (millionths of a dollar per million tokens, in and out) and a date.
fn text_model(id: &str, price: Option<(u64, u64)>, released: Option<u64>) -> ModelInfo {
    let mut m = craft_ai_openai::catalog::classify(id).unwrap();
    m.pricing = price.map(|(i, o)| Pricing {
        input_micros_per_mtok: Some(i),
        output_micros_per_mtok: Some(o),
        per_image_micros: None,
    });
    m.released = released;
    m
}

fn connection(id: &str, provider: &str, free: bool, models: &[ModelInfo]) -> Connection {
    serde_json::from_value(json!({
        "id": id, "label": format!("Connection {id}"), "provider": provider,
        "endpoint": if free { "http://127.0.0.1:9" } else { "https://api.example.com" },
        "locality": if free { "local" } else { "remote" },
        "auth": if free { json!({"kind": "none"}) } else { json!({"kind": "api_key", "secret": format!("{id}-key")}) },
        "billing": if free { "free" } else { "metered" },
        "models": models,
    }))
    .unwrap()
}

fn client(
    connections: Vec<Connection>,
    defaults: Vec<Binding>,
    catalog: Catalog,
) -> (tempfile::TempDir, AiClient) {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    shared
        .replace(&StoreFile {
            connections,
            defaults,
            ..StoreFile::default()
        })
        .unwrap();
    // A saved key for every hosted connection, so a model bound to one counts as ready.
    let secrets = MemoryStore::new();
    for id in ["or", "srv"] {
        secrets
            .set(
                &SecretRef::new(format!("{id}-key")).unwrap(),
                &SecretString::new("k"),
            )
            .unwrap();
    }
    let client = AiClient::new(AppId::new("wordcraft").unwrap(), shared, Arc::new(secrets))
        .with_catalog(catalog);
    (dir, client)
}

fn entry(pick: &str, provider: &str, model: &str) -> CatalogEntry {
    CatalogEntry {
        capability: "text.complete".into(),
        pick: pick.into(),
        provider: provider.into(),
        model: model.into(),
    }
}

#[test]
fn each_model_earns_the_presets_that_fit_it_and_the_reason_is_carried() {
    let hosted = connection(
        "or",
        "openrouter",
        false,
        &[
            text_model("gpt-4o", Some((2_500_000, 10_000_000)), Some(1_700_000_000)),
            text_model("gpt-4o-mini", Some((150_000, 600_000)), Some(1_721_000_000)),
        ],
    );
    let catalog = Catalog {
        version: 1,
        entries: vec![entry("recommended", "openrouter", "gpt-4o-mini")],
    };
    let (_dir, client) = client(vec![hosted], Vec::new(), catalog);
    let picks = client.picks(&rewrite()).unwrap();
    let by = |p: Pick| picks.iter().find(|x| x.pick == p);
    assert_eq!(
        by(Pick::Recommended).unwrap().choice.model.as_str(),
        "gpt-4o-mini"
    );
    assert_eq!(by(Pick::Recommended).unwrap().by, PickedBy::Catalog);
    assert_eq!(
        by(Pick::LowestCost).unwrap().choice.model.as_str(),
        "gpt-4o-mini"
    );
    assert_eq!(by(Pick::LowestCost).unwrap().by, PickedBy::Price(750_000));
    assert_eq!(
        by(Pick::Newest).unwrap().by,
        PickedBy::Released(1_721_000_000)
    );
    assert!(
        by(Pick::BestResult).is_none(),
        "nothing curated for it, so no pick"
    );
    assert_eq!(
        by(Pick::Recommended).unwrap().choice.connection_label,
        "Connection or"
    );
}

#[test]
fn a_free_local_model_beats_any_priced_one_for_lowest_cost() {
    let hosted = connection(
        "or",
        "openrouter",
        false,
        &[text_model("gpt-4o-mini", Some((1, 1)), None)],
    );
    let local = connection(
        "ollama",
        "ollama",
        true,
        &[text_model("llama3", None, None)],
    );
    let (_dir, client) = client(vec![hosted, local], Vec::new(), Catalog::default());
    let picks = client.picks(&rewrite()).unwrap();
    let cheapest = picks.iter().find(|p| p.pick == Pick::LowestCost).unwrap();
    assert_eq!(
        (cheapest.choice.model.as_str(), &cheapest.by),
        ("llama3", &PickedBy::Free)
    );
}

#[test]
fn models_that_report_nothing_earn_no_computed_presets() {
    let plain = connection("srv", "ollama", false, &[text_model("llama3", None, None)]);
    let (_dir, client) = client(vec![plain], Vec::new(), Catalog::default());
    assert!(client.picks(&rewrite()).unwrap().is_empty());
}

#[test]
fn no_connections_means_no_picks_not_an_error() {
    let (_dir, client) = client(Vec::new(), Vec::new(), Catalog::default());
    assert!(client.picks(&rewrite()).unwrap().is_empty());
    assert!(client.recommended(&[rewrite()]).unwrap().is_empty());
}

#[test]
fn recommended_offers_a_model_only_for_features_that_cannot_run_yet_and_binds_nothing() {
    let hosted = connection(
        "or",
        "openrouter",
        false,
        &[
            text_model("gpt-4o-mini", None, None),
            text_model("gpt-4o", None, None),
        ],
    );
    let catalog = Catalog {
        version: 1,
        entries: vec![entry("recommended", "openrouter", "gpt-4o-mini")],
    };
    let bound = Binding {
        role: RoleId::new("summarise").unwrap(),
        connection: ConnectionId::new("or").unwrap(),
        model: ModelId::new("gpt-4o").unwrap(),
    };
    let (_dir, client) = client(vec![hosted], vec![bound], catalog);
    let summarise = role("summarise", "text.complete");
    // The summarise feature already has a model; rewrite has none.
    let offers = client.recommended(&[rewrite(), summarise.clone()]).unwrap();
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0].0.as_str(), "rewrite");
    assert_eq!(offers[0].1.model.as_str(), "gpt-4o-mini");
    // Offering is not choosing: rewrite is still unbound.
    assert!(matches!(
        client.status(&rewrite()),
        craft_ai_client::RoleStatus::Unavailable { .. }
    ));
    // Agreeing is the app calling bind with the offered choice.
    client
        .bind(&rewrite(), &offers[0].1.connection, &offers[0].1.model)
        .unwrap();
    assert!(matches!(
        client.status(&rewrite()),
        craft_ai_client::RoleStatus::Ready(_)
    ));
}

#[test]
fn a_recommendation_for_a_model_the_connection_does_not_offer_is_skipped() {
    let hosted = connection(
        "or",
        "openrouter",
        false,
        &[text_model("gpt-4o", None, None)],
    );
    let catalog = Catalog {
        version: 1,
        entries: vec![entry(
            "recommended",
            "openrouter",
            "a-model-that-was-retired",
        )],
    };
    let (_dir, client) = client(vec![hosted], Vec::new(), catalog);
    assert!(client.recommended(&[rewrite()]).unwrap().is_empty());
}

#[test]
fn billing_free_connections_are_the_ones_that_count_as_free() {
    let c = connection("x", "p", true, &[]);
    assert_eq!(c.billing, Billing::Free);
}

#[test]
fn a_catalog_file_in_the_shared_folder_changes_the_picks_without_a_new_app() {
    let hosted = connection(
        "or",
        "openrouter",
        false,
        &[
            text_model("gpt-4o-mini", None, None),
            text_model("gpt-4o", None, None),
        ],
    );
    let (dir, client) = client(vec![hosted], Vec::new(), Catalog::default());
    let file = craft_ai_native::CatalogFile::new(dir.path().join("catalog.json"));
    let recommended = |c: &AiClient| {
        c.picks(&rewrite())
            .unwrap()
            .into_iter()
            .find(|p| p.pick == Pick::Recommended)
            .map(|p| p.choice.model.to_string())
    };
    assert_eq!(recommended(&client), None, "nothing curated yet");

    std::fs::write(
        file.path(),
        r#"{"version":2,"entries":[{"capability":"text.complete","pick":"recommended","provider":"openrouter","model":"gpt-4o"}]}"#,
    )
    .unwrap();
    let client = client.with_catalog_file(&file);
    assert_eq!(recommended(&client).as_deref(), Some("gpt-4o"));
}

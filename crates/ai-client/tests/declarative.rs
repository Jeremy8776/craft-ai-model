//! A declared custom tool runs through a service described only by a data file: no Rust adapter
//! written for it. The tool, the connection, the model choice and the spec file are all data.

#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use craft_ai_client::{AiClient, ProviderFactory, RoleStatus, ToolDesk};
use craft_ai_core::job::Event;
use craft_ai_core::spec::Spec;
use craft_ai_core::{
    AppId, CapabilityId, Connection, ConsentLedger, ErrorKind, InputRole, JobState, Payload,
    Request, Role, RoleId, SecretRef, SecretString,
};
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use serde_json::json;
use support::{Reply, Server};

fn tool() -> Role {
    Role {
        id: RoleId::new("summarise-note").unwrap(),
        label: "Summarise a note".into(),
        capability: CapabilityId::parse("text.summarize").unwrap(),
        inputs: vec![InputRole::Prompt],
    }
}

fn spec_json(base: &str) -> serde_json::Value {
    json!({
        "spec_version": 1, "id": "notes-service", "label": "Notes Service", "base": base,
        "auth": {"kind": "header", "name": "x-api-key"},
        "models": [{"id": "sum-1", "label": "Summary 1", "capability": "text.summarize",
                    "accepts": {"prompt": 1}, "requires": ["prompt"]}],
        "flow": {"kind": "request_response",
            "call": {"method": "POST", "path": "/summaries", "body": {"text": "{prompt}"}},
            "result": {"at": "/summary", "as": "text"}}
    })
}

struct World {
    dir: tempfile::TempDir,
    shared: SharedStore,
    secrets: Arc<MemoryStore>,
}

impl World {
    /// The tool, a connection to `base` using a spec file, and the launcher's choice.
    fn new(connection_base: &str, spec_base: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("specs")).unwrap();
        let text = spec_json(spec_base).to_string();
        std::fs::write(dir.path().join("specs/notes-service.json"), &text).unwrap();
        let spec = Spec::parse(&text).unwrap();
        let connection: Connection = serde_json::from_value(json!({
            "id": "notes-service", "label": "Notes Service", "provider": "declarative",
            "endpoint": connection_base,
            "locality": if connection_base.starts_with("https") { "remote" } else { "local" },
            "auth": {"kind": "api_key", "secret": "notes-key"},
            "billing": if connection_base.starts_with("https") { "metered" } else { "free" },
            "models": spec.model_infos(),
        }))
        .unwrap();
        let shared = SharedStore::new(dir.path().join("connections.json"));
        shared.update(|s| s.connections.push(connection)).unwrap();
        let secrets = Arc::new(MemoryStore::new());
        secrets
            .set(
                &SecretRef::new("notes-key").unwrap(),
                &SecretString::new("sk-notes-key"),
            )
            .unwrap();
        let world = Self {
            dir,
            shared,
            secrets,
        };
        let launcher = AiClient::new(
            AppId::new("launcher").unwrap(),
            world.shared.clone(),
            world.secrets.clone(),
        );
        let desk = ToolDesk::new(launcher, world.shared.clone(), None);
        desk.register_tool(tool()).unwrap();
        desk.choose(
            &tool(),
            &craft_ai_core::ConnectionId::new("notes-service").unwrap(),
            &craft_ai_core::ModelId::new("sum-1").unwrap(),
        )
        .unwrap();
        world
    }

    fn app(&self, id: &str) -> AiClient {
        let mut factory = ProviderFactory::standard();
        factory.set_declarative_specs(self.dir.path().join("specs"));
        AiClient::new(
            AppId::new(id).unwrap(),
            self.shared.clone(),
            self.secrets.clone(),
        )
        .with_factory(factory)
    }
}

fn note() -> Request {
    Request::new(tool().capability)
        .with_input(InputRole::Prompt, Payload::text("long meeting notes"))
}

#[test]
fn two_apps_run_a_tool_that_exists_only_as_data() {
    let server = Server::start(|r| {
        assert_eq!(r.header("x-api-key"), Some("sk-notes-key"));
        assert_eq!(r.body_json(), json!({"text": "long meeting notes"}));
        Reply::Json(200, r#"{"summary":"short"}"#.into())
    });
    let world = World::new(&server.url, &server.url);
    for app in ["photocraft", "wordcraft"] {
        let client = world.app(app);
        assert!(
            matches!(client.status(&tool()), RoleStatus::Ready(_)),
            "{app}"
        );
        let plan = client.plan(&tool(), note()).unwrap();
        let done = client
            .start(&plan, &ConsentLedger::default())
            .unwrap()
            .wait(Duration::from_millis(2), Duration::from_secs(10))
            .unwrap();
        assert_eq!(done.state, JobState::Succeeded, "{app}");
        assert!(
            done.events
                .iter()
                .any(|e| matches!(e, Event::Output(o) if o.payload == Payload::text("short")))
        );
    }
    assert_eq!(server.count(), 2);
}

#[test]
fn a_service_whose_address_differs_from_its_settings_file_is_refused_before_anything_is_sent() {
    let server = Server::start(|_| Reply::Json(200, r#"{"summary":"x"}"#.into()));
    let other = Server::start(|_| Reply::Json(200, r#"{"summary":"x"}"#.into()));
    // Consent and "on this computer" are decided from the connection's address, so the data file
    // must send to exactly that address.
    let world = World::new(&server.url, &other.url);
    let client = world.app("photocraft");
    let plan = client.plan(&tool(), note()).unwrap();
    let err = client.start(&plan, &ConsentLedger::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidRequest);
    assert!(err.message.contains("different address"), "{}", err.message);
    assert_eq!(server.count() + other.count(), 0);
}

#[test]
fn a_missing_or_damaged_settings_file_says_so_plainly() {
    let server = Server::start(|_| Reply::Json(200, r#"{"summary":"x"}"#.into()));
    let world = World::new(&server.url, &server.url);
    let file = world.dir.path().join("specs/notes-service.json");
    for contents in [None, Some("{ not json"), Some(r#"{"spec_version": 2}"#)] {
        match contents {
            None => std::fs::remove_file(&file).unwrap(),
            Some(text) => std::fs::write(&file, text).unwrap(),
        }
        let client = world.app("photocraft");
        let plan = client.plan(&tool(), note()).unwrap();
        let err = client.start(&plan, &ConsentLedger::default()).unwrap_err();
        assert!(
            err.message.contains("settings for Notes Service"),
            "{}",
            err.message
        );
        assert!(!err.message.contains("sk-notes-key"));
    }
    assert_eq!(server.count(), 0);
}

#[test]
fn a_remote_service_still_asks_before_its_settings_file_is_read() {
    let world = World::new(
        "https://notes.example.com/v1",
        "https://notes.example.com/v1",
    );
    std::fs::remove_file(world.dir.path().join("specs/notes-service.json")).unwrap();
    let client = world.app("photocraft");
    let plan = client.plan(&tool(), note()).unwrap();
    assert!(plan.needs_consent(&ConsentLedger::default()));
    let err = client.start(&plan, &ConsentLedger::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::ConsentRequired, "consent comes first");
}

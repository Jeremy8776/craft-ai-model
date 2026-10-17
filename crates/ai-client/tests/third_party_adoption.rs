//! The platform's acceptance test, written as the story of someone outside this repository.
//!
//! A vendor ("Acme") ships a service and a new kind of tool. Nobody edits this repository or any
//! app. The launcher records the tool and the connection once; two independent apps, each with its
//! own client, its own consent and its own idea of what the person has agreed to, then use it; a
//! third app that has no adapter for Acme is told so plainly. Everything goes through the public
//! API, which is what `docs/ADOPT-A-TOOL.md` walks through.
//!
//! Covers the gate in `docs/PLATFORM-CONTRACT.md`: a declared tool, a bound connection and a
//! registered adapter running one request across two independent clients, with an unavailable
//! adapter, consent, cancellation and a store file from before custom tools existed.

use std::sync::Arc;
use std::time::Duration;

use craft_ai_client::{AiClient, ProviderFactory, RoleStatus, ToolDesk};
use craft_ai_core::conformance::{run_cancel, run_lifecycle, run_rejects, run_unknown_job};
use craft_ai_core::mock::{MockProvider, Outcome, Script};
use craft_ai_core::{
    AppId, CapabilityId, CapabilitySpec, ConnectionId, ConsentLedger, ErrorKind, InputRole,
    JobState, ModelId, ModelInfo, Output, Payload, Provider, Request, Role, RoleId, SecretRef,
    SecretString,
};
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use serde_json::json;

/// The vendor's new tool: describe a photograph. The id is theirs to choose with the other
/// maintainers; the capability and inputs are what any model must offer to be picked for it.
fn describe_photo() -> Role {
    Role {
        id: RoleId::new("describe-photo").unwrap(),
        label: "Describe photograph".into(),
        capability: CapabilityId::parse("image.describe").unwrap(),
        inputs: vec![InputRole::Source],
    }
}

/// The vendor's model, as their service would list it.
fn acme_model() -> ModelInfo {
    ModelInfo::new(ModelId::new("acme-vision-1").unwrap(), "Acme Vision").with(
        CapabilitySpec::new(describe_photo().capability)
            .accepting(InputRole::Source, 1)
            .requiring(InputRole::Source),
    )
}

/// The vendor's adapter. A real one implements `Provider` against their service; the scripted
/// provider that every app tests with stands in here, so the test needs no network.
fn acme_adapter(script: Script) -> Arc<dyn Provider> {
    Arc::new(MockProvider::new(vec![acme_model()], script))
}

fn describing(text: &str) -> Script {
    Script {
        running_polls: 1,
        text_chunks: Vec::new(),
        outcome: Outcome::Succeed(vec![Output {
            payload: Payload::text(text),
            label: None,
        }]),
    }
}

/// An app's client. Every app builds its own; what they share is the store file and the keychain
/// (stood in for by one memory store), never an object.
fn client(
    app: &str,
    shared: &SharedStore,
    keychain: &Arc<MemoryStore>,
    with_acme: Option<Script>,
) -> AiClient {
    let mut factory = ProviderFactory::standard();
    if let Some(script) = with_acme {
        // The whole of adding an adapter to an app: a name and a function.
        factory.register("acme", move |_connection, _key| {
            Ok(acme_adapter(script.clone()))
        });
    }
    AiClient::new(AppId::new(app).unwrap(), shared.clone(), keychain.clone()).with_factory(factory)
}

fn request() -> Request {
    Request::new(describe_photo().capability).with_input(
        InputRole::Source,
        Payload::inline("image/png", vec![0x89, b'P', b'N', b'G']).unwrap(),
    )
}

/// The launcher's side, run once: the store is an older file with no custom tools in it; the tool
/// is declared, the connection saved with its key, and the model chosen for the tool.
fn launcher_sets_it_up(dir: &tempfile::TempDir) -> (SharedStore, Arc<MemoryStore>) {
    let path = dir.path().join("connections.json");
    // A file written before custom tools and per-app settings existed.
    std::fs::write(
        &path,
        r#"{"schema_version": 1, "connections": [], "defaults": []}"#,
    )
    .unwrap();
    let shared = SharedStore::new(&path);
    let keychain = Arc::new(MemoryStore::new());

    let connection = serde_json::from_value(json!({
        "id": "acme", "label": "Acme", "provider": "acme",
        "endpoint": "https://api.acme.example/v1",
        "locality": "remote", "auth": {"kind": "api_key", "secret": "acme-key"},
        "billing": "metered", "models": [serde_json::to_value(acme_model()).unwrap()],
    }))
    .unwrap();
    shared
        .update(|store| store.connections.push(connection))
        .unwrap();
    keychain
        .set(
            &SecretRef::new("acme-key").unwrap(),
            &SecretString::new("not-a-real-key"),
        )
        .unwrap();

    let desk = ToolDesk::new(
        client("launcher", &shared, &keychain, None),
        shared.clone(),
        None,
    );
    desk.register_tool(describe_photo()).unwrap();
    desk.choose(
        &describe_photo(),
        &ConnectionId::new("acme").unwrap(),
        &ModelId::new("acme-vision-1").unwrap(),
    )
    .unwrap();
    (shared, keychain)
}

fn finished_text(job: craft_ai_client::Job) -> (JobState, String) {
    let done = job
        .wait(Duration::from_millis(2), Duration::from_secs(10))
        .unwrap();
    let text = done
        .events
        .iter()
        .filter_map(|e| match e {
            craft_ai_core::Event::Output(o) => match &o.payload {
                Payload::Text { text } => Some(text.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    (done.state, text)
}

#[test]
fn two_independent_apps_use_a_vendors_tool_and_a_third_is_told_it_cannot() {
    let dir = tempfile::tempdir().unwrap();
    let (shared, keychain) = launcher_sets_it_up(&dir);
    let role = describe_photo();

    // The older file survived: its content is intact, the new pieces were added around it.
    let store = shared.load().unwrap();
    assert_eq!(store.custom_tools, vec![role.clone()]);
    assert_eq!(store.connections.len(), 1);

    let photocraft = client("photocraft", &shared, &keychain, Some(describing("a lake")));
    let lightcraft = client("lightcraft", &shared, &keychain, Some(describing("a lake")));

    // Neither app was configured: both read the same store and find the tool ready.
    for app in [&photocraft, &lightcraft] {
        assert!(matches!(app.status(&role), RoleStatus::Ready(_)));
    }

    // The service is online, so using it needs the person's agreement, asked per app.
    let mut ledger = ConsentLedger::default();
    let plan = photocraft.plan(&role, request()).unwrap();
    assert!(plan.needs_consent(&ledger));
    let refused = photocraft.start(&plan, &ledger).err().unwrap();
    assert_eq!(refused.kind, ErrorKind::ConsentRequired);
    ledger.grant(plan.disclosure());
    let (state, text) = finished_text(photocraft.start(&plan, &ledger).unwrap());
    assert_eq!((state, text.as_str()), (JobState::Succeeded, "a lake"));

    // Agreeing in one app is not agreeing in the other.
    let plan = lightcraft.plan(&role, request()).unwrap();
    assert!(plan.needs_consent(&ConsentLedger::default()));
    assert_eq!(
        lightcraft
            .start(&plan, &ConsentLedger::default())
            .err()
            .unwrap()
            .kind,
        ErrorKind::ConsentRequired
    );

    // A third app has no adapter for this service. It is not Ready, and it says why in words a
    // person can read, instead of crashing or quietly using something else.
    let bare = client("wordcraft", &shared, &keychain, None);
    match bare.status(&role) {
        RoleStatus::Unavailable { reason } => assert!(reason.contains("can't run"), "{reason}"),
        other => panic!("expected unavailable, got {other:?}"),
    }
    let error = bare.plan(&role, request()).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("Set up AI"), "{}", error.message);
}

#[test]
fn a_job_can_be_cancelled_from_the_app() {
    let dir = tempfile::tempdir().unwrap();
    let (shared, keychain) = launcher_sets_it_up(&dir);
    let slow = Script {
        running_polls: 10_000,
        ..describing("never")
    };
    let app = client("photocraft", &shared, &keychain, Some(slow));
    let plan = app.plan(&describe_photo(), request()).unwrap();
    let mut ledger = ConsentLedger::default();
    ledger.grant(plan.disclosure());
    let job = app.start(&plan, &ledger).unwrap();
    job.cancel().unwrap();
    let done = job
        .wait(Duration::from_millis(2), Duration::from_secs(10))
        .unwrap();
    assert_eq!(done.state, JobState::Cancelled);
}

#[test]
fn the_vendors_adapter_passes_the_shared_conformance_suite() {
    let adapter = acme_adapter(describing("a lake"));
    let model = acme_model().id;
    run_lifecycle(adapter.as_ref(), &model, &request()).unwrap();
    run_cancel(adapter.as_ref(), &model, &request()).unwrap();
    run_unknown_job(adapter.as_ref()).unwrap();
    // A request the model cannot take (no picture) is refused before anything is sent.
    let empty = Request::new(describe_photo().capability);
    run_rejects(adapter.as_ref(), &model, &empty, ErrorKind::InvalidRequest).unwrap();
}

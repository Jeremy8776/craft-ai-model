//! Adding a server that is not on the list. The connection manager probes it, says what it
//! confirmed and what it could not, and a custom connection only offers the abilities a person
//! declared. Scripted localhost servers only; nothing here reaches the internet or a real key.

#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use craft_ai_client::{
    AiClient, Confirmed, ConnectionManager, ModelChoice, RoleStatus, SaveError, Unconfirmed,
    declare_models,
};
use craft_ai_core::{
    AppId, Billing, Binding, CapabilityId, ConnectionDraft, ConnectionId, ConsentLedger, ErrorKind,
    InputRole, JobState, ModelId, Payload, Request, Role, RoleId, SecretRef, SecretString,
};
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use craft_ai_openai::catalog::Kind;
use serde_json::json;
use support::{Reply, Server};

const KEY: &str = "gw-key-7f3a";
const BASE_PATH: &str = "/gw/ai/v7";

struct Rig {
    manager: ConnectionManager,
    shared: SharedStore,
    secrets: Arc<MemoryStore>,
    _dir: tempfile::TempDir,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let secrets = Arc::new(MemoryStore::new());
    Rig {
        manager: ConnectionManager::new(shared.clone(), secrets.clone()),
        shared,
        secrets,
        _dir: dir,
    }
}

fn base(server: &Server) -> String {
    server.url.replace("/v1", BASE_PATH)
}

fn custom(server: &Server, header: &str) -> ConnectionDraft {
    let mut d = ConnectionDraft::custom();
    d.label = "Studio gateway".into();
    d.endpoint = base(server);
    d.key = KEY.into();
    d.key_header = header.into();
    d
}

fn list(ids: &[&str]) -> String {
    json!({"data": ids.iter().map(|i| json!({"id": i})).collect::<Vec<_>>()}).to_string()
}

/// A gateway that wants `x-api-key` and lists three models at an unusual base path.
fn gateway() -> Server {
    Server::start(|r| {
        if r.path != format!("{BASE_PATH}/models") {
            return Reply::Json(404, "{}".into());
        }
        match r.header("x-api-key") {
            Some(KEY) => Reply::Json(200, list(&["gpt-image-1", "llama3", "embed-1"])),
            _ => Reply::Json(401, json!({"error": {"message": "no key"}}).to_string()),
        }
    })
}

fn probe_of(r: &Rig, d: &ConnectionDraft) -> craft_ai_client::ProbeReport {
    r.manager
        .test_draft(d)
        .unwrap()
        .probe
        .expect("a custom connection is probed")
}

#[test]
fn a_probe_confirms_what_it_saw_and_names_what_it_could_not() {
    let (r, server) = (rig(), gateway());
    let report = r.manager.test_draft(&custom(&server, "x-api-key")).unwrap();
    // Nothing is assumed: the Test button hands back no models to adopt.
    assert!(report.models.is_empty());
    let probe = report.probe.unwrap();
    assert_eq!(
        probe.confirmed,
        [Confirmed::ModelList { count: 3 }, Confirmed::KeyAccepted]
    );
    assert!(probe.unconfirmed.contains(&Unconfirmed::Capabilities));
    assert!(probe.unconfirmed.contains(&Unconfirmed::Billing));
    let ids: Vec<_> = probe.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["embed-1", "gpt-image-1", "llama3"]);
    let kinds = |id: &str| {
        probe
            .models
            .iter()
            .find(|m| m.id == id)
            .unwrap()
            .suggested
            .clone()
    };
    assert_eq!(kinds("llama3"), [Kind::Chat]);
    assert_eq!(
        kinds("gpt-image-1"),
        [Kind::ImageGenerate, Kind::ImageEdit, Kind::ImageInpaint]
    );
    assert!(kinds("embed-1").is_empty());
    for line in probe
        .confirmed
        .iter()
        .map(Confirmed::text)
        .chain(probe.unconfirmed.iter().map(Unconfirmed::text))
    {
        assert!(!line.is_empty() && !line.contains(KEY), "{line}");
    }
}

#[test]
fn the_key_check_sends_the_key_once_and_the_control_request_none() {
    let (r, server) = (rig(), gateway());
    probe_of(&r, &custom(&server, "x-api-key"));
    let seen = server.recorded();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].header("x-api-key"), Some(KEY));
    assert_eq!(seen[0].header("authorization"), None);
    assert_eq!(seen[1].header("x-api-key"), None);
    assert_eq!(seen[1].header("authorization"), None);
    assert!(seen.iter().all(|s| s.path == format!("{BASE_PATH}/models")));
}

#[test]
fn a_server_that_answers_without_the_key_is_not_credited_with_checking_it() {
    let (r, server) = (rig(), Server::start(|_| Reply::Json(200, list(&["a"]))));
    let probe = probe_of(&r, &custom(&server, "x-api-key"));
    assert_eq!(probe.confirmed, [Confirmed::ModelList { count: 1 }]);
    assert!(probe.unconfirmed.contains(&Unconfirmed::KeyNotEnforced));
}

#[test]
fn a_control_request_that_fails_for_another_reason_leaves_the_key_unchecked() {
    let server = Server::start(|r| match r.header("authorization") {
        Some(_) => Reply::Json(200, list(&["a"])),
        None => Reply::Json(500, "{}".into()),
    });
    let r = rig();
    let probe = probe_of(&r, &custom(&server, ""));
    assert!(!probe.confirmed.contains(&Confirmed::KeyAccepted));
    assert!(probe.unconfirmed.contains(&Unconfirmed::KeyNotChecked));
    assert!(!probe.unconfirmed.contains(&Unconfirmed::KeyNotEnforced));
}

#[test]
fn a_rejected_key_is_an_auth_error_that_does_not_echo_it() {
    let r = rig();
    let server = Server::start(|r| {
        let echoed = r.header("x-api-key").unwrap_or_default().to_owned();
        Reply::Json(
            401,
            json!({"error": {"message": format!("bad {echoed}")}}).to_string(),
        )
    });
    let e = r
        .manager
        .test_draft(&custom(&server, "x-api-key"))
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Auth);
    assert!(!e.message.contains(KEY), "{}", e.message);
    assert_eq!(server.count(), 1, "no control request after a refusal");
}

#[test]
fn a_stated_billing_is_not_listed_as_unconfirmed() {
    let (r, server) = (rig(), gateway());
    let mut d = custom(&server, "x-api-key");
    d.billing = Billing::Free;
    let probe = probe_of(&r, &d);
    assert!(!probe.unconfirmed.contains(&Unconfirmed::Billing));
}

#[test]
fn a_cut_list_is_reported_as_incomplete() {
    let ids: Vec<String> = (0..1001).map(|i| format!("m{i:05}")).collect();
    let body = list(&ids.iter().map(String::as_str).collect::<Vec<_>>());
    let server = Server::start(move |_| Reply::Json(200, body.clone()));
    let r = rig();
    let probe = probe_of(&r, &custom(&server, ""));
    assert_eq!(probe.models.len(), 1000);
    assert!(probe.unconfirmed.contains(&Unconfirmed::ListCut));
}

#[test]
fn only_a_custom_connection_gets_the_probe() {
    let r = rig();
    let server = Server::start(|_| Reply::Json(200, list(&["llama3"])));
    let mut d = ConnectionDraft::from_preset(
        craft_ai_core::PRESETS
            .iter()
            .find(|p| p.key == "ollama")
            .unwrap(),
    );
    d.endpoint = server.url.clone();
    let report = r.manager.test_draft(&d).unwrap();
    assert!(report.probe.is_none());
    assert_eq!(report.models.len(), 1);
}

fn choose(ids: &[(&str, &[Kind])]) -> Vec<craft_ai_core::ModelInfo> {
    let choices: Vec<_> = ids
        .iter()
        .map(|(id, kinds)| ModelChoice {
            id: (*id).into(),
            kinds: kinds.to_vec(),
        })
        .collect();
    declare_models(&choices).unwrap()
}

#[test]
fn remembering_a_test_never_replaces_declared_models_with_guesses() {
    let (r, server) = (rig(), gateway());
    let mut d = custom(&server, "x-api-key");
    d.models = choose(&[("llama3", &[Kind::Chat])]);
    let id = r.manager.save(&d).unwrap().id;
    let report = r.manager.test_saved(&id, true).unwrap();
    assert!(report.probe.is_some());
    let saved = r.manager.views().unwrap().remove(0).connection;
    assert_eq!(saved.models.len(), 1);
    assert_eq!(saved.models[0].id.as_str(), "llama3");
    assert_eq!(saved.key_header.as_deref(), Some("x-api-key"));
}

#[test]
fn declared_models_are_checked_before_they_are_offered() {
    let bad = |choices: Vec<ModelChoice>| declare_models(&choices).unwrap_err();
    let chat = |id: &str| ModelChoice {
        id: id.into(),
        kinds: vec![Kind::Chat],
    };
    assert_eq!(
        bad(vec![chat("a"), chat("a")]).kind,
        ErrorKind::InvalidRequest
    );
    assert_eq!(
        bad(vec![ModelChoice {
            id: "a".into(),
            kinds: vec![]
        }])
        .kind,
        ErrorKind::InvalidRequest
    );
    assert_eq!(bad(vec![chat("has space")]).kind, ErrorKind::InvalidRequest);
    let many: Vec<_> = (0..201).map(|i| chat(&format!("m{i}"))).collect();
    assert_eq!(bad(many).kind, ErrorKind::InvalidRequest);
    let exactly: Vec<_> = (0..200).map(|i| chat(&format!("m{i}"))).collect();
    assert_eq!(declare_models(&exactly).unwrap().len(), 200);
    assert!(declare_models(&[]).unwrap().is_empty());
}

fn role() -> Role {
    Role {
        id: RoleId::new("rewrite").unwrap(),
        label: "Rewrite".into(),
        capability: CapabilityId::parse("text.complete").unwrap(),
        inputs: vec![InputRole::Prompt],
    }
}

fn bind(r: &Rig, id: &ConnectionId, model: &str) -> AiClient {
    r.shared
        .update(|f| {
            f.defaults.push(Binding {
                role: role().id,
                connection: id.clone(),
                model: ModelId::new(model).unwrap(),
            });
        })
        .unwrap();
    AiClient::new(client_app(), r.shared.clone(), r.secrets.clone())
}

fn client_app() -> AppId {
    AppId::new("wordcraft").unwrap()
}

#[derive(Default)]
struct Counting {
    inner: MemoryStore,
    gets: AtomicUsize,
}

impl SecretStore for Counting {
    fn name(&self) -> &'static str {
        "counting"
    }
    fn get(&self, k: &SecretRef) -> Result<Option<SecretString>, craft_ai_core::AiError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        self.inner.get(k)
    }
    fn set(&self, k: &SecretRef, v: &SecretString) -> Result<(), craft_ai_core::AiError> {
        self.inner.set(k, v)
    }
    fn delete(&self, k: &SecretRef) -> Result<(), craft_ai_core::AiError> {
        self.inner.delete(k)
    }
}

fn prompt() -> Request {
    Request::new(role().capability).with_input(InputRole::Prompt, Payload::text("hi"))
}

#[test]
fn a_declared_model_runs_with_the_named_header_from_the_saved_connection() {
    let r = rig();
    let server = Server::start(|req| {
        if req.path == format!("{BASE_PATH}/chat/completions")
            && req.header("x-api-key") == Some(KEY)
        {
            Reply::Json(
                200,
                json!({"choices":[{"message":{"content":"from the gateway"}}]}).to_string(),
            )
        } else {
            Reply::Json(401, "{}".into())
        }
    });
    let mut d = custom(&server, "x-api-key");
    d.models = choose(&[("llama3", &[Kind::Chat])]);
    let id = r.manager.save(&d).unwrap().id;
    let client = bind(&r, &id, "llama3");
    let RoleStatus::Ready(ready) = client.status(&role()) else {
        panic!("not ready")
    };
    assert_eq!(ready.billing, Billing::Unknown);
    assert_eq!(client.choices(&role()).unwrap().len(), 1);
    let plan = client.plan(&role(), prompt()).unwrap();
    let job = client.start(&plan, &ConsentLedger::default()).unwrap();
    let done = job
        .wait(Duration::from_millis(5), Duration::from_secs(10))
        .unwrap();
    assert_eq!(done.state, JobState::Succeeded);
}

#[test]
fn a_custom_connection_with_nothing_declared_offers_nothing_and_asks_no_one() {
    let (r, server) = (rig(), gateway());
    let id = r.manager.save(&custom(&server, "x-api-key")).unwrap().id;
    let client = bind(&r, &id, "llama3");
    assert!(client.choices(&role()).unwrap().is_empty());
    assert!(matches!(
        client.status(&role()),
        RoleStatus::Unavailable { .. }
    ));
    assert_eq!(server.count(), 0);
}

#[test]
fn a_remote_custom_connection_still_needs_agreement_before_anything_is_sent() {
    let r = rig();
    let mut d = ConnectionDraft::custom();
    d.label = "Unlisted".into();
    d.endpoint = "https://gw.unlisted.example/v1".into();
    d.key = KEY.into();
    d.models = choose(&[("llama3", &[Kind::Chat])]);
    let id = r.manager.save(&d).unwrap().id;
    bind(&r, &id, "llama3");
    // The app's client reads keys through a store that counts reads.
    let counting = Arc::new(Counting::default());
    let client = AiClient::new(client_app(), r.shared.clone(), counting.clone());
    let plan = client.plan(&role(), prompt()).unwrap();
    let ledger = ConsentLedger::default();
    assert!(plan.needs_consent(&ledger));
    let e = client.start(&plan, &ledger).unwrap_err();
    assert_eq!(e.kind, ErrorKind::ConsentRequired);
    assert_eq!(
        counting.gets.load(Ordering::SeqCst),
        0,
        "the key was read before consent"
    );
}

#[test]
fn a_header_name_is_refused_by_the_manager_and_nothing_is_saved() {
    let (r, server) = (rig(), gateway());
    let e = r.manager.save(&custom(&server, "host")).unwrap_err();
    assert!(matches!(e, SaveError::Invalid(f) if f.field == craft_ai_core::Field::KeyHeader));
    assert!(r.manager.views().unwrap().is_empty());
    assert!(
        r.secrets
            .get(&SecretRef::new("studio-gateway.key").unwrap())
            .unwrap()
            .is_none()
    );
}

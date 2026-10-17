//! The whole app flow end to end: the launcher writes connections and a default binding, the app
//! checks status, plans, gets consent, starts and reads the result. Local connections use the real
//! OpenAI-compatible adapter against a scripted localhost server; remote ones use the mock
//! provider, because the tests never reach the internet.

#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use craft_ai_client::{AiClient, ProviderFactory, RoleStatus};
use craft_ai_core::job::{Event, Output};
use craft_ai_core::mock::{MockProvider, Outcome, Script};
use craft_ai_core::{
    AppId, Binding, CapabilityId, Connection, ConnectionId, ConsentLedger, ErrorKind, InputRole,
    JobState, ModelId, Payload, Provider, Request, Role, RoleId, SecretRef, SecretString,
    StoreFile,
};
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use serde_json::json;
use support::{Reply, Server};

const KEY: &str = "sk-live-needle-777";

fn app() -> AppId {
    AppId::new("wordcraft").unwrap()
}

fn role() -> Role {
    Role {
        id: RoleId::new("rewrite").unwrap(),
        label: "Rewrite".into(),
        capability: CapabilityId::parse("text.complete").unwrap(),
        inputs: vec![InputRole::Prompt],
    }
}

fn prompt(text: &str) -> Request {
    Request::new(role().capability).with_input(InputRole::Prompt, Payload::text(text))
}

fn model_id(s: &str) -> ModelId {
    ModelId::new(s).unwrap()
}

fn conn_id(s: &str) -> ConnectionId {
    ConnectionId::new(s).unwrap()
}

fn connection(
    id: &str,
    provider: &str,
    endpoint: &str,
    remote: bool,
    models: &[&str],
) -> Connection {
    let models: Vec<_> = models
        .iter()
        .filter_map(|m| craft_ai_openai::catalog::classify(m))
        .collect();
    serde_json::from_value(json!({
        "id": id, "label": format!("Connection {id}"), "provider": provider,
        "endpoint": endpoint,
        "locality": if remote { "remote" } else { "local" },
        "auth": if remote { json!({"kind":"api_key","secret": format!("{id}-key")}) } else { json!({"kind":"none"}) },
        "billing": if remote { "metered" } else { "free" },
        "models": models,
    }))
    .unwrap()
}

fn binding(conn: &str, model: &str) -> Binding {
    Binding {
        role: role().id,
        connection: conn_id(conn),
        model: model_id(model),
    }
}

/// A memory store that counts reads, to prove a key is not fetched before consent.
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

struct World {
    _dir: tempfile::TempDir,
    shared: SharedStore,
    secrets: Arc<Counting>,
}

impl World {
    fn new(connections: Vec<Connection>, defaults: Vec<Binding>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let shared = SharedStore::new(dir.path().join("connections.json"));
        shared
            .replace(&StoreFile {
                connections,
                defaults,
                ..StoreFile::default()
            })
            .unwrap();
        Self {
            _dir: dir,
            shared,
            secrets: Arc::new(Counting::default()),
        }
    }

    fn client(&self) -> AiClient {
        AiClient::new(app(), self.shared.clone(), self.secrets.clone())
    }

    fn save_key(&self, conn: &str) {
        let r = SecretRef::new(format!("{conn}-key")).unwrap();
        self.secrets.set(&r, &SecretString::new(KEY)).unwrap();
    }
}

fn completion(text: &str) -> String {
    json!({"choices":[{"message":{"content":text}}]}).to_string()
}

fn run(client: &AiClient, request: Request, ledger: &ConsentLedger) -> craft_ai_client::Finished {
    let plan = client.plan(&role(), request).unwrap();
    let job = client.start(&plan, ledger).unwrap();
    job.wait(Duration::from_millis(5), Duration::from_secs(10))
        .unwrap()
}

fn all_text(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Event::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn no_launcher_and_no_connection_means_a_clear_unavailable_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let client = AiClient::new(
        app(),
        SharedStore::new(dir.path().join("none.json")),
        Arc::new(MemoryStore::new()),
    );
    match client.status(&role()) {
        RoleStatus::Unavailable { reason } => assert!(reason.contains("rewrite"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert!(client.choices(&role()).unwrap().is_empty());
    assert_eq!(
        client.plan(&role(), prompt("x")).unwrap_err().kind,
        ErrorKind::Unsupported
    );
}

#[test]
fn a_local_model_runs_end_to_end_through_the_real_adapter_with_no_consent() {
    let server = Server::start(|_| Reply::Json(200, completion("Hello from local")));
    let world = World::new(
        vec![connection(
            "local",
            "ollama",
            &server.url,
            false,
            &["llama3"],
        )],
        vec![binding("local", "llama3")],
    );
    let client = world.client();
    let RoleStatus::Ready(ready) = client.status(&role()) else {
        panic!("not ready")
    };
    assert_eq!(ready.model.as_str(), "llama3");
    let plan = client.plan(&role(), prompt("hi")).unwrap();
    assert!(!plan.needs_consent(&ConsentLedger::default()));
    let done = run(&client, prompt("hi"), &ConsentLedger::default());
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(all_text(&done.events), "Hello from local");
    assert_eq!(server.recorded()[0].body_json()["model"], "llama3");
}

#[test]
fn an_app_override_beats_the_launcher_default_and_clearing_it_restores_the_default() {
    let server = Server::start(|r| {
        Reply::Json(
            200,
            completion(r.body_json()["model"].as_str().unwrap_or("?")),
        )
    });
    let world = World::new(
        vec![connection(
            "local",
            "ollama",
            &server.url,
            false,
            &["llama3", "mistral"],
        )],
        vec![binding("local", "llama3")],
    );
    let client = world.client();
    let ledger = ConsentLedger::default();
    assert_eq!(
        all_text(&run(&client, prompt("x"), &ledger).events),
        "llama3"
    );

    client
        .bind(&role(), &conn_id("local"), &model_id("mistral"))
        .unwrap();
    assert_eq!(
        all_text(&run(&client, prompt("x"), &ledger).events),
        "mistral"
    );
    // Another app is unaffected by this app's override.
    let other = AiClient::new(
        AppId::new("gridcraft").unwrap(),
        world.shared.clone(),
        world.secrets.clone(),
    );
    let done = other.plan(&role(), prompt("x")).unwrap();
    assert_eq!(done.model().as_str(), "llama3");

    client.clear_binding(&role().id).unwrap();
    assert_eq!(
        all_text(&run(&client, prompt("x"), &ledger).events),
        "llama3"
    );
}

#[test]
fn binding_to_something_that_cannot_fill_the_role_is_refused_and_writes_nothing() {
    let server = Server::start(|_| Reply::Json(200, completion("x")));
    let world = World::new(
        vec![connection(
            "local",
            "ollama",
            &server.url,
            false,
            &["llama3", "gpt-image-1"],
        )],
        vec![binding("local", "llama3")],
    );
    let before = std::fs::read(world.shared.path()).unwrap();
    let client = world.client();
    // An image model cannot rewrite text; an unknown model and connection do not exist.
    assert!(
        client
            .bind(&role(), &conn_id("local"), &model_id("gpt-image-1"))
            .is_err()
    );
    assert!(
        client
            .bind(&role(), &conn_id("local"), &model_id("nope"))
            .is_err()
    );
    assert!(
        client
            .bind(&role(), &conn_id("ghost"), &model_id("llama3"))
            .is_err()
    );
    assert_eq!(std::fs::read(world.shared.path()).unwrap(), before);
    // The picker offers only models that can do the job.
    let choices = client.choices(&role()).unwrap();
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].model.as_str(), "llama3");
}

/// A factory whose "mock" provider records the key it was handed.
fn mock_factory(seen_key: Arc<Mutex<Option<String>>>, builds: Arc<AtomicUsize>) -> ProviderFactory {
    let mut f = ProviderFactory::standard();
    f.register("mock", move |conn, secret| {
        builds.fetch_add(1, Ordering::SeqCst);
        *seen_key.lock().unwrap() = secret.map(|s| s.expose().to_owned());
        let script = Script {
            running_polls: 1,
            text_chunks: vec!["ab".into(), "cd".into()],
            outcome: Outcome::Succeed(Vec::<Output>::new()),
        };
        Ok(Arc::new(MockProvider::new(conn.models.clone(), script)) as Arc<dyn Provider>)
    });
    f
}

fn remote_world() -> World {
    World::new(
        vec![connection(
            "cloud",
            "mock",
            "https://api.example.com/v1",
            true,
            &["gpt-4o"],
        )],
        vec![binding("cloud", "gpt-4o")],
    )
}

#[test]
fn remote_use_needs_consent_which_cannot_be_skipped_and_is_bound_to_the_host() {
    let world = remote_world();
    world.save_key("cloud");
    let seen = Arc::new(Mutex::new(None));
    let builds = Arc::new(AtomicUsize::new(0));
    let client = world
        .client()
        .with_factory(mock_factory(seen.clone(), builds.clone()));

    let plan = client.plan(&role(), prompt("secret draft")).unwrap();
    let mut ledger = ConsentLedger::default();
    assert!(plan.needs_consent(&ledger));
    // The disclosure names kinds, sizes and the destination, never the contents.
    let shown = format!("{:?}", plan.disclosure());
    assert!(shown.contains("api.example.com"));
    assert!(!shown.contains("secret draft"));

    // start() re-checks, so a forgotten prompt cannot send anything or even read the key.
    let err = client.start(&plan, &ledger).unwrap_err();
    assert_eq!(err.kind, ErrorKind::ConsentRequired);
    assert_eq!(builds.load(Ordering::SeqCst), 0);
    assert!(seen.lock().unwrap().is_none());
    assert_eq!(
        world.secrets.gets.load(Ordering::SeqCst),
        0,
        "the key was read before consent"
    );

    ledger.grant(plan.disclosure());
    let done = client
        .start(&plan, &ledger)
        .unwrap()
        .wait(Duration::from_millis(5), Duration::from_secs(10))
        .unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(all_text(&done.events), "abcd");
    assert_eq!(seen.lock().unwrap().as_deref(), Some(KEY));

    // Repointing the connection at another host asks again.
    world
        .shared
        .update(|s| s.connections[0].endpoint = Some("https://elsewhere.example.org/v1".into()))
        .unwrap();
    let moved = client.plan(&role(), prompt("secret draft")).unwrap();
    assert!(moved.needs_consent(&ledger));
}

#[test]
fn a_lapsed_agreement_stops_a_start_before_the_key_is_read() {
    let world = remote_world();
    world.save_key("cloud");
    let seen = Arc::new(Mutex::new(None));
    let builds = Arc::new(AtomicUsize::new(0));
    let client = world
        .client()
        .with_factory(mock_factory(seen.clone(), builds.clone()));
    let plan = client.plan(&role(), prompt("draft")).unwrap();

    // Agreed until one second after 1970: long gone.
    let mut ledger = ConsentLedger::default();
    ledger.grant_until(plan.disclosure(), 1);
    assert!(plan.needs_consent(&ledger));
    assert!(!plan.needs_consent_at(&ledger, 0), "valid before it lapses");
    let err = client.start(&plan, &ledger).unwrap_err();
    assert_eq!(err.kind, ErrorKind::ConsentRequired);
    assert_eq!(builds.load(Ordering::SeqCst), 0);
    assert_eq!(world.secrets.gets.load(Ordering::SeqCst), 0);

    // Agreed for another hour: runs.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    ledger.grant_until(plan.disclosure(), now + 3_600);
    assert!(!plan.needs_consent(&ledger));
    let done = client
        .start(&plan, &ledger)
        .unwrap()
        .wait(Duration::from_millis(5), Duration::from_secs(10))
        .unwrap();
    assert_eq!(done.state, JobState::Succeeded);
}

#[test]
fn a_missing_key_is_reported_by_connection_name_and_never_reaches_an_adapter() {
    let world = remote_world();
    let builds = Arc::new(AtomicUsize::new(0));
    let client = world
        .client()
        .with_factory(mock_factory(Arc::new(Mutex::new(None)), builds.clone()));
    match client.status(&role()) {
        RoleStatus::Unavailable { reason } => {
            assert!(
                reason.contains("Connection cloud") && reason.contains("Launcher"),
                "{reason}"
            );
        }
        other => panic!("{other:?}"),
    }
    let plan = client.plan(&role(), prompt("x")).unwrap();
    let mut ledger = ConsentLedger::default();
    ledger.grant(plan.disclosure());
    let err = client.start(&plan, &ledger).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Auth);
    assert_eq!(builds.load(Ordering::SeqCst), 0);
}

#[test]
fn a_bad_request_fails_at_plan_before_the_user_is_asked_anything() {
    let world = remote_world();
    world.save_key("cloud");
    let client = world.client();
    let empty = Request::new(role().capability);
    assert_eq!(
        client.plan(&role(), empty).unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
    // Well-formed in every other way, so only the capability check can refuse it.
    let wrong_capability = Request::new(CapabilityId::parse("image.generate").unwrap())
        .with_input(InputRole::Prompt, Payload::text("a fox"));
    let err = client.plan(&role(), wrong_capability).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidRequest);
    // The role's own check explains it in the app's terms, naming the feature.
    assert!(err.message.contains("Rewrite"), "{}", err.message);
}

#[test]
fn a_provider_this_app_has_no_adapter_for_is_a_clear_unsupported_error() {
    let world = World::new(
        vec![connection(
            "odd",
            "weirdai",
            "http://127.0.0.1:9/v1",
            false,
            &["llama3"],
        )],
        vec![binding("odd", "llama3")],
    );
    let client = world.client();
    // Refused at plan, so the person is never asked to agree to a send the app cannot make.
    let err = client.plan(&role(), prompt("x")).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert!(
        err.message.contains("can't run the chosen model"),
        "{}",
        err.message
    );
    assert!(
        !err.message.contains("weirdai"),
        "no internal names in what a person reads"
    );
}

#[test]
fn an_app_with_no_adapter_never_fetches_the_key() {
    let world = remote_world();
    world.save_key("cloud");
    let seen = Arc::new(Mutex::new(None));
    let builds = Arc::new(AtomicUsize::new(0));
    let capable = world
        .client()
        .with_factory(mock_factory(seen.clone(), builds.clone()));
    let plan = capable.plan(&role(), prompt("x")).unwrap();
    let mut ledger = ConsentLedger::default();
    ledger.grant(plan.disclosure());

    // Another app, with no adapter for this service, is handed that plan and agreement.
    let err = world.client().start(&plan, &ledger).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert_eq!(
        world.secrets.gets.load(Ordering::SeqCst),
        0,
        "no keychain read"
    );
    assert_eq!(builds.load(Ordering::SeqCst), 0);
    assert!(seen.lock().unwrap().is_none());
}

#[test]
fn nothing_prints_the_key_anywhere_in_the_flow() {
    let world = remote_world();
    world.save_key("cloud");
    let client = world.client().with_factory(mock_factory(
        Arc::new(Mutex::new(None)),
        Arc::new(AtomicUsize::new(0)),
    ));
    let plan = client.plan(&role(), prompt("x")).unwrap();
    let mut ledger = ConsentLedger::default();
    let mut texts = vec![
        format!("{client:?}"),
        format!("{plan:?}"),
        format!("{:?}", client.status(&role())),
        format!("{:?}", client.start(&plan, &ledger).unwrap_err()),
    ];
    ledger.grant(plan.disclosure());
    let job = client.start(&plan, &ledger).unwrap();
    texts.push(format!("{job:?}"));
    texts.push(format!(
        "{:?}",
        job.wait(Duration::from_millis(5), Duration::from_secs(10))
            .unwrap()
    ));
    texts.push(std::fs::read_to_string(world.shared.path()).unwrap());
    for t in texts {
        assert!(!t.contains("needle"), "{t}");
    }
}

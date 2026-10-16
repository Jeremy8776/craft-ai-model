//! The connection manager against real files, a real secret-store trait object and a scripted
//! localhost server for the Test button. Nothing here touches the OS keychain or the internet.

#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use std::sync::Arc;

use craft_ai_client::{ConnectionManager, KeyState, SaveError, TestRunner, TestTarget};
use craft_ai_core::store::AppSettings;
use craft_ai_core::{
    AiError, AppId, Binding, Connection, ConnectionDraft, ConnectionId, ErrorKind, Field, ModelId,
    PRESETS, RoleId, SecretRef,
};
use craft_ai_native::secrets::{EnvStore, MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use support::{Reply, Server};

const KEY: &str = "sk-live-key-12345";

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
    let manager = ConnectionManager::new(shared.clone(), secrets.clone());
    Rig {
        manager,
        shared,
        secrets,
        _dir: dir,
    }
}

fn preset(key: &str) -> ConnectionDraft {
    ConnectionDraft::from_preset(PRESETS.iter().find(|p| p.key == key).unwrap())
}

fn openai(key: &str) -> ConnectionDraft {
    let mut d = preset("openai");
    d.key = key.to_owned();
    d
}

fn id(s: &str) -> ConnectionId {
    ConnectionId::new(s).unwrap()
}

fn secret_ref(s: &str) -> SecretRef {
    SecretRef::new(s).unwrap()
}

fn stored_key(r: &Rig, s: &str) -> Option<String> {
    r.secrets
        .get(&secret_ref(s))
        .unwrap()
        .map(|k| k.expose().to_owned())
}

/// Where the saved connection `conn` keeps its key. Slots are named per save, so ask the file.
fn locator(r: &Rig, conn: &str) -> String {
    let store = r.shared.load().unwrap();
    let c = store.connections.iter().find(|c| c.id == id(conn)).unwrap();
    c.auth.secret_ref().unwrap().as_str().to_owned()
}

/// True when the secret store holds nothing at all.
fn store_is_empty(r: &Rig) -> bool {
    format!("{:?}", r.secrets) == "MemoryStore(0 secrets)"
}

fn file_text(r: &Rig) -> String {
    std::fs::read_to_string(r.shared.path()).unwrap_or_default()
}

fn models_reply() -> Reply {
    Reply::Json(
        200,
        r#"{"object":"list","data":[{"id":"gpt-4o","object":"model"},{"id":"gpt-image-1","object":"model"}]}"#
            .into(),
    )
}

/// A draft pointed at a scripted local server, so the test button has something to ask.
fn local_draft(url: &str) -> ConnectionDraft {
    let mut d = preset("ollama");
    d.endpoint = url.to_owned();
    d
}

#[test]
fn a_local_connection_needs_no_key_and_shows_as_such() {
    let r = rig();
    let saved = r.manager.save(&preset("ollama")).unwrap();
    assert_eq!(saved.id, id("ollama"));
    let views = r.manager.views().unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].key, KeyState::NotNeeded);
    assert_eq!(views[0].connection.locality, craft_ai_core::Locality::Local);
}

#[test]
fn a_key_goes_to_the_secret_store_and_never_into_the_file() {
    let r = rig();
    let saved = r.manager.save(&openai(KEY)).unwrap();
    assert_eq!(saved.id, id("openai"));
    let loc = locator(&r, "openai");
    assert!(loc.starts_with("openai.key"), "{loc}");
    assert_eq!(stored_key(&r, &loc).as_deref(), Some(KEY));
    let text = file_text(&r);
    assert!(text.contains(&loc), "the locator is in the file");
    assert!(!text.contains(KEY), "the key must not be");
    assert_eq!(r.manager.views().unwrap()[0].key, KeyState::Saved);
}

#[test]
fn a_connection_missing_its_key_is_flagged() {
    let r = rig();
    r.manager.save(&openai(KEY)).unwrap();
    r.secrets
        .delete(&secret_ref(&locator(&r, "openai")))
        .unwrap();
    assert_eq!(r.manager.views().unwrap()[0].key, KeyState::Missing);
}

#[test]
fn a_form_problem_is_reported_by_field_and_saves_nothing() {
    let r = rig();
    let mut d = openai("two words");
    let SaveError::Invalid(e) = r.manager.save(&d).unwrap_err() else {
        panic!("expected a form error");
    };
    assert_eq!(e.field, Field::Key);
    assert!(!e.message.contains("two words"));
    d.key = KEY.into();
    d.endpoint = "http://example.com/v1".into();
    assert!(matches!(
        r.manager.save(&d),
        Err(SaveError::Invalid(e)) if e.field == Field::Address
    ));
    assert!(r.manager.views().unwrap().is_empty());
    assert!(store_is_empty(&r), "no key was written for a refused form");
}

#[test]
fn renaming_keeps_the_id_and_key_and_a_new_key_replaces_it() {
    let r = rig();
    r.manager.save(&openai(KEY)).unwrap();
    let view = r.manager.views().unwrap().remove(0);
    let mut d = ConnectionDraft::from_connection(&view.connection);
    d.label = "Work".into();
    assert_eq!(r.manager.save(&d).unwrap().id, id("openai"));
    let first = locator(&r, "openai");
    assert_eq!(stored_key(&r, &first).as_deref(), Some(KEY));
    assert_eq!(r.manager.views().unwrap()[0].connection.label, "Work");
    d.key = "sk-replacement-9".into();
    r.manager.save(&d).unwrap();
    let second = locator(&r, "openai");
    assert!(stored_key(&r, &first).is_none(), "the replaced key is gone");
    assert_eq!(stored_key(&r, &second).as_deref(), Some("sk-replacement-9"));
    assert_eq!(
        r.manager.views().unwrap().len(),
        1,
        "an edit never duplicates"
    );
}

#[test]
fn turning_the_key_off_removes_it_from_the_secret_store() {
    let r = rig();
    r.manager.save(&openai(KEY)).unwrap();
    let view = r.manager.views().unwrap().remove(0);
    let mut d = ConnectionDraft::from_connection(&view.connection);
    d.uses_key = false;
    let loc = locator(&r, "openai");
    let saved = r.manager.save(&d).unwrap();
    assert!(saved.warning.is_none());
    assert!(stored_key(&r, &loc).is_none());
    assert_eq!(r.manager.views().unwrap()[0].key, KeyState::NotNeeded);
}

#[test]
fn same_names_get_separate_ids_and_separate_keys() {
    let r = rig();
    r.manager.save(&openai("key-one")).unwrap();
    let second = r.manager.save(&openai("key-two")).unwrap();
    assert_eq!(second.id, id("openai-2"));
    let (one, two) = (locator(&r, "openai"), locator(&r, "openai-2"));
    assert_ne!(one, two);
    assert_eq!(stored_key(&r, &one).as_deref(), Some("key-one"));
    assert_eq!(stored_key(&r, &two).as_deref(), Some("key-two"));
}

#[test]
fn a_keychain_that_refuses_gives_a_way_out_and_saves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let manager = ConnectionManager::new(shared, Arc::new(EnvStore::with_lookup(|_| None)));
    let SaveError::Failed(e) = manager.save(&openai(KEY)).unwrap_err() else {
        panic!("expected a storage failure");
    };
    assert!(
        e.message.contains("CRAFT_AI_SECRET_OPENAI_KEY"),
        "{}",
        e.message
    );
    assert!(!e.message.contains(KEY));
    assert!(manager.views().unwrap().is_empty());
}

fn seed_bindings(r: &Rig, connection: &str) {
    let b = |role: &str| Binding {
        role: RoleId::new(role).unwrap(),
        connection: id(connection),
        model: ModelId::new("m").unwrap(),
    };
    r.shared
        .update(|f| {
            f.defaults.push(b("rewrite"));
            f.apps.insert(
                AppId::new("wordcraft").unwrap(),
                AppSettings {
                    overrides: vec![b("summarise")],
                },
            );
        })
        .unwrap();
}

#[test]
fn deleting_removes_the_connection_its_choices_and_its_key() {
    let r = rig();
    r.manager.save(&openai(KEY)).unwrap();
    r.manager.save(&preset("ollama")).unwrap();
    seed_bindings(&r, "openai");
    let loc = locator(&r, "openai");
    assert_eq!(r.manager.views().unwrap()[0].bindings, 2);
    let removed = r.manager.delete(&id("openai")).unwrap();
    assert_eq!(removed.bindings_removed, 2);
    assert!(removed.key_removed);
    assert!(stored_key(&r, &loc).is_none());
    let store = r.shared.load().unwrap();
    assert_eq!(store.connections.len(), 1);
    assert!(store.defaults.is_empty());
    assert!(store.apps.values().all(|a| a.overrides.is_empty()));
    assert!(r.manager.delete(&id("openai")).is_err(), "already gone");
}

#[test]
fn deleting_keeps_a_key_another_connection_still_uses() {
    let r = rig();
    r.manager.save(&openai(KEY)).unwrap();
    let loc = locator(&r, "openai");
    // Two connections that (through an older file) share one locator.
    r.shared
        .update(|f| {
            let mut twin: Connection = f.connections[0].clone();
            twin.id = id("twin");
            f.connections.push(twin);
        })
        .unwrap();
    r.manager.delete(&id("openai")).unwrap();
    assert_eq!(stored_key(&r, &loc).as_deref(), Some(KEY));
    r.manager.delete(&id("twin")).unwrap();
    assert!(stored_key(&r, &loc).is_none());
}

#[test]
fn enabling_and_disabling_round_trips() {
    let r = rig();
    r.manager.save(&preset("ollama")).unwrap();
    r.manager.set_enabled(&id("ollama"), false).unwrap();
    assert!(!r.manager.views().unwrap()[0].connection.enabled);
    r.manager.set_enabled(&id("ollama"), true).unwrap();
    assert!(r.manager.views().unwrap()[0].connection.enabled);
    assert!(r.manager.set_enabled(&id("nope"), true).is_err());
}

#[test]
fn testing_a_draft_lists_models_sends_the_typed_key_and_saves_nothing() {
    let r = rig();
    let server = Server::start(|_| models_reply());
    let mut d = local_draft(&server.url);
    d.uses_key = true;
    d.key = KEY.into();
    let report = r.manager.test_draft(&d).unwrap();
    let names: Vec<_> = report.models.iter().map(|m| m.id.to_string()).collect();
    assert!(names.contains(&"gpt-4o".to_owned()), "{names:?}");
    let seen = server.recorded();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/v1/models");
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    assert!(
        r.manager.views().unwrap().is_empty(),
        "a test saves nothing"
    );
    assert!(store_is_empty(&r), "a test writes no key");
}

#[test]
fn testing_an_edited_draft_uses_the_saved_key() {
    let r = rig();
    let server = Server::start(|_| models_reply());
    let mut d = local_draft(&server.url);
    d.uses_key = true;
    d.key = KEY.into();
    r.manager.save(&d).unwrap();
    let view = r.manager.views().unwrap().remove(0);
    let edit = ConnectionDraft::from_connection(&view.connection);
    assert!(edit.key.is_empty());
    r.manager.test_draft(&edit).unwrap();
    let seen = server.recorded();
    assert_eq!(
        seen.last().unwrap().header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
}

#[test]
fn a_rejected_key_a_dead_server_and_a_bad_form_give_safe_messages() {
    let r = rig();
    let rejecting = Server::start(|_| {
        Reply::Json(
            401,
            r#"{"error":{"message":"Incorrect API key provided: sk-live-key-12345"}}"#.into(),
        )
    });
    let mut d = local_draft(&rejecting.url);
    d.uses_key = true;
    d.key = KEY.into();
    let e = r.manager.test_draft(&d).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Auth);
    assert!(
        !format!("{e} {e:?}").contains(KEY),
        "the server echoed the key; we must not"
    );

    let dead = local_draft(&Server::dead_url());
    let e = r.manager.test_draft(&dead).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Network);

    let mut bad = local_draft("nonsense");
    bad.key = "x".into();
    assert_eq!(
        r.manager.test_draft(&bad).unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
}

#[test]
fn testing_a_saved_connection_asks_the_server_even_with_a_saved_model_list() {
    let r = rig();
    let server = Server::start(|_| models_reply());
    r.manager.save(&local_draft(&server.url)).unwrap();
    r.shared
        .update(|f| {
            f.connections[0].models = vec![craft_ai_core::ModelInfo::new(
                ModelId::new("stale-model").unwrap(),
                "Stale",
            )];
        })
        .unwrap();
    let report = r.manager.test_saved(&id("ollama"), false).unwrap();
    assert_eq!(
        server.count(),
        1,
        "answered by the server, not the saved list"
    );
    assert!(report.models.iter().any(|m| m.id.to_string() == "gpt-4o"));
    // Without `remember` the saved list is untouched; with it, it is replaced.
    assert_eq!(
        r.manager.views().unwrap()[0].connection.models[0]
            .id
            .to_string(),
        "stale-model"
    );
    r.manager.test_saved(&id("ollama"), true).unwrap();
    let models = r.manager.views().unwrap().remove(0).connection.models;
    assert!(models.iter().all(|m| m.id.to_string() != "stale-model"));
    assert!(models.iter().any(|m| m.id.to_string() == "gpt-image-1"));
}

#[test]
fn testing_without_a_saved_key_says_where_to_add_it() {
    let r = rig();
    let server = Server::start(|_| models_reply());
    let mut d = local_draft(&server.url);
    d.uses_key = true;
    d.key = KEY.into();
    r.manager.save(&d).unwrap();
    r.secrets
        .delete(&secret_ref(&locator(&r, "ollama")))
        .unwrap();
    let e = r.manager.test_saved(&id("ollama"), false).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Auth);
    assert_eq!(server.count(), 0, "nothing is sent without a key");
}

#[test]
fn a_damaged_or_newer_file_is_an_error_not_a_crash_and_is_left_alone() {
    let r = rig();
    std::fs::write(r.shared.path(), b"{ not json").unwrap();
    assert!(r.manager.views().is_err());
    assert!(r.manager.save(&preset("ollama")).is_err());
    assert_eq!(std::fs::read(r.shared.path()).unwrap(), b"{ not json");
    std::fs::write(r.shared.path(), br#"{"schema_version": 999}"#).unwrap();
    assert!(r.manager.views().is_err());
    assert!(r.manager.save(&preset("ollama")).is_err());
    assert_eq!(
        std::fs::read(r.shared.path()).unwrap(),
        br#"{"schema_version": 999}"#
    );
}

fn wait_for(runner: &mut TestRunner) -> Option<Result<craft_ai_client::TestReport, AiError>> {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < end {
        if let Some(a) = runner.poll() {
            return Some(a);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    None
}

#[test]
fn a_background_test_delivers_its_answer_once() {
    let r = rig();
    let server = Server::start(|_| models_reply());
    let mut runner = TestRunner::new(r.manager.clone());
    assert!(runner.poll().is_none(), "nothing started, nothing to take");
    runner.start(TestTarget::Draft(local_draft(&server.url)));
    let report = wait_for(&mut runner).expect("an answer").unwrap();
    assert_eq!(report.models.len(), 2);
    assert!(runner.poll().is_none(), "an answer is taken once");
}

#[test]
fn only_the_latest_background_test_is_answered() {
    let r = rig();
    let server = Server::start(|_| models_reply());
    let mut runner = TestRunner::new(r.manager.clone());
    // The first fails at once (the address is nonsense); the second is the one wanted.
    runner.start(TestTarget::Draft(local_draft("nonsense")));
    runner.start(TestTarget::Draft(local_draft(&server.url)));
    let answer = wait_for(&mut runner).expect("an answer");
    assert!(
        answer.is_ok(),
        "the answer is the second test's, not the first's error"
    );
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        runner.poll().is_none(),
        "the first test's late answer never shows up"
    );
}

#[test]
fn a_background_test_of_a_saved_connection_remembers_the_models() {
    let r = rig();
    let server = Server::start(|_| models_reply());
    r.manager.save(&local_draft(&server.url)).unwrap();
    let mut runner = TestRunner::new(r.manager.clone());
    runner.start(TestTarget::Saved(id("ollama")));
    wait_for(&mut runner).unwrap().unwrap();
    assert_eq!(r.manager.views().unwrap()[0].connection.models.len(), 2);
}

#[test]
fn new_provider_presets_store_keys_separately_and_probe_where_safe() {
    let r = rig();
    let bfl_server = Server::start(|req| {
        assert_eq!(req.path, "/v1/credits");
        assert_eq!(req.header("x-key"), Some("bfl-test"));
        Reply::Json(200, r#"{"credits":1}"#.into())
    });
    let ark_server = Server::start(|req| {
        assert_eq!(
            req.path,
            "/v3/contents/generations/tasks?page_num=1&page_size=1"
        );
        assert_eq!(req.header("authorization"), Some("Bearer ark-test"));
        Reply::Json(200, "{}".into())
    });
    let mut bfl = preset("bfl");
    bfl.endpoint = bfl_server.url.clone();
    bfl.key = "bfl-test".into();
    let mut ark = preset("seedance");
    ark.endpoint = ark_server.url.replace("/v1", "/v3");
    ark.key = "ark-test".into();
    r.manager.save(&bfl).unwrap();
    r.manager.save(&ark).unwrap();
    assert_eq!(
        r.manager
            .test_saved(&id("black-forest-labs"), true)
            .unwrap()
            .models
            .len(),
        2
    );
    assert_eq!(
        r.manager
            .test_saved(&id("seedance-byteplus-modelark"), true)
            .unwrap()
            .models
            .len(),
        1
    );
    let text = file_text(&r);
    assert!(!text.contains("bfl-test") && !text.contains("ark-test"));
}

#[test]
fn vertex_express_key_is_saved_without_claiming_a_free_probe() {
    let r = rig();
    let mut d = preset("vertex-express");
    d.key = "vertex-test".into();
    let saved = r.manager.save(&d).unwrap();
    let conn = &r.manager.views().unwrap()[0].connection;
    assert_eq!(conn.models[0].id.as_str(), "gemini-2.5-flash-image");
    assert!(conn.models[0].label.contains("unverified"));
    assert_eq!(
        r.manager.test_saved(&saved.id, false).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert!(!file_text(&r).contains("vertex-test"));
}

#[test]
fn sam3_is_not_a_generic_url_preset_it_is_a_local_recipe_through_comfyui() {
    // The URL preset was removed on purpose: SAM 3 runs through ComfyUI, and the Features screen
    // sets it up. A preset here would advertise a connection that cannot do the job.
    assert!(PRESETS.iter().all(|p| p.key != "sam3"));
}

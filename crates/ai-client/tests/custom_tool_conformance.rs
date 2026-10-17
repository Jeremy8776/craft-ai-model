//! Conformance for one declared custom tool, across two independent app clients that share only
//! the connections file. No app interface, no network: the adapter is a scripted stand-in.
//!
//! What a maintainer's adapter for a declared tool has to keep true:
//! - the launcher's one choice is read by every app, and each app runs it through its own adapter;
//! - an app that does not ship the adapter says so plainly and sends nothing;
//! - a remote service asks first, and neither the key nor the adapter is touched before consent;
//! - a running job can be cancelled without disturbing another app's job;
//! - a connections file from before custom tools existed keeps working when a tool is declared.

#[path = "support/describe_tool.rs"]
mod describe_tool;

use std::sync::Arc;
use std::time::Duration;

use craft_ai_client::{AiClient, Finished, ProviderFactory, RoleStatus, ToolDesk};
use craft_ai_core::job::Event;
use craft_ai_core::{
    AppId, ConsentLedger, ErrorKind, JobState, SecretRef, SecretString, StoreFile,
};
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use describe_tool::{KEY_NAME, Rig, Seen, connection, model, photo_request, tool};

struct World {
    _dir: tempfile::TempDir,
    shared: SharedStore,
    secrets: Arc<MemoryStore>,
}

impl World {
    /// A connections file with the declared tool and the launcher's choice already made.
    fn chosen(remote: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let shared = SharedStore::new(dir.path().join("connections.json"));
        shared
            .update(|s| s.connections.push(connection(remote)))
            .unwrap();
        let world = Self {
            _dir: dir,
            shared,
            secrets: Arc::new(MemoryStore::new()),
        };
        let desk = world.desk();
        desk.register_tool(tool()).unwrap();
        desk.choose(&tool(), &connection(remote).id, &model().id)
            .unwrap();
        world
    }

    fn client(&self, app: &str, factory: ProviderFactory) -> AiClient {
        AiClient::new(
            AppId::new(app).unwrap(),
            self.shared.clone(),
            self.secrets.clone(),
        )
        .with_factory(factory)
    }

    fn desk(&self) -> ToolDesk {
        let launcher = AiClient::new(
            AppId::new("launcher").unwrap(),
            self.shared.clone(),
            self.secrets.clone(),
        );
        ToolDesk::new(launcher, self.shared.clone(), None)
    }
}

fn finish(job: &craft_ai_client::Job) -> Finished {
    job.wait(Duration::from_millis(2), Duration::from_secs(10))
        .unwrap()
}

fn text(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Event::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn two_apps_run_the_one_declared_tool_through_their_own_adapters() {
    let world = World::chosen(false);
    let (a, b) = (Rig::new(), Rig::new());
    let photo = world.client("photocraft", a.factory());
    let light = world.client("lightcraft", b.factory());

    for client in [&photo, &light] {
        let RoleStatus::Ready(ready) = client.status(&tool()) else {
            panic!("the chosen model should be ready in every app that ships the adapter");
        };
        assert_eq!(ready.model.as_str(), "describe-v1");
        let plan = client.plan(&tool(), photo_request(4)).unwrap();
        assert!(!plan.needs_consent(&ConsentLedger::default()));
        let done = finish(&client.start(&plan, &ConsentLedger::default()).unwrap());
        assert_eq!(done.state, JobState::Succeeded);
        assert_eq!(text(&done.events), "A red door.");
    }

    let expected = vec![Seen {
        model: model().id,
        capability: tool().capability,
        source_bytes: vec![4],
    }];
    assert_eq!(
        a.requests(),
        expected,
        "photocraft's adapter got the request"
    );
    assert_eq!(
        b.requests(),
        expected,
        "lightcraft's adapter got the request"
    );
    assert!(
        world.shared.load().unwrap().apps.is_empty(),
        "nothing was set up per app"
    );
}

#[test]
fn an_app_without_the_adapter_says_so_and_sends_nothing() {
    let world = World::chosen(false);
    let with = Rig::new();
    let ships_it = world.client("photocraft", with.factory());
    let lacks_it = world.client("lightcraft", ProviderFactory::standard());

    assert!(matches!(ships_it.status(&tool()), RoleStatus::Ready(_)));
    let RoleStatus::Unavailable { reason } = lacks_it.status(&tool()) else {
        panic!("an app with no adapter must not report Ready");
    };
    assert!(reason.contains("can't run the chosen model"), "{reason}");

    // Planning refuses too, so nobody is asked to agree to a send this app cannot make.
    let err = lacks_it.plan(&tool(), photo_request(4)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert!(
        err.message.contains("can't run the chosen model"),
        "{}",
        err.message
    );
    // And a plan made elsewhere is still refused at start, with the same words.
    let elsewhere = ships_it.plan(&tool(), photo_request(4)).unwrap();
    let err = lacks_it
        .start(&elsewhere, &ConsentLedger::default())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert!(
        err.message.contains("can't run the chosen model"),
        "{}",
        err.message
    );
    assert!(
        with.requests().is_empty(),
        "the other app's adapter was not used"
    );

    let view = lacks_it.picker(&tool(), &[]).unwrap();
    assert!(
        view.current.unwrap().problem.is_some(),
        "the picker agrees with status"
    );
    assert!(
        ships_it
            .picker(&tool(), &[])
            .unwrap()
            .current
            .unwrap()
            .problem
            .is_none()
    );
}

#[test]
fn a_remote_service_asks_first_and_nothing_is_touched_before_consent() {
    let world = World::chosen(true);
    world
        .secrets
        .set(
            &SecretRef::new(KEY_NAME).unwrap(),
            &SecretString::new("sk-test-not-a-real-key"),
        )
        .unwrap();
    let rig = Rig::new();
    let client = world.client("photocraft", rig.factory());

    let plan = client.plan(&tool(), photo_request(4)).unwrap();
    let mut ledger = ConsentLedger::default();
    assert!(plan.needs_consent(&ledger));
    let shown = format!("{:?}", plan.disclosure());
    assert!(shown.contains("describe.example.com"), "{shown}");

    let err = client.start(&plan, &ledger).unwrap_err();
    assert_eq!(err.kind, ErrorKind::ConsentRequired);
    assert_eq!(rig.adapters_built(), 0, "no adapter before consent");
    assert!(rig.requests().is_empty());

    ledger.grant(plan.disclosure());
    let done = finish(&client.start(&plan, &ledger).unwrap());
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(rig.requests().len(), 1);
}

#[test]
fn cancelling_one_apps_job_leaves_the_other_apps_job_alone() {
    let world = World::chosen(false);
    let photo = world.client("photocraft", Rig::new().factory());
    let light = world.client("lightcraft", Rig::new().factory());
    let ledger = ConsentLedger::default();

    let first = photo
        .start(&photo.plan(&tool(), photo_request(4)).unwrap(), &ledger)
        .unwrap();
    let second = light
        .start(&light.plan(&tool(), photo_request(4)).unwrap(), &ledger)
        .unwrap();
    assert!(matches!(
        first.poll().unwrap().state,
        JobState::Running { .. }
    ));
    first.cancel().unwrap();
    first.cancel().unwrap(); // idempotent

    assert_eq!(finish(&first).state, JobState::Cancelled);
    assert_eq!(finish(&second).state, JobState::Succeeded);
}

#[test]
fn a_connections_file_from_before_custom_tools_keeps_working() {
    // Build a store that already has a connection and a launcher choice for the tool's id, then
    // write it the way an older build did: no `custom_tools` field at all.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("connections.json");
    let old = StoreFile {
        connections: vec![connection(false)],
        defaults: vec![craft_ai_core::Binding {
            role: tool().id,
            connection: connection(false).id,
            model: model().id,
        }],
        ..StoreFile::default()
    };
    let mut json: serde_json::Value = serde_json::from_str(&old.to_json().unwrap()).unwrap();
    json.as_object_mut().unwrap().remove("custom_tools");
    let raw = serde_json::to_string_pretty(&json).unwrap();
    assert!(!raw.contains("custom_tools"));
    std::fs::write(&path, &raw).unwrap();

    let shared = SharedStore::new(&path);
    let rig = Rig::new();
    let client = AiClient::new(
        AppId::new("photocraft").unwrap(),
        shared.clone(),
        Arc::new(MemoryStore::new()),
    )
    .with_factory(rig.factory());
    assert!(
        matches!(client.status(&tool()), RoleStatus::Ready(_)),
        "old file, undeclared tool"
    );

    let desk = ToolDesk::new(
        AiClient::new(
            AppId::new("launcher").unwrap(),
            shared.clone(),
            Arc::new(MemoryStore::new()),
        ),
        shared.clone(),
        None,
    );
    desk.register_tool(tool()).unwrap();
    let after = shared.load().unwrap();
    assert_eq!(after.schema_version, old.schema_version);
    assert_eq!(after.connections, old.connections);
    assert_eq!(after.defaults, old.defaults);
    assert!(
        matches!(client.status(&tool()), RoleStatus::Ready(_)),
        "declared, same choice"
    );
    let done = finish(
        &client
            .start(
                &client.plan(&tool(), photo_request(2)).unwrap(),
                &ConsentLedger::default(),
            )
            .unwrap(),
    );
    assert_eq!(done.state, JobState::Succeeded);

    // Removing the declaration takes its choice with it and leaves the connection.
    desk.remove_tool(&tool().id).unwrap();
    let gone = shared.load().unwrap();
    assert!(gone.defaults.is_empty());
    assert_eq!(gone.connections, old.connections);
}

//! Adopting a built-in engine from the app side, end to end, using only the public surface:
//! enable the tool from a weights file, register the engine's runners, ask for a capability.
//! Nothing here parses a store, reads a key, checks consent or loads a model.
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use craft_ai_client::{AiClient, EngineRegistry, ProviderFactory, RoleStatus, ToolDesk};
use craft_ai_core::{
    AppId, ConsentLedger, ErrorKind, Event, InputRole, JobState, Payload, Request, StoreFile,
};
use craft_ai_engine::testing::FakeRunner;
use craft_ai_features::{Feature, GpuVendor};
use craft_ai_native::secrets::MemoryStore;
use craft_ai_native::shared::SharedStore;

fn select_subject() -> Feature {
    craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == "select-subject")
        .unwrap()
}

struct World {
    dir: tempfile::TempDir,
    shared: SharedStore,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let shared = SharedStore::new(dir.path().join("connections.json"));
        shared.replace(&StoreFile::default()).unwrap();
        Self { dir, shared }
    }

    fn client(&self, app: &str, registry: EngineRegistry) -> AiClient {
        let mut factory = ProviderFactory::standard();
        factory.register_engines(registry);
        AiClient::new(
            AppId::new(app).unwrap(),
            self.shared.clone(),
            Arc::new(MemoryStore::new()),
        )
        .with_factory(factory)
    }

    /// What the launcher does when a person finishes setting a tool up.
    fn set_up(&self, engine: &str) {
        let file = self.dir.path().join("sam3.safetensors");
        std::fs::write(&file, b"weights").unwrap();
        let desk = ToolDesk::new(
            self.client("launcher", EngineRegistry::new()),
            self.shared.clone(),
            None,
        );
        desk.enable_native_with(&select_subject(), engine, &file)
            .unwrap();
    }
}

fn request() -> Request {
    Request::new(select_subject().capability.clone())
        .with_input(InputRole::Source, Payload::text("the picture"))
}

#[test]
fn an_app_runs_a_built_in_model_by_asking_for_the_capability() {
    let w = World::new();
    w.set_up("candle");
    let role = select_subject().role().unwrap();

    let runner = FakeRunner::new("candle", "sam3", "image.segment");
    let loads = runner.loads.clone();
    let mut registry = EngineRegistry::new();
    registry.register(runner);
    let app = w.client("lightcraft", registry);

    assert!(matches!(app.status(&role), RoleStatus::Ready(_)));
    let ledger = ConsentLedger::default();
    let plan = app.plan(&role, request()).unwrap();
    assert!(!plan.needs_consent(&ledger));
    let done = app
        .start(&plan, &ledger)
        .unwrap()
        .wait(Duration::from_millis(2), Duration::from_secs(10))
        .unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    let outputs = done.events.iter().filter(|e| matches!(e, Event::Output(_)));
    assert_eq!(outputs.count(), 1);
    assert_eq!(loads.load(Ordering::SeqCst), 1);
}

#[test]
fn a_second_job_reuses_the_model_the_first_one_loaded() {
    let w = World::new();
    w.set_up("candle");
    let role = select_subject().role().unwrap();
    let runner = FakeRunner::new("candle", "sam3", "image.segment");
    let (loads, runs) = (runner.loads.clone(), runner.runs.clone());
    let mut registry = EngineRegistry::new();
    registry.register(runner);
    let app = w.client("lightcraft", registry);
    let ledger = ConsentLedger::default();
    for _ in 0..3 {
        // The client asks the factory for a provider on every job.
        let plan = app.plan(&role, request()).unwrap();
        let done = app
            .start(&plan, &ledger)
            .unwrap()
            .wait(Duration::from_millis(2), Duration::from_secs(10))
            .unwrap();
        assert_eq!(done.state, JobState::Succeeded);
    }
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    assert_eq!(
        loads.load(Ordering::SeqCst),
        1,
        "each job loaded the weights again"
    );
}

#[test]
fn an_app_without_the_engine_is_told_so_not_crashed() {
    let w = World::new();
    w.set_up("burn");
    let role = select_subject().role().unwrap();
    let mut registry = EngineRegistry::new();
    registry.register(FakeRunner::new("candle", "sam3", "image.segment"));
    let app = w.client("lightcraft", registry);
    let error = match app.plan(&role, request()) {
        Err(e) => e,
        Ok(plan) => app.start(&plan, &ConsentLedger::default()).err().unwrap(),
    };
    assert_eq!(error.kind, ErrorKind::Unsupported);
}

#[test]
fn a_registered_engine_declares_its_accelerators_for_engine_advice() {
    let mut runner = FakeRunner::new("candle", "sam3", "image.segment");
    runner.vendors = vec![GpuVendor::Nvidia, GpuVendor::Apple];
    let mut registry = EngineRegistry::new();
    registry.register(runner);
    let mut factory = ProviderFactory::standard();
    // Without the cargo feature, standard() has no built-in engines of its own.
    assert_eq!(factory.knows("candle"), cfg!(feature = "candle"));
    factory.register_engines(registry);
    assert!(factory.knows("candle") && factory.knows("Candle"));
    assert_eq!(
        factory.accelerators("candle"),
        Some(&[GpuVendor::Nvidia, GpuVendor::Apple][..])
    );
}

#[cfg(feature = "candle")]
#[test]
fn the_candle_feature_is_the_whole_of_adopting_the_candle_engine() {
    // No registry, no registration: the cargo feature is all an app turns on.
    let factory = ProviderFactory::standard();
    assert!(factory.knows("candle"));
    // Whether this build can use a graphics card is the engine's own statement, not the app's.
    assert_eq!(
        factory.accelerators("candle").map(<[_]>::is_empty),
        Some(!cfg!(target_os = "macos"))
    );
}

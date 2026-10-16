//! A maintainer's template for adding one tool to the shared AI layer, with no app interface.
//!
//! It is shared by `examples/custom_tool.rs` (run it to see the steps) and
//! `tests/custom_tool_conformance.rs` (the checks every adapter for a declared tool must pass).
//!
//! The four moving parts, in the order a maintainer meets them:
//!
//! 1. the tool declaration ([`tool`]), one suite-wide id, capability and input set;
//! 2. a model that can do it, offered by a connection ([`connection`]);
//! 3. an adapter for that connection's provider name, registered on a [`ProviderFactory`]
//!    ([`Rig::factory`]); and
//! 4. the app's own client, which finds all of the above through the shared store.
//!
//! The adapter here plays a script and writes down what it was asked, so a test can see that the
//! request really reached it. It sends nothing anywhere.

// The example and the conformance test each use a different part of this module.
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use craft_ai_client::ProviderFactory;
use craft_ai_core::mock::{MockProvider, Outcome, Script};
use craft_ai_core::{
    AiError, CapabilityId, CapabilitySpec, Connection, InputRole, InputRole::Source, JobId,
    JobSnapshot, ModelId, ModelInfo, Payload, Provider, Request, Role, RoleId,
};
use serde_json::json;

/// The provider name the example adapter answers to.
pub const PROVIDER: &str = "describer";
/// The secret locator a remote variant of the connection uses.
pub const KEY_NAME: &str = "describer-key";

/// The declaration, step 1. Every app that has this tool reads the same id, capability and inputs.
pub fn tool() -> Role {
    Role {
        id: RoleId::new("describe-photo").unwrap(),
        label: "Describe photograph".into(),
        capability: CapabilityId::parse("image.describe").unwrap(),
        inputs: vec![Source],
    }
}

/// A model that takes the tool's inputs.
pub fn model() -> ModelInfo {
    ModelInfo::new(ModelId::new("describe-v1").unwrap(), "Describe v1")
        .with(CapabilitySpec::new(tool().capability).accepting(InputRole::Source, 1))
}

/// A connection offering that model: on this computer, or a remote service with a saved key.
pub fn connection(remote: bool) -> Connection {
    serde_json::from_value(json!({
        "id": "describer-service", "label": "Describer", "provider": PROVIDER,
        "endpoint": if remote { "https://describe.example.com/v1" } else { "http://127.0.0.1:9" },
        "locality": if remote { "remote" } else { "local" },
        "auth": if remote { json!({"kind": "api_key", "secret": KEY_NAME}) } else { json!({"kind": "none"}) },
        "billing": if remote { "metered" } else { "free" },
        "models": [model()],
    }))
    .unwrap()
}

/// One request the adapter received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub model: ModelId,
    pub capability: CapabilityId,
    /// Size in bytes of each inline source image.
    pub source_bytes: Vec<usize>,
}

/// A small photo request: one inline source image.
pub fn photo_request(bytes: usize) -> Request {
    Request::new(tool().capability).with_input(
        Source,
        Payload::inline("image/png", vec![7; bytes]).unwrap(),
    )
}

/// The adapter, step 3. It validates like any real one (through the scripted provider), writes down
/// the request, then plays a short description that arrives over several polls.
struct Describer {
    inner: MockProvider,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Provider for Describer {
    fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        self.inner.models()
    }

    fn submit(&self, model: &ModelId, request: &Request) -> Result<JobId, AiError> {
        let id = self.inner.submit(model, request)?;
        let source_bytes = request
            .inputs
            .iter()
            .filter(|i| i.role == Source)
            .filter_map(|i| i.payload.size_hint())
            .map(|n| usize::try_from(n).unwrap_or(usize::MAX))
            .collect();
        self.seen.lock().unwrap().push(Seen {
            model: model.clone(),
            capability: request.capability.clone(),
            source_bytes,
        });
        Ok(id)
    }

    fn poll(&self, job: &JobId) -> Result<JobSnapshot, AiError> {
        self.inner.poll(job)
    }

    fn cancel(&self, job: &JobId) -> Result<(), AiError> {
        self.inner.cancel(job)
    }
}

/// One app's adapter registration, with counters a test can read. Each app builds its own.
#[derive(Clone, Default)]
pub struct Rig {
    pub seen: Arc<Mutex<Vec<Seen>>>,
    pub builds: Arc<AtomicUsize>,
}

impl Rig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub fn adapters_built(&self) -> usize {
        self.builds.load(Ordering::SeqCst)
    }

    /// The factory an app that ships this adapter hands to its client.
    pub fn factory(&self) -> ProviderFactory {
        let rig = self.clone();
        let mut factory = ProviderFactory::standard();
        factory.register(PROVIDER, move |conn, _secret| {
            rig.builds.fetch_add(1, Ordering::SeqCst);
            let script = Script {
                running_polls: 3,
                text_chunks: vec!["A red ".into(), "door.".into()],
                outcome: Outcome::Succeed(Vec::new()),
            };
            Ok(Arc::new(Describer {
                inner: MockProvider::new(conn.models.clone(), script),
                seen: rig.seen.clone(),
            }) as Arc<dyn Provider>)
        });
        factory
    }
}

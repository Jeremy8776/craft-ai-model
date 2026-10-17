//! [`AiClient`]: what an app calls.
//!
//! One flow for every Craft app, matching the adapter checklist:
//!
//! 1. [`status`](AiClient::status): is this feature usable? Gives a plain reason when it is not
//!    (no model chosen, connection switched off, key missing), so the app can grey the feature out
//!    and point at the launcher. Every app works with no connection at all.
//! 2. [`plan`](AiClient::plan): resolve the role, check the request against the chosen model, and
//!    build the [`Disclosure`] of what would leave the machine. Nothing is sent and no key is read.
//! 3. The app shows the disclosure if [`Plan::needs_consent`], and records agreement in its
//!    [`ConsentLedger`].
//! 4. [`start`](AiClient::start): re-checks consent (so it cannot be skipped), reads the key, builds
//!    the provider and submits. The returned [`Job`] is polled and cancelled by the app's own job
//!    system.
//!
//! The key exists only inside `start`, from the moment it is read until the adapter is built. It is
//! never in a [`Plan`], a [`Job`], a status or an error.

use std::sync::Arc;
use std::time::{Duration, Instant};

use craft_ai_core::job::Event;
use craft_ai_core::payload::Limits;
use craft_ai_core::picks::{Candidate, choose};
use craft_ai_core::role::eligible;
use craft_ai_core::{
    AiError, AppId, Billing, Binding, Catalog, Connection, ConnectionId, ConsentLedger, Disclosure,
    ErrorKind, HistoryLog, JobId, JobSnapshot, JobState, Locality, ModelId, Pick, PickedBy,
    Provider, Request, Role, RoleId, Usage,
};
use craft_ai_native::secrets::{EnvStore, KeychainStore, LayeredStore, SecretStore, secret_for};
use craft_ai_native::shared::SharedStore;
use craft_ai_native::{CatalogFile, HistoryFile};

use crate::factory::ProviderFactory;
use crate::history::{self, Recorder};

/// What a person is told when the app has no adapter for the model they chose. One sentence, so
/// status, plan and start agree and the plain-language test can pin it.
const NO_ADAPTER: &str = "This app can't run the chosen model yet. Choose another in Set up AI.";

/// Whether a role can run right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoleStatus {
    Ready(RoleReady),
    /// Not usable, with a sentence the app can show.
    Unavailable {
        reason: String,
    },
}

/// What a ready role is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleReady {
    pub connection: ConnectionId,
    pub connection_label: String,
    pub model: ModelId,
    pub model_label: String,
    pub locality: Locality,
    pub billing: Billing,
}

/// One entry in a model picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub connection: ConnectionId,
    pub connection_label: String,
    pub model: ModelId,
    pub model_label: String,
    pub locality: Locality,
}

/// A model that earned a preset for a job, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickedChoice {
    pub pick: Pick,
    pub choice: Choice,
    pub by: PickedBy,
}

/// A checked request waiting for consent. Holds no secret.
#[derive(Clone, Debug)]
pub struct Plan {
    pub(crate) connection: Connection,
    pub(crate) model: ModelId,
    pub(crate) request: Request,
    pub(crate) disclosure: Disclosure,
    pub(crate) role_id: RoleId,
    pub(crate) role_label: String,
}

/// Unix seconds now; 0 if the clock is before 1970, which makes timed agreements read as valid
/// rather than lapsed, so a broken clock never asks twice.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Plan {
    /// What would be sent where. Show this to the user.
    pub fn disclosure(&self) -> &Disclosure {
        &self.disclosure
    }

    /// True when the person must agree first: nothing on file covers this app, connection,
    /// destination, capability and kind of data, or the agreement has lapsed.
    pub fn needs_consent(&self, ledger: &ConsentLedger) -> bool {
        self.needs_consent_at(ledger, now_secs())
    }

    /// [`needs_consent`](Self::needs_consent) at a given time (unix seconds).
    pub fn needs_consent_at(&self, ledger: &ConsentLedger, now: u64) -> bool {
        ledger.needs_consent(&self.disclosure, now)
    }

    pub fn connection_label(&self) -> &str {
        &self.connection.label
    }

    pub fn model(&self) -> &ModelId {
        &self.model
    }
}

/// A started job.
#[derive(Clone)]
pub struct Job {
    provider: Arc<dyn Provider>,
    id: JobId,
    recorder: Option<Arc<Recorder>>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Job({})", self.id)
    }
}

/// Everything a finished job produced.
#[derive(Debug)]
pub struct Finished {
    pub state: JobState,
    pub events: Vec<Event>,
    pub usage: Option<Usage>,
}

impl Job {
    pub fn id(&self) -> &JobId {
        &self.id
    }

    pub fn poll(&self) -> Result<JobSnapshot, AiError> {
        let snap = self.provider.poll(&self.id)?;
        if let Some(recorder) = &self.recorder {
            recorder.observe(&snap.state, snap.usage.as_ref());
        }
        Ok(snap)
    }

    pub fn cancel(&self) -> Result<(), AiError> {
        self.provider.cancel(&self.id)
    }

    /// Polls every `interval` until the job ends or `timeout` passes, collecting events. For
    /// scripts and tests; an app with its own job system polls instead.
    pub fn wait(&self, interval: Duration, timeout: Duration) -> Result<Finished, AiError> {
        let deadline = Instant::now() + timeout;
        let mut events = Vec::new();
        loop {
            let snap = self.poll()?;
            events.extend(snap.events);
            if snap.state.is_terminal() {
                return Ok(Finished {
                    state: snap.state,
                    events,
                    usage: snap.usage,
                });
            }
            if Instant::now() >= deadline {
                let _ = self.cancel();
                if let Some(recorder) = &self.recorder {
                    recorder.gave_up();
                }
                return Err(AiError::new(ErrorKind::Network, "the job took too long"));
            }
            std::thread::sleep(interval);
        }
    }
}

/// The app's connection to the AI layer.
pub struct AiClient {
    app: AppId,
    pub(crate) shared: SharedStore,
    pub(crate) secrets: Arc<dyn SecretStore>,
    factory: ProviderFactory,
    history: Option<Arc<HistoryFile>>,
    keep_prompts: bool,
    catalog: Catalog,
}

impl AiClient {
    pub fn new(app: AppId, shared: SharedStore, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            app,
            shared,
            secrets,
            factory: ProviderFactory::standard(),
            history: None,
            keep_prompts: false,
            catalog: Catalog::builtin(),
        }
    }

    /// The usual setup: the standard shared store location, and keys from the environment then the
    /// OS keychain. (The passphrase-encrypted file is not included, because only the caller can
    /// supply its passphrase; build a [`LayeredStore`] and use [`new`](Self::new) to add it.)
    pub fn standard(app: AppId) -> Result<Self, AiError> {
        let shared = SharedStore::at_default_location()?;
        let secrets = LayeredStore::new(vec![
            Box::new(EnvStore::new()),
            Box::new(KeychainStore::new()),
        ]);
        let mut client = Self::new(app, shared, Arc::new(secrets));
        client.history = HistoryFile::at_default_location().map(Arc::new);
        Ok(client)
    }

    /// Logs every job to `file` (see [`history`](Self::history)). [`standard`](Self::standard)
    /// already does this at the default location; `new` does not.
    pub fn with_history(mut self, file: HistoryFile) -> Self {
        self.history = Some(Arc::new(file));
        self
    }

    /// Replaces the curated catalog the presets use (the built-in one is empty to start with).
    pub fn with_catalog(mut self, catalog: Catalog) -> Self {
        self.catalog = catalog;
        self
    }

    /// Uses the curated picks in the shared folder's catalog file over the built-in ones, so a
    /// newer list reaches every app without a release. A missing or damaged file leaves the
    /// built-in picks (call [`CatalogFile::load`] to show a person why).
    pub fn with_catalog_file(self, file: &CatalogFile) -> Self {
        self.with_catalog(file.load().catalog)
    }

    /// Stops logging jobs.
    pub fn without_history(mut self) -> Self {
        self.history = None;
        self
    }

    /// Whether the log keeps the prompt text (clipped). Off by default: the log then holds what
    /// was sent and where, not what was said.
    pub fn keep_prompts_in_history(mut self, keep: bool) -> Self {
        self.keep_prompts = keep;
        self
    }

    /// Every logged job from every app, newest first. Lines that cannot be read are counted in
    /// [`HistoryLog::skipped`] rather than failing the read.
    pub fn history(&self) -> Result<HistoryLog, AiError> {
        let file = self.history_file()?;
        let read = file
            .read()
            .map_err(|e| AiError::new(ErrorKind::Internal, format!("history unavailable: {e}")))?;
        Ok(craft_ai_core::history::merge(read.records, read.skipped))
    }

    /// Deletes the log for every app.
    pub fn clear_history(&self) -> Result<(), AiError> {
        self.history_file()?
            .clear()
            .map_err(|e| AiError::new(ErrorKind::Internal, format!("history unavailable: {e}")))
    }

    fn history_file(&self) -> Result<&Arc<HistoryFile>, AiError> {
        self.history.as_ref().ok_or_else(|| {
            AiError::new(ErrorKind::Unsupported, "request history is turned off here")
        })
    }

    /// Replaces the provider factory, to add adapters.
    pub fn with_factory(mut self, factory: ProviderFactory) -> Self {
        self.factory = factory;
        self
    }

    pub fn app(&self) -> &AppId {
        &self.app
    }

    pub(crate) fn has_adapter(&self, provider: &str) -> bool {
        self.factory.knows(provider)
    }

    pub(crate) fn declared_accelerators(
        &self,
        provider: &str,
    ) -> Option<Vec<craft_ai_features::GpuVendor>> {
        self.factory.accelerators(provider).map(<[_]>::to_vec)
    }

    /// Where this app's consent ledger is kept by default.
    pub fn consent_file(&self) -> Option<craft_ai_native::ConsentFile> {
        craft_ai_native::ConsentFile::at_default_location(&self.app)
    }

    /// Can `role` run now? The reason, when it cannot, is safe to show.
    pub fn status(&self, role: &Role) -> RoleStatus {
        let unavailable = |reason: String| RoleStatus::Unavailable { reason };
        let store = match self.shared.load() {
            Ok(s) => s,
            Err(e) => return unavailable(e.to_string()),
        };
        let resolved = match store.resolve(&self.app, role) {
            Ok(r) => r,
            Err(e) => return unavailable(e.to_string()),
        };
        if !self.factory.knows(&resolved.connection.provider) {
            return unavailable(NO_ADAPTER.to_owned());
        }
        if let Err(e) = secret_for(&*self.secrets, resolved.connection) {
            return unavailable(e.message);
        }
        RoleStatus::Ready(RoleReady {
            connection: resolved.connection.id.clone(),
            connection_label: resolved.connection.label.clone(),
            model: resolved.model.id.clone(),
            model_label: resolved.model.label.clone(),
            locality: resolved.connection.locality,
            billing: resolved.connection.billing,
        })
    }

    /// Every model that could fill `role`, for a picker.
    pub fn choices(&self, role: &Role) -> Result<Vec<Choice>, AiError> {
        let store = self.shared.load()?;
        store
            .check_tool(role)
            .map_err(|e| AiError::invalid(e.to_string()))?;
        Ok(eligible(role, &store.connections)
            .into_iter()
            .map(|r| Choice {
                connection: r.connection.id.clone(),
                connection_label: r.connection.label.clone(),
                model: r.model.id.clone(),
                model_label: r.model.label.clone(),
                locality: r.connection.locality,
            })
            .collect())
    }

    /// The presets (recommended, lowest cost, newest and so on) each model earns for `role`, in
    /// picker order. A preset nobody earns is left out: a curated pick the connections do not offer
    /// is skipped, and cheapest or newest need the provider to have reported a price or a date.
    /// Nothing is bound; the app offers these and binds only when the person agrees.
    pub fn picks(&self, role: &Role) -> Result<Vec<PickedChoice>, AiError> {
        let store = self.shared.load()?;
        store
            .check_tool(role)
            .map_err(|e| AiError::invalid(e.to_string()))?;
        let eligible = eligible(role, &store.connections);
        let candidates: Vec<Candidate<'_>> = eligible
            .iter()
            .map(|r| Candidate {
                provider: &r.connection.provider,
                model: r.model,
                free: r.connection.billing == Billing::Free,
            })
            .collect();
        let mut out = Vec::new();
        for picked in choose(&role.capability, &candidates, &self.catalog) {
            let Some(r) = eligible.get(picked.candidate) else {
                continue;
            };
            out.push(PickedChoice {
                pick: picked.pick,
                choice: Choice {
                    connection: r.connection.id.clone(),
                    connection_label: r.connection.label.clone(),
                    model: r.model.id.clone(),
                    model_label: r.model.label.clone(),
                    locality: r.connection.locality,
                },
                by: picked.by,
            });
        }
        Ok(out)
    }

    /// For each of `roles` that cannot run yet, the model the catalog recommends. This is what to
    /// offer after a connection tests well ("use the recommended models for N features?"); it binds
    /// nothing, so a model is never chosen for the person without their yes.
    pub fn recommended(&self, roles: &[Role]) -> Result<Vec<(RoleId, Choice)>, AiError> {
        let mut out = Vec::new();
        for role in roles {
            if matches!(self.status(role), RoleStatus::Ready(_)) {
                continue;
            }
            if let Some(p) = self
                .picks(role)?
                .into_iter()
                .find(|p| p.pick == Pick::Recommended)
            {
                out.push((role.id.clone(), p.choice));
            }
        }
        Ok(out)
    }

    /// Sets this app's own choice for `role`, overriding the launcher default. Refused, and nothing
    /// written, if the connection or model cannot fill the role.
    pub fn bind(
        &self,
        role: &Role,
        connection: &ConnectionId,
        model: &ModelId,
    ) -> Result<(), AiError> {
        let binding = Binding {
            role: role.id.clone(),
            connection: connection.clone(),
            model: model.clone(),
        };
        self.shared.update(|store| {
            store
                .check_tool(role)
                .map_err(|e| AiError::invalid(e.to_string()))?;
            craft_ai_core::role::resolve(
                role,
                &[std::slice::from_ref(&binding)],
                &store.connections,
            )
            .map_err(|e| AiError::invalid(e.to_string()))?;
            set_override(store, &self.app, binding);
            Ok::<(), AiError>(())
        })??;
        Ok(())
    }

    /// Removes this app's own choice, so the launcher default applies again.
    pub fn clear_binding(&self, role: &RoleId) -> Result<(), AiError> {
        self.shared.update(|store| {
            if let Some(settings) = store.apps.get_mut(&self.app) {
                settings.overrides.retain(|b| &b.role != role);
            }
        })?;
        Ok(())
    }

    /// Resolves the role and checks the request. Sends nothing and reads no key.
    pub fn plan(&self, role: &Role, request: Request) -> Result<Plan, AiError> {
        if request.capability != role.capability {
            return Err(AiError::invalid(format!(
                "this request is for {}, but \"{}\" needs {}",
                request.capability, role.label, role.capability
            )));
        }
        let store = self.shared.load()?;
        let resolved = store
            .resolve(&self.app, role)
            .map_err(|e| AiError::unsupported(e.to_string()))?;
        request.validate(resolved.spec, &Limits::default())?;
        // Refuse here, before anyone is asked to agree to send something this app cannot send.
        if !self.factory.knows(&resolved.connection.provider) {
            return Err(AiError::unsupported(NO_ADAPTER));
        }
        let disclosure = Disclosure::for_request(&self.app, resolved.connection, &request);
        Ok(Plan {
            connection: resolved.connection.clone(),
            model: resolved.model.id.clone(),
            request,
            disclosure,
            role_id: role.id.clone(),
            role_label: role.label.clone(),
        })
    }

    /// Starts the job. Refuses with [`ErrorKind::ConsentRequired`] if `ledger` does not cover the
    /// plan's disclosure.
    pub fn start(&self, plan: &Plan, ledger: &ConsentLedger) -> Result<Job, AiError> {
        if plan.needs_consent(ledger) {
            return Err(AiError::new(
                ErrorKind::ConsentRequired,
                format!(
                    "sending to {} needs your agreement first",
                    plan.disclosure.destination
                ),
            ));
        }
        let provider = match self.prepare(plan) {
            Ok(provider) => provider,
            Err(e) => {
                self.log_refused(plan, None, &e);
                return Err(e);
            }
        };
        let estimate = provider.estimate(&plan.model, &plan.request).ok().flatten();
        match provider.submit(&plan.model, &plan.request) {
            Ok(id) => {
                let recorder = self.history.as_ref().map(|file| {
                    Arc::new(history::begin(file, plan, &id, estimate, self.keep_prompts))
                });
                Ok(Job {
                    provider,
                    id,
                    recorder,
                })
            }
            Err(e) => {
                self.log_refused(plan, estimate, &e);
                Err(e)
            }
        }
    }

    /// Reads the key and builds the adapter.
    fn prepare(&self, plan: &Plan) -> Result<Arc<dyn Provider>, AiError> {
        // Without an adapter there is nothing to give the key to, so do not fetch it.
        if !self.factory.knows(&plan.connection.provider) {
            return Err(AiError::unsupported(NO_ADAPTER));
        }
        let secret = secret_for(&*self.secrets, &plan.connection)?;
        self.factory.build(&plan.connection, secret)
    }

    fn log_refused(&self, plan: &Plan, estimate: Option<craft_ai_core::CostEstimate>, e: &AiError) {
        if let Some(file) = &self.history {
            history::refused(file, plan, estimate, self.keep_prompts, e);
        }
    }
}

impl std::fmt::Debug for AiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AiClient({})", self.app)
    }
}

fn set_override(store: &mut craft_ai_core::StoreFile, app: &AppId, binding: Binding) {
    let settings = store.apps.entry(app.clone()).or_default();
    settings.overrides.retain(|b| b.role != binding.role);
    settings.overrides.push(binding);
}

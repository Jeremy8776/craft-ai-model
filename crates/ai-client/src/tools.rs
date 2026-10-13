//! The launcher's side of "which model does this tool use": choose a model for a tool, check the
//! choice still holds, and run a small real sample to prove it does the job.
//!
//! A tool is a [`Role`] whose id is the tool's id (`select-subject`). The choice is stored as a
//! launcher default binding, which every app that declares the same role inherits unless it
//! overrides it. Everything goes through [`AiClient`] as the app `launcher`, so a sample takes
//! the same path an app does: the binding is resolved, the request is checked against the model,
//! a remote destination needs agreement first, and the key is read only when the job starts.

use std::time::{Duration, Instant};

use craft_ai_model::job::Event;
use craft_ai_model::role::resolve;
use craft_ai_model::{
    AiError, AppId, Billing, Binding, ConnectionId, ConsentLedger, Disclosure, JobState, Locality,
    ModelId, Payload, Request, Role, RoleId, StoreFile,
};
use craft_ai_native::ConsentFile;
use craft_ai_native::shared::SharedStore;

use craft_ai_features::{Feature, setup_options};

use crate::client::{AiClient, Choice, Job, RoleStatus};
use crate::popup::MissingModel;

/// What the launcher shows for one tool.
#[derive(Clone, Debug)]
pub struct ToolView {
    /// Every connected model that could do the job.
    pub choices: Vec<Choice>,
    /// The launcher's current choice, even when it no longer holds.
    pub bound: Option<(ConnectionId, ModelId)>,
    /// Whether the choice resolves and its key is in place right now.
    pub status: RoleStatus,
}

/// What starting a sample came to.
pub enum TestStart {
    Started(Job),
    /// Nothing was sent. The person has to agree to this destination first.
    NeedsConsent {
        disclosure: Disclosure,
        connection_label: String,
        billing: Billing,
    },
}

/// How a sample went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SampleReport {
    Passed { summary: String, millis: u64 },
    Failed { message: String },
}

pub struct ToolDesk {
    pub(crate) client: AiClient,
    pub(crate) shared: SharedStore,
    consent: Option<ConsentFile>,
}

impl ToolDesk {
    pub fn new(client: AiClient, shared: SharedStore, consent: Option<ConsentFile>) -> Self {
        Self {
            client,
            shared,
            consent,
        }
    }

    /// The app id the launcher acts as.
    pub fn app() -> Result<AppId, AiError> {
        AppId::new("launcher").map_err(|e| AiError::invalid(e.to_string()))
    }

    /// What the app's popup shows when `role` cannot run, or `None` when it can. `features` is the
    /// tool catalogue (`craft_ai_features::builtin`), so the real ways to set the tool up are the
    /// ones the launcher offers.
    pub fn missing_model(
        &self,
        role: &Role,
        features: &[Feature],
    ) -> Result<Option<MissingModel>, AiError> {
        let RoleStatus::Unavailable { reason } = self.client.status(role) else {
            return Ok(None);
        };
        let picker = self.client.picker(role, &setup_options(features))?;
        Ok(Some(MissingModel {
            tool: role.id.clone(),
            title: role.label.clone(),
            reason,
            picker,
        }))
    }

    pub fn view(&self, role: &Role) -> Result<ToolView, AiError> {
        let store = self.shared.load()?;
        let bound = store
            .defaults
            .iter()
            .find(|b| b.role == role.id)
            .map(|b| (b.connection.clone(), b.model.clone()));
        Ok(ToolView {
            choices: self.client.choices(role)?,
            bound,
            status: self.client.status(role),
        })
    }

    /// Makes `connection` and `model` the launcher's choice for `role`. Refused, with nothing
    /// written, when that model cannot do the job.
    pub fn choose(
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
        // Check inside the same store lock as the write: another app may change the tool or
        // connection between an unlocked read and the update.
        self.shared.update(|store| {
            store
                .check_tool(role)
                .map_err(|e| AiError::invalid(e.to_string()))?;
            resolve(role, &[std::slice::from_ref(&binding)], &store.connections)
                .map_err(|e| AiError::invalid(e.to_string()))?;
            set_default(store, binding);
            Ok::<(), AiError>(())
        })??;
        Ok(())
    }

    /// Gives every tool with no model chosen the best one that is connected: one on this computer
    /// first, then the first hosted one. A choice the person made is never touched, even one that
    /// no longer works, so what they picked is still there when the connection comes back. Returns
    /// the tools bound now.
    pub fn auto_bind(&self, roles: &[Role]) -> Result<Vec<RoleId>, AiError> {
        let mut bound = Vec::new();
        for role in roles {
            let view = self.view(role)?;
            if view.bound.is_some() {
                continue;
            }
            let pick = view
                .choices
                .iter()
                .find(|c| c.locality == Locality::Local)
                .or_else(|| view.choices.first());
            if let Some(choice) = pick {
                self.choose(role, &choice.connection, &choice.model)?;
                bound.push(role.id.clone());
            }
        }
        Ok(bound)
    }

    /// Custom tool declarations shared with every Craft app; declaring one does not install an adapter.
    pub fn custom_tools(&self) -> Result<Vec<Role>, AiError> {
        Ok(self.shared.load()?.custom_tools)
    }

    /// Register a user-defined tool across the suite, without changing the built-in catalogue.
    /// A name collision is refused rather than replacing a tool or its existing binding.
    pub fn register_tool(&self, role: Role) -> Result<(), AiError> {
        if craft_ai_features::builtin()
            .iter()
            .any(|f| f.id == role.id.as_str())
        {
            return Err(AiError::invalid(
                "that tool name is already used by a built-in tool",
            ));
        }
        self.shared.update(|store| {
            if store.custom_tools.iter().any(|old| old.id == role.id) {
                return Err(AiError::invalid("that tool name is already in use"));
            }
            store.custom_tools.push(role);
            Ok(())
        })??;
        Ok(())
    }

    /// Remove a custom tool and its stored choices from every app. Built-in tools cannot be removed.
    pub fn remove_tool(&self, id: &RoleId) -> Result<(), AiError> {
        self.shared.update(|store| {
            if !store.custom_tools.iter().any(|tool| &tool.id == id) {
                return Err(AiError::invalid("that custom tool does not exist"));
            }
            store.custom_tools.retain(|tool| &tool.id != id);
            store.defaults.retain(|binding| &binding.role != id);
            for app in store.apps.values_mut() {
                app.overrides.retain(|binding| &binding.role != id);
            }
            Ok(())
        })??;
        Ok(())
    }

    /// Forgets the launcher's choice for a tool.
    pub fn clear(&self, role: &RoleId) -> Result<(), AiError> {
        self.shared
            .update(|store| store.defaults.retain(|b| &b.role != role))?;
        Ok(())
    }

    /// Plans a sample. A remote destination the person has not agreed to yet comes back as
    /// [`TestStart::NeedsConsent`] with nothing sent; pass `agreed` once they have said yes and
    /// the agreement is recorded for the launcher, like any app's.
    pub fn start_test(
        &self,
        role: &Role,
        request: Request,
        agreed: bool,
    ) -> Result<TestStart, AiError> {
        let plan = self.client.plan(role, request)?;
        let mut ledger = self
            .consent
            .as_ref()
            .map_or_else(ConsentLedger::default, ConsentFile::load_or_empty);
        if plan.needs_consent(&ledger) {
            if !agreed {
                let billing = match self.client.status(role) {
                    RoleStatus::Ready(ready) => ready.billing,
                    RoleStatus::Unavailable { .. } => Billing::Metered,
                };
                return Ok(TestStart::NeedsConsent {
                    disclosure: plan.disclosure().clone(),
                    connection_label: plan.connection_label().to_owned(),
                    billing,
                });
            }
            ledger.grant(plan.disclosure());
            if let Some(file) = &self.consent {
                file.save(&ledger)?;
            }
        }
        Ok(TestStart::Started(self.client.start(&plan, &ledger)?))
    }

    /// Waits for a started sample and says whether it produced something of the right kind.
    pub fn finish(role: &Role, job: &Job, timeout: Duration) -> SampleReport {
        let started = Instant::now();
        let done = match job.wait(Duration::from_millis(500), timeout) {
            Ok(done) => done,
            Err(error) => {
                return SampleReport::Failed {
                    message: error.message,
                };
            }
        };
        let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match done.state {
            JobState::Succeeded => {}
            JobState::Failed { error } => {
                return SampleReport::Failed {
                    message: error.message,
                };
            }
            JobState::Cancelled => {
                return SampleReport::Failed {
                    message: "the test was cancelled".to_owned(),
                };
            }
            _ => {
                return SampleReport::Failed {
                    message: "the test did not finish".to_owned(),
                };
            }
        }
        let wanted = role.capability.modality().as_str().to_owned();
        let outputs: Vec<&Payload> = done
            .events
            .iter()
            .filter_map(|e| match e {
                Event::Output(o) => Some(&o.payload),
                _ => None,
            })
            .collect();
        if outputs.is_empty() {
            return SampleReport::Failed {
                message: "the model finished but returned nothing".to_owned(),
            };
        }
        let right_kind = outputs.iter().filter(|p| media_kind(p) == wanted).count();
        if right_kind == 0 {
            return SampleReport::Failed {
                message: format!("the model returned something other than {wanted} output"),
            };
        }
        SampleReport::Passed {
            summary: format!("Returned {right_kind} {wanted} result(s)"),
            millis,
        }
    }
}

fn media_kind(payload: &Payload) -> String {
    match payload {
        Payload::Inline { media_type, .. } => media_type.as_str(),
        Payload::Handle(h) => h.media_type.as_str(),
        Payload::Text { .. } => "text/plain",
    }
    .split('/')
    .next()
    .unwrap_or("")
    .to_owned()
}

fn set_default(store: &mut StoreFile, binding: Binding) {
    store.defaults.retain(|b| b.role != binding.role);
    store.defaults.push(binding);
}

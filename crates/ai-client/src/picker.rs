//! The data behind a tool-first model picker.
//!
//! A person chooses a tool, then a model for it. The models are listed under the provider that
//! serves them, each with one line saying what is known about it. Only models able to do the tool
//! are listed. A provider that is not set up is shown, greyed, with a way to set it up, so a
//! person can see what exists. A choice that has stopped working says why.
//!
//! This holds data only. The widget lives in `craft-ai-egui`; the app acts on what the person
//! picks (`ToolDesk::choose` for the whole suite, `AiClient::bind` for one app).

use craft_ai_core::{AiError, ConnectionId, Locality, ModelId, Pick, Role, RoleId};
use craft_ai_features::SetupOption;

use crate::client::{AiClient, RoleStatus};
use crate::picker_groups::{binding_for, connection_groups, model_label, unset_groups};
use crate::picker_presets::{price_unit, reason};

/// Where a provider stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GroupState {
    /// Its models can be picked now.
    Ready,
    /// This app has no adapter able to run this connection's backend.
    NeedsAdapter,
    /// Connected, but no key is saved for it.
    NeedsKey,
    /// Connected, but switched off.
    SwitchedOff,
    /// Not connected or installed yet. It has no models listed.
    NotSetUp,
}

/// How to get a provider into a state where its models can be picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupLink {
    /// Set up something new: the id of a `SetupOption` (`service:<preset>` or `local:<tool>`).
    New { option: String },
    /// Fix a connection that already exists (add its key, or switch it on).
    Fix { connection: ConnectionId },
}

/// One model under a provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerModel {
    pub connection: ConnectionId,
    pub model: ModelId,
    pub label: String,
    /// One line of what is known: where it runs, what it costs, what it takes.
    pub description: String,
    /// This is the model the app uses for the tool now.
    pub selected: bool,
}

/// A provider and its models.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerGroup {
    pub title: String,
    pub locality: Option<Locality>,
    pub state: GroupState,
    /// Why a greyed provider is greyed, in a sentence. Empty when it is ready.
    pub note: String,
    pub models: Vec<PickerModel>,
    /// Engine advice suggests this provider for this computer. Only set by
    /// [`AiClient::picker_with_advice`]; a suggestion, never a choice.
    pub suggested: bool,
    pub setup: Option<SetupLink>,
    /// What setting it up downloads, in bytes. Only a local model that is not installed has one.
    pub size_bytes: Option<u64>,
}

/// A model that earned a preset for the tool, with one line saying why. Listed above the full list
/// of models. Only a model on a provider that is ready is offered here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerPreset {
    pub pick: Pick,
    /// Why this model earned it: its reported price, its release date, or that it is our pick.
    pub reason: String,
    pub model: PickerModel,
}

/// The choice the app has now, which may no longer work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentChoice {
    pub connection: ConnectionId,
    pub model: ModelId,
    pub label: String,
    /// Set when the choice cannot run. Safe to show.
    pub problem: Option<String>,
}

/// Everything a picker window shows for one tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerView {
    pub tool: RoleId,
    pub title: String,
    pub current: Option<CurrentChoice>,
    /// The presets that anyone earned, in picker order. Empty when none was (no catalog entry
    /// matches and no provider reported a price or a date); the full list below still works.
    pub presets: Vec<PickerPreset>,
    pub groups: Vec<PickerGroup>,
}

impl AiClient {
    /// The picker for `role`. `options` is `craft_ai_features::setup_options` for the catalogue;
    /// the ones that unlock this tool become greyed providers. Reads the store and the key
    /// holder; sends nothing and never reads a key's value.
    pub fn picker(&self, role: &Role, options: &[SetupOption]) -> Result<PickerView, AiError> {
        let store = self.shared.load()?;
        store
            .check_tool(role)
            .map_err(|e| AiError::invalid(e.to_string()))?;
        let binding = binding_for(&store, self.app(), &role.id);
        let mut groups = connection_groups(self, &store, role, binding);
        groups.extend(unset_groups(&store, role, options));
        groups.sort_by_key(|g| g.state);
        let current = binding.map(|b| CurrentChoice {
            connection: b.connection.clone(),
            model: b.model.clone(),
            label: model_label(&store, b),
            problem: match self.status(role) {
                RoleStatus::Ready(_) => None,
                RoleStatus::Unavailable { reason } => Some(reason),
            },
        });
        let presets = self.presets_for(role, &groups)?;
        Ok(PickerView {
            tool: role.id.clone(),
            title: role.label.clone(),
            current,
            presets,
            groups,
        })
    }
}

impl AiClient {
    /// The presets for `role` that point at a model the picker lists as ready, each with its line.
    fn presets_for(
        &self,
        role: &Role,
        groups: &[PickerGroup],
    ) -> Result<Vec<PickerPreset>, AiError> {
        let unit = price_unit(role.capability.as_str());
        Ok(self
            .picks(role)?
            .into_iter()
            .filter_map(|picked| {
                let model = groups
                    .iter()
                    .filter(|g| g.state == GroupState::Ready)
                    .flat_map(|g| &g.models)
                    .find(|m| {
                        m.connection == picked.choice.connection && m.model == picked.choice.model
                    })?;
                Some(PickerPreset {
                    pick: picked.pick,
                    reason: reason(&picked, unit),
                    model: model.clone(),
                })
            })
            .collect())
    }
}

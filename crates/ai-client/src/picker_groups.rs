//! Building the provider groups for the picker: the connections a person has, and the providers
//! they could still set up. Pure functions over the store, so each rule has its own test.

use craft_ai_core::role::fits;
use craft_ai_core::{
    AppId, Auth, Billing, Binding, Connection, InputRole, Locality, ModelInfo, PRESETS, Role,
    RoleId, StoreFile,
};
use craft_ai_features::{Route, SetupOption};
use craft_ai_native::secrets::secret_for;

use crate::client::AiClient;
use crate::picker::{GroupState, PickerGroup, PickerModel, SetupLink};

/// The binding the app would use: its own choice first, then the launcher default.
pub(crate) fn binding_for<'a>(
    store: &'a StoreFile,
    app: &AppId,
    role: &RoleId,
) -> Option<&'a Binding> {
    store
        .apps
        .get(app)
        .and_then(|a| a.overrides.iter().find(|b| &b.role == role))
        .or_else(|| store.defaults.iter().find(|b| &b.role == role))
}

/// The model's name if its connection still lists it, else the id the binding holds.
pub(crate) fn model_label(store: &StoreFile, binding: &Binding) -> String {
    store
        .connections
        .iter()
        .find(|c| c.id == binding.connection)
        .and_then(|c| c.model(&binding.model))
        .map_or_else(|| binding.model.as_str().to_owned(), |m| m.label.clone())
}

/// One group per connection that has at least one model able to do the tool.
pub(crate) fn connection_groups(
    client: &AiClient,
    store: &StoreFile,
    role: &Role,
    selected: Option<&Binding>,
) -> Vec<PickerGroup> {
    let mut groups = Vec::new();
    for connection in &store.connections {
        let models: Vec<PickerModel> = connection
            .models
            .iter()
            .filter(|m| fits(role, m).is_some())
            .map(|m| PickerModel {
                connection: connection.id.clone(),
                model: m.id.clone(),
                label: m.label.clone(),
                description: describe(connection, m, role),
                selected: selected
                    .is_some_and(|b| b.connection == connection.id && b.model == m.id),
            })
            .collect();
        if models.is_empty() {
            continue;
        }
        let (state, note) = if !connection.enabled {
            (GroupState::SwitchedOff, "Switched off.".to_owned())
        } else if !client.has_adapter(&connection.provider) {
            (
                GroupState::NeedsAdapter,
                "This app can't use this option yet.".to_owned(),
            )
        } else if secret_for(&*client.secrets, connection).is_err() {
            let note = match connection.auth {
                Auth::Subscription { .. } => "Not signed in.",
                _ => "No key is saved for it.",
            };
            (GroupState::NeedsKey, note.to_owned())
        } else {
            (GroupState::Ready, String::new())
        };
        groups.push(PickerGroup {
            title: connection.label.clone(),
            locality: Some(connection.locality),
            state,
            note,
            models,
            suggested: false,
            setup: (state != GroupState::Ready && state != GroupState::NeedsAdapter).then(|| {
                SetupLink::Fix {
                    connection: connection.id.clone(),
                }
            }),
            size_bytes: None,
        });
    }
    groups
}

/// Providers that could do the tool and are not set up, greyed rather than hidden. A model that
/// can be downloaded but has no workflow to run it yet is not offered.
pub(crate) fn unset_groups(
    store: &StoreFile,
    role: &Role,
    options: &[SetupOption],
) -> Vec<PickerGroup> {
    let mut groups = Vec::new();
    for option in options
        .iter()
        .filter(|o| o.usable && o.unlocks.iter().any(|u| u == role.id.as_str()))
    {
        let (locality, note) = match &option.route {
            Route::Service { preset } => {
                let Some(preset) = PRESETS.iter().find(|p| p.key == preset) else {
                    continue;
                };
                if store
                    .connections
                    .iter()
                    .any(|c| c.provider == preset.provider)
                {
                    continue;
                }
                (None, format!("Not connected. {}", option.note))
            }
            Route::Local { .. } => {
                let have_local = store.connections.iter().any(|c| {
                    c.locality == Locality::Local
                        && c.models.iter().any(|m| fits(role, m).is_some())
                });
                if have_local {
                    continue;
                }
                (
                    Some(Locality::Local),
                    "Not installed. Runs on this computer.".to_owned(),
                )
            }
        };
        groups.push(PickerGroup {
            title: option.title.clone(),
            locality,
            state: GroupState::NotSetUp,
            note: note.trim_end().to_owned(),
            models: Vec::new(),
            suggested: false,
            setup: Some(SetupLink::New {
                option: option.id.clone(),
            }),
            size_bytes: option.size_bytes,
        });
    }
    groups
}

/// One line of what is known, never more: where it runs, what it costs and what it takes, all
/// from what the connection and the model declare.
fn describe(connection: &Connection, model: &ModelInfo, role: &Role) -> String {
    let place = match connection.locality {
        Locality::Local => "Runs on this computer",
        Locality::Remote => "Sent to the service",
    };
    let cost = match connection.billing {
        Billing::Free => Some("free"),
        Billing::Metered => Some("billed per use"),
        Billing::SubscriptionAllowance => Some("uses your subscription allowance"),
        Billing::Unknown => None,
    };
    let mut line = place.to_owned();
    if let Some(cost) = cost {
        line.push_str(", ");
        line.push_str(cost);
    }
    line.push('.');
    if let Some(spec) = model.spec_for(&role.capability) {
        let mut inputs: Vec<&InputRole> = spec.accepts.keys().collect();
        inputs.sort_by_key(|i| input_rank(i));
        let names: Vec<String> = inputs.into_iter().map(input_name).collect();
        if let Some(list) = join_and(&names) {
            line.push_str(" Takes ");
            line.push_str(&list);
            line.push('.');
        }
    }
    line
}

fn input_rank(role: &InputRole) -> u8 {
    match role {
        InputRole::Source => 0,
        InputRole::Mask => 1,
        InputRole::Reference => 2,
        InputRole::Prompt => 3,
        InputRole::System => 4,
        InputRole::Context => 5,
        InputRole::Other(_) => 6,
    }
}

fn input_name(role: &InputRole) -> String {
    match role {
        InputRole::Source => "a source image".to_owned(),
        InputRole::Mask => "a mask".to_owned(),
        InputRole::Reference => "a reference image".to_owned(),
        InputRole::Prompt => "a prompt".to_owned(),
        InputRole::System => "a system message".to_owned(),
        InputRole::Context => "context".to_owned(),
        InputRole::Other(other) => other.clone(),
    }
}

fn join_and(names: &[String]) -> Option<String> {
    match names {
        [] => None,
        [one] => Some(one.clone()),
        [init @ .., last] => Some(format!("{} and {last}", init.join(", "))),
    }
}

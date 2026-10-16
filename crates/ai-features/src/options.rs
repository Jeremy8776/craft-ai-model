//! What a person can set up, and which tools each thing switches on.
//!
//! The launcher's Set up AI screen and an app's "no model yet" popup both start from this list, so
//! they agree. It is derived from the features, never written twice: a hosted service unlocks every
//! tool whose hosted routes name it, and a local model unlocks the tool whose recipe it is.

use crate::feature::Feature;
use craft_ai_core::PRESETS;
use serde::{Deserialize, Serialize};

/// How a thing is set up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Route {
    /// Download model files for this feature and run them through ComfyUI on this computer.
    Local { feature: String },
    /// Connect a hosted service. `preset` is a key of `craft_ai_core::PRESETS`.
    Service { preset: String },
}

/// One thing to set up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupOption {
    /// Stable: `local:<feature>` or `service:<preset>`.
    pub id: String,
    pub title: String,
    /// One line about it. Empty for a local model, whose wording depends on the computer.
    pub note: String,
    pub route: Route,
    /// Ids of the features it switches on, in catalogue order.
    pub unlocks: Vec<String>,
    /// Apps can run it today. False for a model that can be downloaded but has no workflow to run it
    /// yet; the screen says so rather than promising a tool.
    pub usable: bool,
    /// How much a local model downloads, in bytes, so a popup can say so before it asks. `None`
    /// for a hosted service, which downloads nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

/// Every local model first, then hosted services in the order the features first name them.
pub fn setup_options(features: &[Feature]) -> Vec<SetupOption> {
    let mut locals = Vec::new();
    let mut services: Vec<SetupOption> = Vec::new();
    for feature in features {
        if let Some(recipe) = &feature.local {
            locals.push(SetupOption {
                id: format!("local:{}", feature.id),
                title: recipe.label.clone(),
                note: String::new(),
                route: Route::Local {
                    feature: feature.id.clone(),
                },
                unlocks: vec![feature.id.clone()],
                usable: recipe.usable,
                size_bytes: Some(recipe.total_bytes()),
            });
        }
        for option in &feature.cloud {
            let Some(preset) = PRESETS.iter().find(|p| p.key == option.preset) else {
                continue;
            };
            let id = format!("service:{}", preset.key);
            match services.iter_mut().find(|s| s.id == id) {
                Some(existing) => {
                    if !existing.unlocks.contains(&feature.id) {
                        existing.unlocks.push(feature.id.clone());
                    }
                }
                None => services.push(SetupOption {
                    id,
                    title: preset.name.to_owned(),
                    note: preset.note.to_owned(),
                    route: Route::Service {
                        preset: preset.key.to_owned(),
                    },
                    unlocks: vec![feature.id.clone()],
                    usable: true,
                    size_bytes: None,
                }),
            }
        }
    }
    locals.extend(services);
    locals
}

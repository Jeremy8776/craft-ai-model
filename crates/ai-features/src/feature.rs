//! The AI tools a person can switch on, and where they would appear.

use crate::recipe::{CloudOption, LocalRecipe};
use craft_ai_core::{CapabilityId, InputRole, Role, RoleId};
use serde::{Deserialize, Serialize};

/// Where a tool stands in one app. Nothing is `Shipped` until an app really calls the shared layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppStatus {
    /// Suggested from the app's description. The app's owner has not confirmed it.
    Proposed,
    /// The owner has agreed to it.
    Planned,
    /// The app calls the shared layer for it today.
    Shipped,
}

/// One place a feature shows up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppUse {
    pub app: String,
    pub tool: String,
    pub status: AppStatus,
}

/// A tool the user can enable once and use across the suite.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feature {
    pub id: String,
    pub title: String,
    pub summary: String,
    /// What a provider must advertise to run it.
    pub capability: CapabilityId,
    /// What the tool sends, so a model that cannot take these is not offered for it.
    #[serde(default)]
    pub inputs: Vec<InputRole>,
    pub uses: Vec<AppUse>,
    /// Run it on this computer, when a recipe has been worked out and checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalRecipe>,
    /// Hosted routes that have a working adapter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cloud: Vec<CloudOption>,
}

impl Feature {
    /// The role the launcher binds for this tool. Apps declare a role with the same id to inherit
    /// the launcher's choice; an app's own binding still wins.
    pub fn role(&self) -> Option<Role> {
        Some(Role {
            id: RoleId::new(&self.id).ok()?,
            label: self.title.clone(),
            capability: self.capability.clone(),
            inputs: self.inputs.clone(),
        })
    }

    /// Apps where the tool exists today.
    pub fn shipped_in(&self) -> impl Iterator<Item = &AppUse> {
        self.uses.iter().filter(|u| u.status == AppStatus::Shipped)
    }
}

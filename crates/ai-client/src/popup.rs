//! What an app's missing-model popup shows (task v3).
//!
//! An app already has a tool, such as Select subject, but no model is chosen for it, or the choice
//! stopped working. [`ToolDesk::missing_model`](crate::ToolDesk::missing_model) hands the app this
//! data: why the tool cannot run, and the picker with the real ways to fix it. The app draws it in
//! its own theme and its own window. Choosing a model binds it for the whole suite through
//! [`ToolDesk::choose`](crate::ToolDesk::choose); a set-up link goes through the same shared store
//! or opens the launcher.

use craft_ai_model::RoleId;

use crate::picker::PickerView;

/// A tool that cannot run, with what the person can do about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingModel {
    pub tool: RoleId,
    /// The tool's name, such as "Select subject".
    pub title: String,
    /// Why it cannot run, in a sentence. Safe to show.
    pub reason: String,
    /// The models that could do it and the providers that could be set up for it.
    pub picker: PickerView,
}

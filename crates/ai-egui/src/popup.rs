//! The in-app missing-model popup (task v3), as a neutral fallback.
//!
//! An app that already has a tool but no model for it shows this, or draws its own over the same
//! [`MissingModel`] data in its own theme. It reports what the person asked for; the app binds a
//! choice with `ToolDesk::choose`, which makes it the choice for every app, and acts on a set-up
//! link by opening the launcher or its own connection panel.

use craft_ai_client::{MissingModel, SetupLink};
use craft_ai_model::{ConnectionId, ModelId};
use egui::Ui;

use crate::picker::{PickerAction, provider_list};

/// Follows the tool's name in the heading: "Select subject needs a model".
pub const HEADING_SUFFIX: &str = "needs a model";
/// Button that closes the popup without choosing.
pub const NOT_NOW: &str = "Not now";

/// What the person asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PopupAction {
    /// Use this model for the tool, for every app.
    Choose {
        connection: ConnectionId,
        model: ModelId,
    },
    /// Set something up, or fix a connection.
    SetUp(SetupLink),
    /// Close the popup and leave the tool off for now.
    NotNow,
}

/// Draws the popup content for one tool.
pub fn missing_model_popup(ui: &mut Ui, popup: &MissingModel) -> Option<PopupAction> {
    let mut action = None;
    ui.vertical(|ui| {
        ui.heading(format!("{} {HEADING_SUFFIX}", popup.title));
        ui.colored_label(ui.visuals().warn_fg_color, &popup.reason);
        ui.add_space(8.0);
        match provider_list(ui, &popup.picker) {
            Some(PickerAction::Choose { connection, model }) => {
                action = Some(PopupAction::Choose { connection, model });
            }
            Some(PickerAction::SetUp(link)) => action = Some(PopupAction::SetUp(link)),
            None => {}
        }
        ui.add_space(8.0);
        if ui.button(NOT_NOW).clicked() {
            action = Some(PopupAction::NotNow);
        }
    });
    action
}

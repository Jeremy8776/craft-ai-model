//! The in-app missing-model popup, as a neutral fallback.
//!
//! An app that already has a tool but no model for it shows this, or draws its own over the same
//! [`MissingModel`] data in its own theme. It reports what the person asked for; the app binds a
//! choice with `ToolDesk::choose`, which makes it the choice for every app, and acts on a set-up
//! link by opening the launcher or its own connection panel.

use craft_ai_client::{MissingModel, SetupLink};
use craft_ai_core::{ConnectionId, ModelId};
use craft_ai_features::{PortableBuild, SetupOffer, SetupRoute, gigabytes};
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
    /// The person approved downloading just ComfyUI, not the model or add-ons.
    InstallComfyUi(PortableBuild),
}

/// State of the optional install section, owned by the host window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PopupInstallState {
    pub open: bool,
    pub accepted: bool,
    selected_asset: Option<String>,
}

/// Draws the popup with an optional ComfyUI install offer. This only reports a choice;
/// the host must start the install in a background task after receiving the action.
pub fn missing_model_popup_with_install(
    ui: &mut Ui,
    popup: &MissingModel,
    offer: Option<&SetupOffer>,
    install: &mut PopupInstallState,
) -> Option<PopupAction> {
    let action = missing_model_popup(ui, popup);
    let Some(offer) =
        offer.filter(|o| o.feature == popup.tool.as_str() && o.route == SetupRoute::ComfyUi)
    else {
        install.open = false;
        install.accepted = false;
        install.selected_asset = None;
        return action;
    };
    let Some(build) = offer.portable.as_ref().filter(|_| offer.can_install()) else {
        for step in &offer.manual_steps {
            ui.label(step);
        }
        for reason in &offer.blockers {
            ui.colored_label(ui.visuals().warn_fg_color, reason);
        }
        return action;
    };
    if install.selected_asset.as_deref() != Some(&build.asset) {
        install.open = false;
        install.accepted = false;
        install.selected_asset = Some(build.asset.clone());
    }
    ui.separator();
    if ui.button("Install ComfyUI on this computer").clicked() {
        install.open = !install.open;
        install.accepted = false;
    }
    if install.open {
        ui.label(format!(
            "Download {} from the ComfyUI authors.",
            gigabytes(build.size_bytes)
        ));
        ui.label("This installs ComfyUI only. The model and its add-ons are separate.");
        ui.hyperlink_to(
            "Read ComfyUI's licence (GPL-3.0)",
            "https://github.com/Comfy-Org/ComfyUI/blob/v0.39.0/LICENSE",
        );
        ui.label("A copy is kept in this app's folder. It can be removed later.");
        ui.checkbox(
            &mut install.accepted,
            "I agree to install ComfyUI from its authors",
        );
        if ui
            .add_enabled(install.accepted, egui::Button::new("Install ComfyUI"))
            .clicked()
        {
            install.open = false;
            install.accepted = false;
            return Some(PopupAction::InstallComfyUi(build.clone()));
        }
    }
    action
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

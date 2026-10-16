//! The compact status and model picker: is this feature usable, which model does it use,
//! and a way to change that or open the launcher.
//!
//! The widget reports what the user asked for; the app acts on it (`AiClient::bind`,
//! `clear_binding`, launching the ArtCraft Launcher, or showing the [`ConnectionPanel`]
//! (crate::ConnectionPanel) when the app offers to add a connection itself). Connections are
//! normally entered once, in the launcher; the panel writes to the same store, so nothing differs
//! afterwards.

use craft_ai_client::{Choice, RoleStatus};
use craft_ai_core::{ConnectionId, ModelId};
use egui::{ComboBox, Ui};

use crate::text;

/// What the user asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusAction {
    /// Open the ArtCraft Launcher so the user can add or fix a connection.
    OpenLauncher,
    /// Use this model for this app's feature.
    Pick {
        connection: ConnectionId,
        model: ModelId,
    },
    /// Drop this app's own choice and follow the launcher default again.
    UseDefault,
    /// Show the add-connection panel (only offered when the view says `can_add_here`).
    AddConnection,
}

/// Label of the launcher button.
pub const OPEN_LAUNCHER: &str = "Open Launcher";
/// Label of the button that drops the app's own choice.
pub const USE_DEFAULT: &str = "Use default";
/// Label of the button that adds a connection without the launcher.
pub const ADD_CONNECTION: &str = "Add a connection here";

/// Everything the widget shows.
pub struct StatusView<'a> {
    /// The feature's name, such as "Generative Fill".
    pub label: &'a str,
    pub status: &'a RoleStatus,
    /// The models that could fill the role ([`AiClient::choices`](craft_ai_client::AiClient)).
    pub choices: &'a [Choice],
    /// Whether this app has its own choice overriding the launcher default.
    pub has_override: bool,
    /// The app can show the add-connection panel. For machines with no launcher (a standalone
    /// install, a Flatpak, a headless box); offered next to *Open Launcher*, never instead of it.
    pub can_add_here: bool,
}

fn choice_text(c: &Choice) -> String {
    format!(
        "{} \u{2014} {} ({})",
        c.model_label,
        c.connection_label,
        text::locality_badge(c.locality).to_lowercase()
    )
}

/// Draws the status line, the picker when there is something to pick, and the buttons.
pub fn ai_status(ui: &mut Ui, view: &StatusView<'_>) -> Option<StatusAction> {
    let mut action = None;
    ui.vertical(|ui| {
        ui.label(text::status_line(view.label, view.status));
        ui.horizontal(|ui| {
            if matches!(view.status, RoleStatus::Unavailable { .. })
                && ui.button(OPEN_LAUNCHER).clicked()
            {
                action = Some(StatusAction::OpenLauncher);
            }
            if matches!(view.status, RoleStatus::Unavailable { .. })
                && view.can_add_here
                && ui.button(ADD_CONNECTION).clicked()
            {
                action = Some(StatusAction::AddConnection);
            }
            if !view.choices.is_empty() {
                let current = match view.status {
                    RoleStatus::Ready(r) => Some((&r.connection, &r.model)),
                    RoleStatus::Unavailable { .. } => None,
                };
                let selected = view
                    .choices
                    .iter()
                    .find(|c| current == Some((&c.connection, &c.model)))
                    .map_or_else(|| "Choose a model".to_owned(), choice_text);
                ComboBox::from_id_salt(("craft-ai-core-picker", view.label))
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for c in view.choices {
                            let is_current = current == Some((&c.connection, &c.model));
                            if ui.selectable_label(is_current, choice_text(c)).clicked()
                                && !is_current
                            {
                                action = Some(StatusAction::Pick {
                                    connection: c.connection.clone(),
                                    model: c.model.clone(),
                                });
                            }
                        }
                    });
            }
            if view.has_override && ui.button(USE_DEFAULT).clicked() {
                action = Some(StatusAction::UseDefault);
            }
        });
    });
    action
}

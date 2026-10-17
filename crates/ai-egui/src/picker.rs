//! The model picker: a tool's models listed under the provider that serves them.
//!
//! Theme neutral: it draws with the host's own visuals and takes no colours of its own beyond the
//! host's warning colour. The data comes from `AiClient::picker`; the widget only reports what the
//! person asked for and the host acts on it (`ToolDesk::choose` to use a model across the suite,
//! `AiClient::bind` for one app, and opening the launcher or the add-connection panel for a
//! set-up link). A greyed provider is never hidden and never selectable: its models are shown so a
//! person can see what setting it up would give them.

use craft_ai_client::{
    GroupState, PickerGroup, PickerModel, PickerPreset, PickerView, SetupLink, pick_label,
};
use craft_ai_core::{ConnectionId, ModelId};
use egui::text::LayoutJob;
use egui::{Align, FontSelection, Layout, RichText, ScrollArea, TextFormat, Ui};

use crate::text;

/// Heading of the picker.
pub const HEADING: &str = "Choose a model";
/// Button on a provider that is not set up.
pub const SET_UP: &str = "Set up";
/// Button on a connection with no key saved.
pub const ADD_KEY: &str = "Add key";
/// Button on a connection that is switched off.
pub const SWITCH_ON: &str = "Switch on";
/// Shown when no model and no provider can do the tool.
pub const NOTHING: &str = "Nothing here can do this tool yet.";
/// Heading over the presets, when any model earned one.
pub const SUGGESTED: &str = "Suggested";
/// Heading over the full list, when presets are shown above it.
pub const ALL_MODELS: &str = "All models";
/// Added to the model the app uses now.
pub const IN_USE: &str = "in use";
/// Tallest the list gets before it scrolls. Fixed on purpose: a list that takes the space left
/// over makes a centred modal grow every frame.
const LIST_HEIGHT: f32 = 380.0;

/// What the person asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickerAction {
    /// Use this model for the tool.
    Choose {
        connection: ConnectionId,
        model: ModelId,
    },
    /// Set something up, or fix a connection. The host opens the launcher or its own panel.
    SetUp(SetupLink),
}

/// Draws the picker for one tool.
pub fn model_picker(ui: &mut Ui, view: &PickerView) -> Option<PickerAction> {
    let mut action = None;
    ui.vertical(|ui| {
        ui.heading(HEADING);
        ui.label(RichText::new(format!("for {}", view.title)).weak());
        if let Some(current) = &view.current
            && let Some(problem) = &current.problem
        {
            ui.add_space(4.0);
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Your choice, {}, is not working: {problem}", current.label),
            );
        }
        ui.add_space(6.0);
        action = provider_list(ui, view);
    });
    action
}

/// The providers and their models, without a heading. The popup draws its own around this.
pub(crate) fn provider_list(ui: &mut Ui, view: &PickerView) -> Option<PickerAction> {
    if view.groups.is_empty() {
        ui.label(NOTHING);
        return None;
    }
    let mut action = None;
    ScrollArea::vertical()
        .id_salt(("craft-ai-picker", view.tool.as_str()))
        .max_height(LIST_HEIGHT)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if !view.presets.is_empty() {
                ui.label(RichText::new(SUGGESTED).strong());
                for preset in &view.presets {
                    if let Some(found) = preset_ui(ui, preset) {
                        action = Some(found);
                    }
                }
                ui.add_space(8.0);
                ui.label(RichText::new(ALL_MODELS).strong());
            }
            for group in &view.groups {
                if let Some(found) = group_ui(ui, group) {
                    action = Some(found);
                }
                ui.add_space(8.0);
            }
        });
    action
}

/// A preset: its name and why, then the model, which is picked like any other row.
fn preset_ui(ui: &mut Ui, preset: &PickerPreset) -> Option<PickerAction> {
    ui.horizontal(|ui| {
        ui.label(RichText::new(pick_label(preset.pick)).small().strong());
        ui.label(RichText::new(&preset.reason).small().weak());
    });
    model_row(ui, &preset.model)
}

fn setup_label(group: &PickerGroup) -> Option<&'static str> {
    match (&group.setup, group.state) {
        (Some(SetupLink::New { .. }), _) => Some(SET_UP),
        (Some(SetupLink::Fix { .. }), GroupState::NeedsKey) => Some(ADD_KEY),
        (Some(SetupLink::Fix { .. }), _) => Some(SWITCH_ON),
        (None, _) => None,
    }
}

fn group_ui(ui: &mut Ui, group: &PickerGroup) -> Option<PickerAction> {
    let mut action = None;
    let ready = group.state == GroupState::Ready;
    ui.horizontal(|ui| {
        let title = RichText::new(&group.title).strong();
        ui.label(if ready { title } else { title.weak() });
        if let Some(locality) = group.locality {
            ui.label(RichText::new(text::locality_badge(locality)).small().weak());
        }
        if group.suggested {
            ui.label(
                RichText::new("Suggested for this computer")
                    .small()
                    .strong(),
            );
        }
        if let Some(size) = group.size_bytes {
            ui.label(
                RichText::new(format!("Download {}", text::bytes(size)))
                    .small()
                    .weak(),
            );
        }
        if let (Some(link), Some(label)) = (&group.setup, setup_label(group))
            && ui.button(label).clicked()
        {
            action = Some(PickerAction::SetUp(link.clone()));
        }
    });
    if !group.note.is_empty() {
        ui.label(RichText::new(&group.note).small().weak());
    }
    // Models of a provider that cannot run yet are shown, but cannot be picked.
    ui.add_enabled_ui(ready, |ui| {
        for model in &group.models {
            if let Some(found) = model_row(ui, model) {
                action = Some(found);
            }
        }
    });
    action
}

fn model_row(ui: &mut Ui, model: &PickerModel) -> Option<PickerAction> {
    let style = ui.style().clone();
    let mut job = LayoutJob::default();
    let mut name = model.label.clone();
    if model.selected {
        name.push_str(&format!("  ({IN_USE})"));
    }
    job.append(
        &name,
        0.0,
        TextFormat::simple(
            FontSelection::Default.resolve(&style),
            ui.visuals().strong_text_color(),
        ),
    );
    job.append(
        &format!("\n{}", model.description),
        0.0,
        TextFormat::simple(
            egui::TextStyle::Small.resolve(&style),
            ui.visuals().weak_text_color(),
        ),
    );
    let clicked = ui
        .with_layout(Layout::top_down_justified(Align::LEFT), |ui| {
            ui.selectable_label(model.selected, job).clicked()
        })
        .inner;
    (clicked && !model.selected).then(|| PickerAction::Choose {
        connection: model.connection.clone(),
        model: model.model.clone(),
    })
}

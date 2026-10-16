//! The add-connection form (the fallback for machines with no launcher): same rules as the
//! launcher's screen, because both edit a [`ConnectionDraft`].
//!
//! It draws and reports; [`ConnectionPanel`](crate::ConnectionPanel) is the version that also does
//! the saving and testing, which most apps want. The sentences are in [`words`] so an app with its
//! own look can reuse them.

use craft_ai_core::{Connection, ConnectionDraft, DraftError, Field, Locality, PRESETS};
use egui::{ComboBox, RichText, TextEdit, Ui};

/// The form's sentences.
pub mod words {
    use craft_ai_core::Locality;

    pub const TITLE_ADD: &str = "Add a connection";
    pub const TITLE_EDIT: &str = "Edit connection";
    pub const START_FROM: &str = "Start from";
    pub const NAME: &str = "Name";
    pub const ADDRESS: &str = "Address";
    pub const USES_KEY: &str = "Uses a key";
    pub const KEY: &str = "Key";
    pub const KEY_HINT_NEW: &str = "Paste the key here";
    pub const KEY_HINT_KEEP: &str = "Leave empty to keep the saved key";
    pub const SAVE: &str = "Save";
    pub const TEST: &str = "Test";
    pub const CANCEL: &str = "Cancel";

    /// Where data goes, from the address typed so far.
    pub fn where_it_goes(l: Locality) -> &'static str {
        match l {
            Locality::Local => "Data sent to this connection stays on this computer.",
            Locality::Remote => "Data sent to this connection leaves this computer.",
        }
    }

    /// The test result line, nothing while idle.
    pub fn test_line(state: &super::TestLine) -> Option<String> {
        match state {
            super::TestLine::Idle => None,
            super::TestLine::Running => Some("Testing\u{2026}".to_owned()),
            super::TestLine::Passed { models, millis } => {
                Some(format!("Connected. Models found: {models} ({millis} ms)"))
            }
            super::TestLine::Failed(why) => Some(format!("Test failed: {why}")),
            super::TestLine::Unverified(why) => Some(format!("Not verified: {why}")),
        }
    }
}

/// How the last test went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestLine {
    Idle,
    Running,
    Passed { models: usize, millis: u64 },
    Unverified(String),
    Failed(String),
}

/// The form's state.
pub struct ConnectionFormState {
    pub draft: ConnectionDraft,
    pub error: Option<DraftError>,
    pub test: TestLine,
    preset: Option<&'static str>,
}

impl ConnectionFormState {
    /// A blank form for a new connection.
    pub fn new() -> Self {
        Self::from_preset(0)
    }

    fn from_preset(index: usize) -> Self {
        let preset = PRESETS.get(index);
        Self {
            draft: preset.map(ConnectionDraft::from_preset).unwrap_or_default(),
            error: None,
            test: TestLine::Idle,
            preset: preset.map(|p| p.key),
        }
    }

    fn switch_preset(&mut self, index: usize) {
        // A pasted key belongs to one provider, never carry it across presets.
        *self = Self::from_preset(index);
    }

    /// A form for changing a saved connection. The key box starts empty.
    pub fn edit(connection: &Connection) -> Self {
        Self {
            draft: ConnectionDraft::from_connection(connection),
            error: None,
            test: TestLine::Idle,
            preset: None,
        }
    }
}

impl Default for ConnectionFormState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn switching_provider_discards_the_previous_api_key() {
        let mut form = ConnectionFormState::new();
        form.draft.key = "provider-a-secret".into();
        let bfl = PRESETS.iter().position(|p| p.key == "bfl").unwrap();
        form.switch_preset(bfl);
        assert!(form.draft.key.is_empty());
        assert_eq!(form.draft.provider, "bfl");
    }
}

/// What the person asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormEvent {
    Save,
    Test,
    Cancel,
}

/// Draws the form. `testing` greys out Test while one is running.
pub fn connection_form(
    ui: &mut Ui,
    state: &mut ConnectionFormState,
    testing: bool,
) -> Option<FormEvent> {
    let mut event = None;
    let adding = state.draft.editing.is_none();
    ui.vertical(|ui| {
        ui.heading(if adding {
            words::TITLE_ADD
        } else {
            words::TITLE_EDIT
        });
        if adding {
            let mut picked = None;
            ui.horizontal(|ui| {
                ui.label(words::START_FROM);
                let current = PRESETS
                    .iter()
                    .find(|p| state.preset == Some(p.key))
                    .map_or("", |p| p.name);
                ComboBox::from_id_salt("craft-ai-preset")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (i, p) in PRESETS.iter().enumerate() {
                            if ui
                                .selectable_label(state.preset == Some(p.key), p.name)
                                .clicked()
                            {
                                picked = Some(i);
                            }
                        }
                    });
            });
            if let Some(note) = PRESETS
                .iter()
                .find(|p| state.preset == Some(p.key))
                .map(|p| p.note)
            {
                ui.label(RichText::new(note).small().weak());
            }
            if let Some(i) = picked {
                state.switch_preset(i);
            }
        }

        let bad = |field: Field| state.error.as_ref().is_some_and(|e| e.field == field);
        let heading = |ui: &mut Ui, text: &str, field: Field| {
            let t = RichText::new(text);
            ui.label(if bad(field) {
                t.color(ui.visuals().error_fg_color)
            } else {
                t
            });
        };

        heading(ui, words::NAME, Field::Label);
        ui.add(TextEdit::singleline(&mut state.draft.label).desired_width(f32::INFINITY));
        heading(ui, words::ADDRESS, Field::Address);
        let moved = ui
            .add(
                TextEdit::singleline(&mut state.draft.endpoint)
                    .desired_width(f32::INFINITY)
                    .hint_text("https://\u{2026}"),
            )
            .changed();
        let mut edited = moved;
        if let Some(l) = state.draft.locality() {
            let colour = match l {
                Locality::Local => ui.visuals().text_color(),
                Locality::Remote => ui.visuals().warn_fg_color,
            };
            ui.label(RichText::new(words::where_it_goes(l)).small().color(colour));
        }
        edited |= ui
            .checkbox(&mut state.draft.uses_key, words::USES_KEY)
            .changed();
        if state.draft.uses_key {
            heading(ui, words::KEY, Field::Key);
            let hint = if adding {
                words::KEY_HINT_NEW
            } else {
                words::KEY_HINT_KEEP
            };
            edited |= ui
                .add(
                    TextEdit::singleline(&mut state.draft.key)
                        .password(true)
                        .desired_width(f32::INFINITY)
                        .hint_text(hint),
                )
                .changed();
        }
        // An earlier test, and the models it found, no longer describe what is typed.
        if moved {
            state.draft.models.clear();
        }
        if edited
            && matches!(
                state.test,
                TestLine::Passed { .. } | TestLine::Unverified(_) | TestLine::Failed(_)
            )
        {
            state.test = TestLine::Idle;
        }

        if let Some(e) = &state.error {
            ui.colored_label(ui.visuals().error_fg_color, &e.message);
        }
        if let Some(line) = words::test_line(&state.test) {
            let colour = if matches!(state.test, TestLine::Failed(_)) {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().text_color()
            };
            ui.label(RichText::new(line).small().color(colour));
        }
        ui.horizontal(|ui| {
            if ui.button(words::SAVE).clicked() {
                event = Some(FormEvent::Save);
            }
            ui.add_enabled_ui(!testing, |ui| {
                if ui.button(words::TEST).clicked() {
                    event = Some(FormEvent::Test);
                }
            });
            if ui.button(words::CANCEL).clicked() {
                event = Some(FormEvent::Cancel);
            }
        });
    });
    event
}

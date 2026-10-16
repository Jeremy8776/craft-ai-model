//! Job progress with cancel and a cost line. Works for a two-second image edit (a spinner
//! with no percentage) and a two-minute video job (a bar), and shows the final state afterwards.

use craft_ai_core::{CostEstimate, JobState};
use egui::{ProgressBar, Ui};

use crate::text;

/// What the user asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressAction {
    Cancel,
}

/// Label of the cancel button.
pub const CANCEL: &str = "Cancel";

/// Draws the job's state. Returns [`ProgressAction::Cancel`] on the frame the button is pressed;
/// the button only appears while the job can still be cancelled.
pub fn job_progress(
    ui: &mut Ui,
    state: &JobState,
    cost: Option<&CostEstimate>,
) -> Option<ProgressAction> {
    let mut action = None;
    ui.vertical(|ui| {
        let label = text::state_label(state);
        ui.horizontal(|ui| {
            match text::progress_fraction(state) {
                Some(f) => {
                    // Leave room for the Cancel button; a bar left to fill the row would push it
                    // out of sight.
                    let width = (ui.available_width() - 90.0).max(80.0);
                    ui.add(
                        ProgressBar::new(f)
                            .text(label.clone())
                            .desired_width(width)
                            .animate(false),
                    );
                }
                None => {
                    if !state.is_terminal() {
                        ui.spinner();
                    }
                    ui.label(label.clone());
                }
            }
            if !state.is_terminal() && ui.button(CANCEL).clicked() {
                action = Some(ProgressAction::Cancel);
            }
        });
        if let Some(c) = cost {
            ui.label(text::cost_text(c));
        }
    });
    action
}

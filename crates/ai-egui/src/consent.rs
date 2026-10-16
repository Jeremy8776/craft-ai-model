//! The consent prompt: what is about to leave the machine, and a clear yes or no.
//!
//! The widget only reports the choice. The app records agreement in its `ConsentLedger` (and saves
//! it) when the choice is [`ConsentChoice::Allow`], and does nothing when it is `Deny`.
//! `AiClient::start` re-checks the ledger regardless, so a prompt that is skipped sends nothing.

use craft_ai_core::{Disclosure, Locality};
use egui::{RichText, Ui};

use crate::text;

/// What the user chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsentChoice {
    Allow,
    Deny,
}

/// Label of the button that agrees.
pub const ALLOW: &str = "Allow";
/// Label of the button that refuses.
pub const DENY: &str = "Don't send";

/// Shows the disclosure with a local or remote badge. Returns the choice on the frame it is made.
pub fn consent_prompt(
    ui: &mut Ui,
    disclosure: &Disclosure,
    app_label: &str,
) -> Option<ConsentChoice> {
    let mut choice = None;
    ui.vertical(|ui| {
        let badge = RichText::new(text::locality_badge(disclosure.locality)).strong();
        let badge = match disclosure.locality {
            Locality::Remote => badge.color(ui.visuals().warn_fg_color),
            Locality::Local => badge,
        };
        ui.label(badge);
        ui.label(text::consent_headline(disclosure, app_label));
        for line in text::disclosure_lines(disclosure) {
            ui.label(format!("\u{2022} {line}"));
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button(ALLOW).clicked() {
                choice = Some(ConsentChoice::Allow);
            }
            if ui.button(DENY).clicked() {
                choice = Some(ConsentChoice::Deny);
            }
        });
    });
    choice
}

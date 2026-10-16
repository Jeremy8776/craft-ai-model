//! Shared egui widgets for the Craft AI layer.
//!
//! Widgets that every egui Craft app mounts the same way, so the AI experience is identical
//! across the suite:
//!
//! | Widget | Reports |
//! |---|---|
//! | [`ai_status`] | open the launcher, pick a model, use the default |
//! | [`consent_prompt`] | allow or don't send |
//! | [`job_progress`] | cancel |
//! | [`ConnectionPanel`] | saved, closed |
//! | [`history_view`] | refresh, clear, CSV export |
//!
//! They draw and report; the app acts, through [`AiClient`](craft_ai_client::AiClient). The wording
//! is in [`text`] as plain functions, so an app with its own UI can reuse the same sentences. Colours
//! come from the active egui theme, none are hard-coded.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

mod consent;
mod form;
mod history;
pub mod history_text;
mod panel;
mod picker;
mod popup;
mod progress;
mod status;
pub mod text;

pub use consent::{ALLOW, ConsentChoice, DENY, consent_prompt};
pub use form::{ConnectionFormState, FormEvent, TestLine, connection_form, words};
pub use history::{
    CLEAR, CLEAR_NO, CLEAR_YES, EMPTY, EXPORT, HistoryAction, HistoryViewState, NOTHING_MATCHES,
    REFRESH, Range, TAB_DAYS, TAB_JOBS, TAB_MONTHS, Tab, history_view,
};
pub use panel::{ConnectionPanel, PanelEvent};
pub use picker::{
    ADD_KEY, HEADING, IN_USE, NOTHING, PickerAction, SET_UP, SWITCH_ON, model_picker,
};
pub use popup::{
    HEADING_SUFFIX, NOT_NOW, PopupAction, PopupInstallState, missing_model_popup,
    missing_model_popup_with_install,
};
pub use progress::{CANCEL, ProgressAction, job_progress};
pub use status::{ADD_CONNECTION, OPEN_LAUNCHER, StatusAction, StatusView, USE_DEFAULT, ai_status};

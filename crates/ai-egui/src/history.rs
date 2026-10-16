//! The request history view: every AI job the apps ran, with filters, totals and CSV export.
//!
//! Like the other widgets it draws and reports; the app acts. It never reads or deletes the log
//! itself, so it works in the launcher (all apps) and inside one app (filtered to that app) alike:
//!
//! ```ignore
//! // keep a HistoryViewState and the last HistoryLog the client returned
//! match history_view(ui, &mut state, &log, now_ms) {
//!     Some(HistoryAction::Refresh) => log = client.history()?,
//!     Some(HistoryAction::Clear) => { client.clear_history()?; log = client.history()?; }
//!     Some(HistoryAction::Export(csv)) => save_file_dialog_then_write(csv),
//!     None => {}
//! }
//! ```
//!
//! Money shown is only what providers reported. See [`craft_ai_core::history::report`].

use craft_ai_core::HistoryLog;
use craft_ai_core::history::report::{
    HistoryFilter, Totals, apps, by_day, by_month, models, start_of_day_ms, to_csv, totals,
};
use egui::{ComboBox, ScrollArea, Ui};

use crate::history_text as words;

pub const REFRESH: &str = "Refresh";
pub const EXPORT: &str = "Export CSV";
pub const CLEAR: &str = "Clear history";
pub const CLEAR_YES: &str = "Yes, clear it";
pub const CLEAR_NO: &str = "Keep it";
pub const TAB_JOBS: &str = "Jobs";
pub const TAB_DAYS: &str = "By day";
pub const TAB_MONTHS: &str = "By month";
pub const EMPTY: &str = "No AI jobs have been run yet.";
pub const NOTHING_MATCHES: &str = "No jobs match these filters.";

const MS_PER_DAY: u64 = 86_400_000;
const ROW_HEIGHT: f32 = 20.0;
const LIST_HEIGHT: f32 = 320.0;

/// How far back to look.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Range {
    #[default]
    All,
    Today,
    Week,
    Month,
}

impl Range {
    const ALL: [Range; 4] = [Range::All, Range::Today, Range::Week, Range::Month];

    pub fn label(self) -> &'static str {
        match self {
            Range::All => "All time",
            Range::Today => "Today",
            Range::Week => "Last 7 days",
            Range::Month => "Last 30 days",
        }
    }

    fn since(self, now_ms: u64, offset_minutes: i32) -> Option<u64> {
        match self {
            Range::All => None,
            Range::Today => Some(start_of_day_ms(now_ms, offset_minutes)),
            Range::Week => Some(now_ms.saturating_sub(7 * MS_PER_DAY)),
            Range::Month => Some(now_ms.saturating_sub(30 * MS_PER_DAY)),
        }
    }
}

/// Which table to show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Jobs,
    Days,
    Months,
}

/// What the user asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryAction {
    /// Read the log again.
    Refresh,
    /// The user confirmed deleting the history for every app.
    Clear,
    /// CSV text of what is currently shown, for the app to save where the user chooses.
    Export(String),
}

/// What the view remembers between frames.
#[derive(Clone, Debug, Default)]
pub struct HistoryViewState {
    pub app: Option<String>,
    pub model: Option<String>,
    pub range: Range,
    pub tab: Tab,
    /// Offset from UTC in minutes used for dates and "today". 0 means UTC; an app that knows the
    /// local offset sets it.
    pub offset_minutes: i32,
    confirming_clear: bool,
}

impl HistoryViewState {
    /// A view fixed to one app, for an app showing its own entries.
    pub fn for_app(app: impl Into<String>) -> Self {
        Self {
            app: Some(app.into()),
            ..Self::default()
        }
    }

    /// Sets the offset from UTC (minutes, east positive) used for dates and "today".
    pub fn with_offset(mut self, offset_minutes: i32) -> Self {
        self.offset_minutes = offset_minutes;
        self
    }

    fn filter(&self, now_ms: u64) -> HistoryFilter {
        HistoryFilter {
            app: self.app.clone(),
            model: self.model.clone(),
            since_ms: self.range.since(now_ms, self.offset_minutes),
        }
    }
}

fn pick(ui: &mut Ui, id: &str, all: &str, options: &[String], current: &mut Option<String>) {
    // A filter left on a value that has since left the log would hide everything; fall back.
    if current.as_ref().is_some_and(|c| !options.contains(c)) {
        *current = None;
    }
    let shown = current.clone().unwrap_or_else(|| all.to_owned());
    ComboBox::from_id_salt(id)
        .selected_text(shown)
        .show_ui(ui, |ui| {
            ui.selectable_value(current, None, all);
            for option in options {
                ui.selectable_value(current, Some(option.clone()), option);
            }
        });
}

/// Draws the history. `now_ms` is the current Unix time in milliseconds, passed in so the view
/// does no clock reads of its own.
pub fn history_view(
    ui: &mut Ui,
    state: &mut HistoryViewState,
    log: &HistoryLog,
    now_ms: u64,
) -> Option<HistoryAction> {
    let mut action = None;
    let app_options = apps(&log.entries);
    let model_options = models(&log.entries);
    ui.horizontal_wrapped(|ui| {
        pick(ui, "history-app", "All apps", &app_options, &mut state.app);
        pick(
            ui,
            "history-model",
            "All models",
            &model_options,
            &mut state.model,
        );
        ComboBox::from_id_salt("history-range")
            .selected_text(state.range.label())
            .show_ui(ui, |ui| {
                for range in Range::ALL {
                    ui.selectable_value(&mut state.range, range, range.label());
                }
            });
        if ui.button(REFRESH).clicked() {
            action = Some(HistoryAction::Refresh);
        }
    });

    let filter = state.filter(now_ms);
    let shown = filter.apply(&log.entries);

    ui.horizontal(|ui| {
        for (tab, label) in [
            (Tab::Jobs, TAB_JOBS),
            (Tab::Days, TAB_DAYS),
            (Tab::Months, TAB_MONTHS),
        ] {
            ui.selectable_value(&mut state.tab, tab, label);
        }
    });

    if log.entries.is_empty() {
        ui.label(EMPTY);
    } else if shown.is_empty() {
        ui.label(NOTHING_MATCHES);
    } else {
        let all = totals(shown.iter().copied());
        ui.label(words::counts_line(&all));
        ui.label(words::cost_line(&all));
        ui.add_space(4.0);
        match state.tab {
            Tab::Jobs => {
                ScrollArea::vertical()
                    .max_height(LIST_HEIGHT)
                    .auto_shrink([false, true])
                    .show_rows(ui, ROW_HEIGHT, shown.len(), |ui, range| {
                        for entry in shown.iter().skip(range.start).take(range.len()) {
                            ui.add(
                                egui::Label::new(words::entry_line(entry, state.offset_minutes))
                                    .truncate(),
                            );
                        }
                    });
            }
            Tab::Days => {
                let rows = by_day(&shown, state.offset_minutes);
                grouped(ui, rows.iter().map(|(d, t)| (d.to_string(), t)));
            }
            Tab::Months => {
                let rows = by_month(&shown, state.offset_minutes);
                grouped(ui, rows.iter().map(|(m, t)| (m.to_string(), t)));
            }
        }
    }
    if log.skipped > 0 {
        ui.weak(words::skipped_line(log.skipped));
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!shown.is_empty(), egui::Button::new(EXPORT))
            .clicked()
        {
            action = Some(HistoryAction::Export(to_csv(&shown)));
        }
        if state.confirming_clear {
            ui.label("Delete the history for every app?");
            if ui.button(CLEAR_YES).clicked() {
                state.confirming_clear = false;
                action = Some(HistoryAction::Clear);
            }
            if ui.button(CLEAR_NO).clicked() {
                state.confirming_clear = false;
            }
        } else if ui
            .add_enabled(!log.entries.is_empty(), egui::Button::new(CLEAR))
            .clicked()
        {
            state.confirming_clear = true;
        }
    });
    action
}

fn grouped<'a>(ui: &mut Ui, rows: impl Iterator<Item = (String, &'a Totals)>) {
    ScrollArea::vertical()
        .max_height(LIST_HEIGHT)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (label, totals) in rows {
                ui.label(words::group_line(&label, totals));
            }
        });
}

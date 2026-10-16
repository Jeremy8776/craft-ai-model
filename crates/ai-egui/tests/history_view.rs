//! The history view driven like a person would: filter, switch tables, export, clear.

use craft_ai_core::history::{EndRecord, HISTORY_SCHEMA, StartRecord, merge};
use craft_ai_core::job::Usage;
use craft_ai_core::{ErrorKind, HistoryLog, HistoryRecord, Locality, Outcome};
use craft_ai_egui::history_text::entry_line;
use craft_ai_egui::{
    CLEAR, CLEAR_NO, CLEAR_YES, EMPTY, EXPORT, HistoryAction, HistoryViewState, NOTHING_MATCHES,
    REFRESH, Range, TAB_DAYS, TAB_MONTHS, Tab, history_view,
};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

const T0: u64 = 1_760_000_000_000; // 2025-10-09T08:53:20Z
const DAY: u64 = 86_400_000;

struct Job {
    run: &'static str,
    at: u64,
    app: &'static str,
    model: &'static str,
    outcome: Option<Outcome>,
    cost: Option<u64>,
}

fn job(run: &'static str, at: u64, app: &'static str, model: &'static str) -> Job {
    Job {
        run,
        at,
        app,
        model,
        outcome: Some(Outcome::Done),
        cost: None,
    }
}

fn log(jobs: &[Job], skipped: usize) -> HistoryLog {
    let mut records = Vec::new();
    for j in jobs {
        records.push(HistoryRecord::Start(StartRecord {
            v: HISTORY_SCHEMA,
            run: j.run.into(),
            at_ms: j.at,
            app: j.app.into(),
            role_id: "fill".into(),
            role_label: "Generative Fill".into(),
            capability: "image.inpaint".into(),
            connection: "c".into(),
            connection_label: "Hosted".into(),
            model: j.model.into(),
            locality: Locality::Remote,
            destination: "https://api.example.com".into(),
            sent: Vec::new(),
            params: Vec::new(),
            provider_job: None,
            estimate: None,
            prompt: None,
        }));
        if let Some(outcome) = &j.outcome {
            records.push(HistoryRecord::End(EndRecord {
                v: HISTORY_SCHEMA,
                run: j.run.into(),
                at_ms: j.at + 1000,
                outcome: outcome.clone(),
                usage: j.cost.map(|c| Usage {
                    cost_micros: Some(c),
                    currency: Some("USD".into()),
                    ..Usage::default()
                }),
            }));
        }
    }
    merge(records, skipped)
}

struct State {
    view: HistoryViewState,
    log: HistoryLog,
    now: u64,
    action: Option<HistoryAction>,
}

fn harness(view: HistoryViewState, log: HistoryLog, now: u64) -> Harness<'static, State> {
    Harness::new_ui_state(
        |ui, s: &mut State| {
            if let Some(a) = history_view(ui, &mut s.view, &s.log, s.now) {
                s.action = Some(a);
            }
        },
        State {
            view,
            log,
            now,
            action: None,
        },
    )
}

fn two_apps() -> HistoryLog {
    log(
        &[
            Job {
                cost: Some(31_000),
                ..job("a", T0, "photocraft", "flux")
            },
            Job {
                outcome: Some(Outcome::Failed {
                    kind: ErrorKind::Quota,
                }),
                ..job("b", T0 + 1000, "wordcraft", "gpt-4o")
            },
            Job {
                outcome: None,
                ..job("c", T0 + 2000, "photocraft", "flux")
            },
        ],
        0,
    )
}

#[test]
fn an_empty_history_says_so_and_offers_no_clear_or_export() {
    let mut h = harness(HistoryViewState::default(), log(&[], 0), T0);
    h.run();
    h.get_by_label(EMPTY);
    assert!(h.get_by_label(CLEAR).accesskit_node().is_disabled());
    assert!(h.get_by_label(EXPORT).accesskit_node().is_disabled());
}

#[test]
fn jobs_show_as_lines_with_counts_and_only_reported_cost() {
    let log = two_apps();
    let line_a = entry_line(&log.entries[2], 0);
    let mut h = harness(HistoryViewState::default(), log, T0 + 5000);
    h.run();
    h.get_by_label("3 jobs: 1 done, 1 failed, 1 not finished.");
    h.get_by_label("Reported cost: 0.03 USD.");
    h.get_by_label(&line_a);
    assert!(line_a.contains("0.03 USD"), "{line_a}");
}

#[test]
fn filtering_by_app_hides_the_others_and_export_follows_the_filter() {
    let log = two_apps();
    let word_line = entry_line(&log.entries[1], 0);
    let mut view = HistoryViewState::default();
    view.app = Some("photocraft".into());
    let mut h = harness(view, log, T0 + 5000);
    h.run();
    h.get_by_label("2 jobs: 1 done, 1 not finished.");
    assert!(h.query_by_label(&word_line).is_none());
    h.get_by_label(EXPORT).click();
    h.run();
    let Some(HistoryAction::Export(csv)) = h.state().action.clone() else {
        panic!("no export: {:?}", h.state().action);
    };
    assert!(csv.starts_with("started_utc,app,feature"));
    assert!(csv.contains("photocraft"));
    assert!(!csv.contains("wordcraft"), "{csv}");
    assert_eq!(csv.matches("\r\n").count(), 3, "header and two rows");
}

#[test]
fn a_filter_left_on_something_no_longer_in_the_log_falls_back_to_everything() {
    let mut view = HistoryViewState::for_app("gone-app");
    view.model = Some("gone-model".into());
    let mut h = harness(view, two_apps(), T0 + 5000);
    h.run();
    h.run();
    h.get_by_label("3 jobs: 1 done, 1 failed, 1 not finished.");
    assert_eq!(h.state().view.app, None);
    assert_eq!(h.state().view.model, None);
}

#[test]
fn a_range_with_nothing_in_it_says_no_jobs_match() {
    let mut view = HistoryViewState::default();
    view.range = Range::Week;
    let mut h = harness(view, two_apps(), T0 + 40 * DAY);
    h.run();
    h.get_by_label(NOTHING_MATCHES);
    assert!(h.get_by_label(EXPORT).accesskit_node().is_disabled());
}

#[test]
fn today_starts_at_midnight_in_the_chosen_offset() {
    // 08:53Z on 9 October is 10:53 at UTC+2; "today" there began at 22:00Z on the 8th.
    let log = log(
        &[
            job("old", T0 - 12 * 3_600_000, "photocraft", "flux"),
            job("new", T0, "photocraft", "flux"),
        ],
        0,
    );
    let mut view = HistoryViewState::default();
    view.range = Range::Today;
    view.offset_minutes = 120;
    let mut h = harness(view, log, T0 + 1000);
    h.run();
    h.get_by_label("1 job: 1 done.");
}

#[test]
fn the_day_and_month_tables_total_per_group() {
    let log = log(
        &[
            Job {
                cost: Some(10_000),
                ..job("a", T0, "photocraft", "flux")
            },
            Job {
                cost: Some(20_000),
                ..job("b", T0 + 1000, "photocraft", "flux")
            },
            Job {
                cost: Some(5_000),
                ..job("c", T0 + 30 * DAY, "photocraft", "flux")
            },
        ],
        0,
    );
    let mut h = harness(HistoryViewState::default(), log, T0 + 31 * DAY);
    h.run();
    h.get_by_label(TAB_DAYS).click();
    h.run();
    h.get_by_label("2025-11-08  \u{b7}  1 job  \u{b7}  0.0050 USD");
    h.get_by_label("2025-10-09  \u{b7}  2 jobs  \u{b7}  0.03 USD");
    h.get_by_label(TAB_MONTHS).click();
    h.run();
    h.get_by_label("2025-11  \u{b7}  1 job  \u{b7}  0.0050 USD");
    h.get_by_label("2025-10  \u{b7}  2 jobs  \u{b7}  0.03 USD");
    assert_eq!(h.state().view.tab, Tab::Months);
}

#[test]
fn clearing_needs_a_second_yes_and_keep_cancels_it() {
    let mut h = harness(HistoryViewState::default(), two_apps(), T0 + 5000);
    h.run();
    h.get_by_label(CLEAR).click();
    h.run();
    assert_eq!(h.state().action, None, "one click is only a question");
    h.get_by_label("Delete the history for every app?");
    h.get_by_label(CLEAR_NO).click();
    h.run();
    assert_eq!(h.state().action, None);
    h.get_by_label(CLEAR).click();
    h.run();
    h.get_by_label(CLEAR_YES).click();
    h.run();
    assert_eq!(h.state().action, Some(HistoryAction::Clear));
    h.run();
    assert!(
        h.query_by_label(CLEAR_YES).is_none(),
        "the question goes away"
    );
}

#[test]
fn refresh_is_reported_and_unreadable_lines_are_mentioned() {
    let mut h = harness(
        HistoryViewState::default(),
        log(&[job("a", T0, "p", "m")], 2),
        T0,
    );
    h.run();
    h.get_by_label("2 lines in the history could not be read and were left out.");
    h.get_by_label(REFRESH).click();
    h.run();
    assert_eq!(h.state().action, Some(HistoryAction::Refresh));
}

#[test]
fn a_long_log_draws_only_the_rows_in_view() {
    let jobs: Vec<Job> = (0..3000u64)
        .map(|i| {
            let run: &'static str = Box::leak(format!("r{i}").into_boxed_str());
            job(run, T0 + i * 60_000, "photocraft", "flux")
        })
        .collect();
    let log = log(&jobs, 0);
    let oldest = entry_line(&log.entries[2999], 0);
    let newest = entry_line(&log.entries[0], 0);
    let mut h = harness(HistoryViewState::default(), log, T0 + 4_000_000);
    h.run();
    h.get_by_label(&newest);
    assert!(h.query_by_label(&oldest).is_none());
    h.get_by_label("3000 jobs: 3000 done.");
}

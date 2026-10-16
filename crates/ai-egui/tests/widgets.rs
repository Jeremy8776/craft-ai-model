//! The widgets driven headlessly: real egui layout, real clicks, no window and no GPU.

use craft_ai_client::{Choice, RoleReady, RoleStatus};
use craft_ai_core::consent::DisclosureItem;
use craft_ai_core::{
    AiError, AppId, Billing, CapabilityId, ConnectionId, CostEstimate, Disclosure, ErrorKind,
    InputRole, JobState, Locality, ModelId, PayloadKind,
};
use craft_ai_egui::{
    ADD_CONNECTION, ALLOW, CANCEL, ConsentChoice, DENY, OPEN_LAUNCHER, ProgressAction,
    StatusAction, StatusView, USE_DEFAULT, ai_status, consent_prompt, job_progress,
};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

fn disclosure(locality: Locality) -> Disclosure {
    Disclosure {
        app: AppId::new("photocraft").unwrap(),
        connection: ConnectionId::new("cloud").unwrap(),
        capability: CapabilityId::parse("image.inpaint").unwrap(),
        locality,
        destination: "https://api.example.com".into(),
        items: vec![DisclosureItem {
            role: InputRole::Source,
            kind: PayloadKind::Image,
            bytes: Some(2_500_000),
        }],
        param_names: vec!["size".into()],
    }
}

fn choice(conn: &str, model: &str, label: &str, locality: Locality) -> Choice {
    Choice {
        connection: ConnectionId::new(conn).unwrap(),
        connection_label: format!("Conn {conn}"),
        model: ModelId::new(model).unwrap(),
        model_label: label.into(),
        locality,
    }
}

fn ready(conn: &str, model: &str, label: &str) -> RoleStatus {
    RoleStatus::Ready(RoleReady {
        connection: ConnectionId::new(conn).unwrap(),
        connection_label: format!("Conn {conn}"),
        model: ModelId::new(model).unwrap(),
        model_label: label.into(),
        locality: Locality::Local,
        billing: Billing::Free,
    })
}

// ---- consent ----

fn consent_harness(d: Disclosure) -> Harness<'static, Option<ConsentChoice>> {
    Harness::new_ui_state(
        move |ui, state: &mut Option<ConsentChoice>| {
            if let Some(c) = consent_prompt(ui, &d, "PhotoCraft") {
                *state = Some(c);
            }
        },
        None,
    )
}

#[test]
fn consent_shows_destination_kinds_sizes_and_the_remote_badge() {
    let mut h = consent_harness(disclosure(Locality::Remote));
    h.run();
    h.get_by_label("Leaves this computer");
    h.get_by_label("PhotoCraft will send the following to https://api.example.com");
    h.get_by_label("\u{2022} image (source), 2.5 MB");
    h.get_by_label("\u{2022} settings: size");
    assert_eq!(
        *h.state(),
        None,
        "nothing is chosen until a button is pressed"
    );
}

#[test]
fn consent_allow_and_deny_report_exactly_one_choice() {
    let mut h = consent_harness(disclosure(Locality::Remote));
    h.run();
    h.get_by_label(ALLOW).click();
    h.run();
    assert_eq!(*h.state(), Some(ConsentChoice::Allow));

    let mut h = consent_harness(disclosure(Locality::Remote));
    h.run();
    h.get_by_label(DENY).click();
    h.run();
    assert_eq!(*h.state(), Some(ConsentChoice::Deny));
}

#[test]
fn consent_for_a_local_model_says_it_stays_here() {
    let mut h = consent_harness(disclosure(Locality::Local));
    h.run();
    h.get_by_label("Stays on this computer");
    assert!(h.query_by_label("Leaves this computer").is_none());
}

// ---- status ----

struct StatusState {
    status: RoleStatus,
    choices: Vec<Choice>,
    has_override: bool,
    can_add_here: bool,
    last: Option<StatusAction>,
}

fn status_harness(s: StatusState) -> Harness<'static, StatusState> {
    Harness::new_ui_state(
        |ui, st: &mut StatusState| {
            let view = StatusView {
                label: "Generative Fill",
                status: &st.status,
                choices: &st.choices,
                has_override: st.has_override,
                can_add_here: st.can_add_here,
            };
            if let Some(a) = ai_status(ui, &view) {
                st.last = Some(a);
            }
        },
        s,
    )
}

#[test]
fn unavailable_feature_explains_itself_and_offers_the_launcher() {
    let mut h = status_harness(StatusState {
        status: RoleStatus::Unavailable {
            reason: "no model is chosen for 'fill'".into(),
        },
        choices: vec![],
        has_override: false,
        can_add_here: false,
        last: None,
    });
    h.run();
    h.get_by_label("Generative Fill is off: no model is chosen for 'fill'");
    assert!(
        h.query_by_label(USE_DEFAULT).is_none(),
        "no override, no reset button"
    );
    h.get_by_label(OPEN_LAUNCHER).click();
    h.run();
    assert_eq!(h.state().last, Some(StatusAction::OpenLauncher));
}

#[test]
fn ready_feature_shows_its_model_and_no_launcher_button() {
    let mut h = status_harness(StatusState {
        status: ready("local", "llama3", "Llama 3"),
        choices: vec![choice("local", "llama3", "Llama 3", Locality::Local)],
        has_override: false,
        can_add_here: false,
        last: None,
    });
    h.run();
    h.get_by_label("Generative Fill: Llama 3 on Conn local (stays on this computer, free)");
    assert!(h.query_by_label(OPEN_LAUNCHER).is_none());
}

#[test]
fn picking_another_model_reports_it_and_the_current_one_does_not() {
    let mut h = status_harness(StatusState {
        status: ready("local", "llama3", "Llama 3"),
        choices: vec![
            choice("local", "llama3", "Llama 3", Locality::Local),
            choice("cloud", "gpt-4o", "GPT-4o", Locality::Remote),
        ],
        has_override: false,
        can_add_here: false,
        last: None,
    });
    h.run();
    h.get_by_role(Role::ComboBox).click();
    h.run();
    // Choosing what is already selected changes nothing.
    h.query_all_by_label_contains("Llama 3 \u{2014} Conn local")
        .last()
        .unwrap()
        .click();
    h.run();
    assert_eq!(h.state().last, None);

    let mut h = status_harness(StatusState {
        status: ready("local", "llama3", "Llama 3"),
        choices: vec![
            choice("local", "llama3", "Llama 3", Locality::Local),
            choice("cloud", "gpt-4o", "GPT-4o", Locality::Remote),
        ],
        has_override: false,
        can_add_here: false,
        last: None,
    });
    h.run();
    h.get_by_role(Role::ComboBox).click();
    h.run();
    h.get_by_label_contains("GPT-4o \u{2014} Conn cloud (leaves this computer)")
        .click();
    h.run();
    assert_eq!(
        h.state().last,
        Some(StatusAction::Pick {
            connection: ConnectionId::new("cloud").unwrap(),
            model: ModelId::new("gpt-4o").unwrap(),
        })
    );
}

#[test]
fn use_default_appears_only_with_an_override() {
    let mut h = status_harness(StatusState {
        status: ready("local", "llama3", "Llama 3"),
        choices: vec![],
        has_override: true,
        can_add_here: false,
        last: None,
    });
    h.run();
    h.get_by_label(USE_DEFAULT).click();
    h.run();
    assert_eq!(h.state().last, Some(StatusAction::UseDefault));
}

#[test]
fn adding_a_connection_here_is_offered_only_when_asked_and_only_when_unavailable() {
    let unavailable = || RoleStatus::Unavailable {
        reason: "no connection".into(),
    };
    // Off by default: the launcher is the only way.
    let mut h = status_harness(StatusState {
        status: unavailable(),
        choices: vec![],
        has_override: false,
        can_add_here: false,
        last: None,
    });
    h.run();
    assert!(h.query_by_label(ADD_CONNECTION).is_none());
    h.get_by_label(OPEN_LAUNCHER);
    // On: offered next to the launcher button, not instead of it.
    let mut h = status_harness(StatusState {
        status: unavailable(),
        choices: vec![],
        has_override: false,
        can_add_here: true,
        last: None,
    });
    h.run();
    h.get_by_label(OPEN_LAUNCHER);
    h.get_by_label(ADD_CONNECTION).click();
    h.run();
    assert_eq!(h.state().last, Some(StatusAction::AddConnection));
    // A feature that already works has nothing to add.
    let mut h = status_harness(StatusState {
        status: ready("local", "llama3", "Llama 3"),
        choices: vec![],
        has_override: false,
        can_add_here: true,
        last: None,
    });
    h.run();
    assert!(h.query_by_label(ADD_CONNECTION).is_none());
}

// ---- progress ----

struct ProgressState {
    job: JobState,
    cost: Option<CostEstimate>,
    cancelled: bool,
}

fn progress_harness(job: JobState, cost: Option<CostEstimate>) -> Harness<'static, ProgressState> {
    Harness::new_ui_state(
        |ui, st: &mut ProgressState| {
            if job_progress(ui, &st.job, st.cost.as_ref()) == Some(ProgressAction::Cancel) {
                st.cancelled = true;
            }
        },
        ProgressState {
            job,
            cost,
            cancelled: false,
        },
    )
}

#[test]
fn a_running_job_shows_percent_cost_and_can_be_cancelled() {
    let cost = CostEstimate {
        micros: 40_000,
        currency: "USD".into(),
        billing: Billing::Metered,
    };
    let mut h = progress_harness(JobState::Running { percent: Some(40) }, Some(cost));
    h.run();
    h.get_by_label_contains("Working, 40%");
    h.get_by_label("about 0.04 USD (billed per use)");
    h.get_by_label(CANCEL).click();
    h.run();
    assert!(h.state().cancelled);
}

#[test]
fn a_job_with_no_percentage_still_shows_it_is_working() {
    let mut h = progress_harness(JobState::Running { percent: None }, None);
    // A spinner repaints forever, so step a few frames instead of running to rest.
    h.step();
    h.step();
    h.get_by_label("Working");
    h.get_by_label(CANCEL);
}

#[test]
fn finished_jobs_say_how_they_ended_and_cannot_be_cancelled() {
    for (state, text) in [
        (JobState::Succeeded, "Done".to_owned()),
        (JobState::Cancelled, "Cancelled".to_owned()),
        (
            JobState::Failed {
                error: AiError::new(ErrorKind::Quota, "out of credits"),
            },
            "Failed: out of credits".to_owned(),
        ),
    ] {
        let mut h = progress_harness(state, None);
        h.run();
        h.get_by_label_contains(&text);
        assert!(h.query_by_label(CANCEL).is_none(), "{text}");
    }
}

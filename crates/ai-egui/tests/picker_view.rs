//! The model picker widget driven like a person would: real layout, real clicks, no window.

use craft_ai_client::{
    CurrentChoice, GroupState, PickerGroup, PickerModel, PickerPreset, PickerView, SetupLink,
};
use craft_ai_core::{ConnectionId, Locality, ModelId, Pick, RoleId};
use craft_ai_egui::{
    ADD_KEY, ALL_MODELS, HEADING, IN_USE, NOTHING, PickerAction, SET_UP, SUGGESTED, SWITCH_ON,
    model_picker,
};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

struct State {
    view: PickerView,
    last: Option<PickerAction>,
}

fn row(conn: &str, model: &str, label: &str, selected: bool) -> PickerModel {
    PickerModel {
        connection: ConnectionId::new(conn).unwrap(),
        model: ModelId::new(model).unwrap(),
        label: label.into(),
        description: format!("About {label}."),
        selected,
    }
}

fn group(title: &str, state: GroupState, models: Vec<PickerModel>) -> PickerGroup {
    PickerGroup {
        title: title.into(),
        locality: Some(Locality::Local),
        state,
        note: String::new(),
        models,
        suggested: false,
        setup: None,
        size_bytes: None,
    }
}

fn view(groups: Vec<PickerGroup>, current: Option<CurrentChoice>) -> PickerView {
    PickerView {
        tool: RoleId::new("select-subject").unwrap(),
        title: "Select subject".into(),
        current,
        presets: Vec::new(),
        groups,
    }
}

fn harness(view: PickerView) -> Harness<'static, State> {
    let mut h = Harness::new_ui_state(
        |ui, st: &mut State| {
            if let Some(a) = model_picker(ui, &st.view) {
                st.last = Some(a);
            }
        },
        State { view, last: None },
    );
    h.run();
    h
}

fn fix(id: &str) -> Option<SetupLink> {
    Some(SetupLink::Fix {
        connection: ConnectionId::new(id).unwrap(),
    })
}

#[test]
fn clicking_a_model_under_a_ready_provider_chooses_it() {
    let mut h = harness(view(
        vec![
            group(
                "Box",
                GroupState::Ready,
                vec![row("box", "a", "Alpha", false)],
            ),
            group(
                "Cloud",
                GroupState::Ready,
                vec![row("cloud", "b", "Beta", false)],
            ),
        ],
        None,
    ));
    h.get_by_label(HEADING);
    h.get_by_label("for Select subject");
    h.get_by_label_contains("About Beta.").click();
    h.run();
    assert_eq!(
        h.state().last,
        Some(PickerAction::Choose {
            connection: ConnectionId::new("cloud").unwrap(),
            model: ModelId::new("b").unwrap(),
        })
    );
}

#[test]
fn each_provider_shows_its_name_and_each_model_its_one_line() {
    let h = harness(view(
        vec![group(
            "Box",
            GroupState::Ready,
            vec![row("box", "a", "Alpha", false)],
        )],
        None,
    ));
    h.get_by_label("Box");
    h.get_by_label("Stays on this computer");
    h.get_by_label_contains("Alpha");
    h.get_by_label_contains("About Alpha.");
}

#[test]
fn a_greyed_provider_cannot_be_picked_from_but_its_set_up_link_works() {
    let mut nokey = group(
        "Cloud",
        GroupState::NeedsKey,
        vec![row("cloud", "b", "Beta", false)],
    );
    nokey.setup = fix("cloud");
    nokey.note = "No key is saved for it.".into();
    let mut h = harness(view(vec![nokey], None));
    h.get_by_label("No key is saved for it.");
    h.get_by_label_contains("About Beta.").click();
    h.run();
    assert_eq!(
        h.state().last,
        None,
        "a model under a greyed provider is not pickable"
    );
    h.get_by_label(ADD_KEY).click();
    h.run();
    assert_eq!(
        h.state().last,
        Some(PickerAction::SetUp(fix("cloud").unwrap()))
    );
}

#[test]
fn the_set_up_button_says_what_it_will_do_for_each_state() {
    let mut off = group(
        "Off",
        GroupState::SwitchedOff,
        vec![row("off", "c", "Gamma", false)],
    );
    off.setup = fix("off");
    let mut new = group("OpenAI", GroupState::NotSetUp, vec![]);
    new.setup = Some(SetupLink::New {
        option: "service:openai".into(),
    });
    let mut h = harness(view(vec![off, new], None));
    // Not hidden: both providers are listed, each with its own button.
    h.get_by_label("Off");
    h.get_by_label("OpenAI");
    h.get_by_label(SWITCH_ON);
    h.get_by_label(SET_UP).click();
    h.run();
    assert_eq!(
        h.state().last,
        Some(PickerAction::SetUp(SetupLink::New {
            option: "service:openai".into()
        }))
    );
}

#[test]
fn the_model_in_use_is_marked_and_choosing_it_again_does_nothing() {
    let mut h = harness(view(
        vec![group(
            "Box",
            GroupState::Ready,
            vec![row("box", "a", "Alpha", true)],
        )],
        None,
    ));
    h.get_by_label_contains(IN_USE);
    h.get_by_label_contains("About Alpha.").click();
    h.run();
    assert_eq!(h.state().last, None);
}

#[test]
fn a_choice_that_stopped_working_says_why_and_a_working_one_says_nothing() {
    let broken = CurrentChoice {
        connection: ConnectionId::new("gone").unwrap(),
        model: ModelId::new("x").unwrap(),
        label: "Old model".into(),
        problem: Some("the connection is gone".into()),
    };
    let h = harness(view(
        vec![group(
            "Box",
            GroupState::Ready,
            vec![row("box", "a", "Alpha", false)],
        )],
        Some(broken.clone()),
    ));
    h.get_by_label("Your choice, Old model, is not working: the connection is gone");

    let working = CurrentChoice {
        problem: None,
        ..broken
    };
    let h = harness(view(
        vec![group(
            "Box",
            GroupState::Ready,
            vec![row("box", "a", "Alpha", false)],
        )],
        Some(working),
    ));
    assert!(h.query_by_label_contains("is not working").is_none());
}

#[test]
fn with_nothing_to_show_it_says_so() {
    let h = harness(view(vec![], None));
    h.get_by_label(NOTHING);
}

#[test]
fn a_provider_to_install_says_how_much_it_downloads() {
    let mut local = group("SAM 3", GroupState::NotSetUp, vec![]);
    local.size_bytes = Some(3_449_860_632);
    let h = harness(view(vec![local], None));
    h.get_by_label("Download 3.4 GB");

    let h = harness(view(
        vec![group("Cloud", GroupState::NotSetUp, vec![])],
        None,
    ));
    assert!(h.query_by_label_contains("Download").is_none());
}

#[test]
fn a_suggested_provider_says_so_and_the_others_do_not() {
    let mut suggested = group(
        "Box",
        GroupState::Ready,
        vec![row("box", "a", "Alpha", false)],
    );
    suggested.suggested = true;
    let h = harness(view(
        vec![
            suggested,
            group(
                "Cloud",
                GroupState::Ready,
                vec![row("cloud", "b", "Beta", false)],
            ),
        ],
        None,
    ));
    h.get_by_label("Suggested for this computer");
    assert_eq!(
        h.query_all_by_label("Suggested for this computer").count(),
        1
    );
}

fn preset(pick: Pick, reason: &str, model: PickerModel) -> PickerPreset {
    PickerPreset {
        pick,
        reason: reason.into(),
        model,
    }
}

#[test]
fn presets_sit_above_the_full_list_with_their_reason_and_choose_like_any_row() {
    let beta = row("cloud", "b", "Beta", false);
    let mut v = view(
        vec![group(
            "Cloud",
            GroupState::Ready,
            vec![row("cloud", "a", "Alpha", false), beta.clone()],
        )],
        None,
    );
    v.presets = vec![preset(
        Pick::LowestCost,
        "Lowest reported price: $0.04 per image.",
        beta,
    )];
    let mut h = harness(v);
    h.get_by_label(SUGGESTED);
    h.get_by_label(ALL_MODELS);
    h.get_by_label("Lowest cost");
    h.get_by_label("Lowest reported price: $0.04 per image.");
    // The model shows twice: once as the preset, once in the full list. Either picks it.
    let rows: Vec<_> = h.get_all_by_label_contains("About Beta.").collect();
    assert_eq!(rows.len(), 2);
    rows[0].click();
    h.run();
    assert_eq!(
        h.state().last,
        Some(PickerAction::Choose {
            connection: ConnectionId::new("cloud").unwrap(),
            model: ModelId::new("b").unwrap(),
        })
    );
}

#[test]
fn with_no_presets_there_is_no_suggested_section_and_no_extra_heading() {
    let h = harness(view(
        vec![group(
            "Box",
            GroupState::Ready,
            vec![row("box", "a", "Alpha", false)],
        )],
        None,
    ));
    assert!(h.query_by_label(SUGGESTED).is_none());
    assert!(h.query_by_label(ALL_MODELS).is_none());
}

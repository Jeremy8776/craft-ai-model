//! The missing-model popup driven like a person would: real layout, real clicks, no window.

use craft_ai_client::{GroupState, MissingModel, PickerGroup, PickerModel, PickerView, SetupLink};
use craft_ai_egui::{ADD_KEY, HEADING_SUFFIX, NOT_NOW, PopupAction, SET_UP, missing_model_popup};
use craft_ai_model::{ConnectionId, Locality, ModelId, RoleId};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

struct State {
    popup: MissingModel,
    last: Option<PopupAction>,
}

fn popup(groups: Vec<PickerGroup>) -> MissingModel {
    MissingModel {
        tool: RoleId::new("select-subject").unwrap(),
        title: "Select subject".into(),
        reason: "no model is chosen for 'select-subject'".into(),
        picker: PickerView {
            tool: RoleId::new("select-subject").unwrap(),
            title: "Select subject".into(),
            current: None,
            groups,
        },
    }
}

fn group(title: &str, state: GroupState) -> PickerGroup {
    PickerGroup {
        title: title.into(),
        locality: Some(Locality::Local),
        state,
        note: String::new(),
        models: Vec::new(),
        setup: None,
        size_bytes: None,
    }
}

fn harness(popup: MissingModel) -> Harness<'static, State> {
    let mut h = Harness::new_ui_state(
        |ui, st: &mut State| {
            if let Some(a) = missing_model_popup(ui, &st.popup) {
                st.last = Some(a);
            }
        },
        State { popup, last: None },
    );
    h.run();
    h
}

#[test]
fn it_says_which_tool_needs_a_model_and_why() {
    let h = harness(popup(vec![group("Box", GroupState::Ready)]));
    h.get_by_label(&format!("Select subject {HEADING_SUFFIX}"));
    h.get_by_label("no model is chosen for 'select-subject'");
}

#[test]
fn picking_a_model_reports_it_so_the_app_can_bind_it_for_every_app() {
    let mut ready = group("Box", GroupState::Ready);
    ready.models.push(PickerModel {
        connection: ConnectionId::new("box").unwrap(),
        model: ModelId::new("sam3").unwrap(),
        label: "SAM 3".into(),
        description: "About SAM 3.".into(),
        selected: false,
    });
    let mut h = harness(popup(vec![ready]));
    h.get_by_label_contains("About SAM 3.").click();
    h.run();
    assert_eq!(
        h.state().last,
        Some(PopupAction::Choose {
            connection: ConnectionId::new("box").unwrap(),
            model: ModelId::new("sam3").unwrap(),
        })
    );
}

#[test]
fn a_provider_to_install_says_how_much_it_downloads_and_can_be_set_up() {
    let mut local = group("SAM 3", GroupState::NotSetUp);
    local.size_bytes = Some(3_449_860_632);
    local.setup = Some(SetupLink::New {
        option: "local:select-subject".into(),
    });
    let mut h = harness(popup(vec![local]));
    h.get_by_label("Download 3.4 GB");
    h.get_by_label(SET_UP).click();
    h.run();
    assert_eq!(
        h.state().last,
        Some(PopupAction::SetUp(SetupLink::New {
            option: "local:select-subject".into()
        }))
    );
}

#[test]
fn a_connection_with_no_key_can_be_fixed_from_the_popup() {
    let mut nokey = group("Cloud", GroupState::NeedsKey);
    nokey.setup = Some(SetupLink::Fix {
        connection: ConnectionId::new("cloud").unwrap(),
    });
    let mut h = harness(popup(vec![nokey]));
    h.get_by_label(ADD_KEY).click();
    h.run();
    assert_eq!(
        h.state().last,
        Some(PopupAction::SetUp(SetupLink::Fix {
            connection: ConnectionId::new("cloud").unwrap()
        }))
    );
}

#[test]
fn not_now_leaves_the_tool_off() {
    let mut h = harness(popup(vec![group("Box", GroupState::Ready)]));
    h.get_by_label(NOT_NOW).click();
    h.run();
    assert_eq!(h.state().last, Some(PopupAction::NotNow));
}

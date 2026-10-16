//! The missing-model popup driven like a person would: real layout, real clicks, no window.

use craft_ai_client::{GroupState, MissingModel, PickerGroup, PickerModel, PickerView, SetupLink};
use craft_ai_core::{ConnectionId, Locality, ModelId, RoleId};
use craft_ai_egui::{
    ADD_KEY, HEADING_SUFFIX, NOT_NOW, PopupAction, PopupInstallState, SET_UP,
    missing_model_popup_with_install,
};
use craft_ai_features::{
    Gpu, GpuVendor, Inventory, Os, SetupOffer, SetupRoute, SystemProfile, setup_offer,
};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

struct State {
    popup: MissingModel,
    offer: Option<SetupOffer>,
    install: PopupInstallState,
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
        suggested: false,
        setup: None,
        size_bytes: None,
    }
}

fn harness(popup: MissingModel) -> Harness<'static, State> {
    harness_with_offer(popup, None)
}

fn harness_with_offer(popup: MissingModel, offer: Option<SetupOffer>) -> Harness<'static, State> {
    let mut h = Harness::new_ui_state(
        |ui, st: &mut State| {
            if let Some(a) =
                missing_model_popup_with_install(ui, &st.popup, st.offer.as_ref(), &mut st.install)
            {
                st.last = Some(a);
            }
        },
        State {
            popup,
            offer,
            install: PopupInstallState::default(),
            last: None,
        },
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

fn install_offer() -> SetupOffer {
    let feature = craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == "select-subject")
        .unwrap();
    setup_offer(
        &feature,
        &SystemProfile {
            os: Os::Windows,
            gpus: vec![Gpu {
                name: "RTX 3090".into(),
                vendor: GpuVendor::Nvidia,
                vram_mb: Some(24576),
            }],
            comfyui_found: false,
        },
        &Inventory {
            free_disk_bytes: Some(100_000_000_000),
            ..Inventory::default()
        },
        SetupRoute::ComfyUi,
    )
    .unwrap()
}

#[test]
fn installing_is_optional_and_requires_a_separate_approval() {
    let offer = install_offer();
    let build = offer.portable.clone().unwrap();
    let mut h = harness_with_offer(
        popup(vec![group("SAM 3", GroupState::NotSetUp)]),
        Some(offer),
    );
    assert_eq!(h.state().last, None);
    h.get_by_label("Install ComfyUI on this computer").click();
    h.run();
    h.get_by_label("Download 2.0 GB from the ComfyUI authors.");
    h.get_by_label("This installs ComfyUI only. The model and its add-ons are separate.");
    h.get_by_label("Read ComfyUI's licence (GPL-3.0)");
    h.get_by_label("Install ComfyUI").click();
    h.run();
    assert_eq!(
        h.state().last,
        None,
        "the first click must not start downloading"
    );
    h.get_by_label("I agree to install ComfyUI from its authors")
        .click();
    h.run();
    h.get_by_label("Install ComfyUI").click();
    h.run();
    assert_eq!(h.state().last, Some(PopupAction::InstallComfyUi(build)));
}

#[test]
fn no_offer_means_no_install_button() {
    let h = harness(popup(vec![group("SAM 3", GroupState::NotSetUp)]));
    assert!(
        h.query_by_label("Install ComfyUI on this computer")
            .is_none()
    );
}

#[test]
fn other_computers_get_plain_steps_instead_of_a_dead_install_button() {
    let feature = craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == "select-subject")
        .unwrap();
    let offer = setup_offer(
        &feature,
        &SystemProfile {
            os: Os::Linux,
            gpus: vec![],
            comfyui_found: false,
        },
        &Inventory::default(),
        SetupRoute::ComfyUi,
    )
    .unwrap();
    let h = harness_with_offer(popup(vec![]), Some(offer));
    assert!(
        h.query_by_label("Install ComfyUI on this computer")
            .is_none()
    );
    h.get_by_label("Install ComfyUI from comfy.org, open it once, then come back here.");
}

//! The tool-first model picker's data, against a temporary store and no servers.

use std::sync::Arc;

use craft_ai_client::{AiClient, GroupState, PickerView, SetupLink};
use craft_ai_core::{
    AppId, Binding, CapabilityId, CapabilitySpec, Connection, ConnectionId, InputRole, Locality,
    ModelId, ModelInfo, Role, SecretRef, SecretString, StoreFile,
};
use craft_ai_features::{Route, SetupOption};
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use serde_json::json;

const KEY: &str = "sekrit-value-1234";

fn builtin_role(id: &str) -> Role {
    craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == id)
        .unwrap()
        .role()
        .unwrap()
}

fn select_subject() -> Role {
    builtin_role("select-subject")
}

fn model(id: &str, label: &str, capability: &str, inputs: &[InputRole]) -> ModelInfo {
    let mut spec = CapabilitySpec::new(CapabilityId::parse(capability).unwrap());
    for input in inputs {
        spec = spec.accepting(input.clone(), 1);
    }
    ModelInfo::new(ModelId::new(id).unwrap(), label).with(spec)
}

fn segmenter(id: &str, label: &str) -> ModelInfo {
    model(
        id,
        label,
        "image.segment",
        &[InputRole::Source, InputRole::Prompt],
    )
}

fn inpainter(id: &str, label: &str) -> ModelInfo {
    model(
        id,
        label,
        "image.inpaint",
        &[InputRole::Source, InputRole::Mask, InputRole::Prompt],
    )
}

/// `remote` connections need a key; the rest run on this computer and need none.
fn connection(id: &str, provider: &str, remote: bool, models: Vec<ModelInfo>) -> Connection {
    serde_json::from_value(json!({
        "id": id, "label": format!("Label {id}"), "provider": provider,
        "endpoint": if remote { "https://api.example.com/v1" } else { "http://127.0.0.1:9" },
        "locality": if remote { "remote" } else { "local" },
        "auth": if remote { json!({"kind": "api_key", "secret": format!("{id}-key")}) } else { json!({"kind": "none"}) },
        "billing": if remote { "metered" } else { "free" },
        "models": models,
    }))
    .unwrap()
}

fn bound(role: &Role, conn: &str, model: &str) -> Binding {
    Binding {
        role: role.id.clone(),
        connection: ConnectionId::new(conn).unwrap(),
        model: ModelId::new(model).unwrap(),
    }
}

struct World {
    _dir: tempfile::TempDir,
    shared: SharedStore,
    secrets: Arc<MemoryStore>,
}

impl World {
    fn new(connections: Vec<Connection>, defaults: Vec<Binding>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let shared = SharedStore::new(dir.path().join("connections.json"));
        shared
            .replace(&StoreFile {
                connections,
                defaults,
                ..StoreFile::default()
            })
            .unwrap();
        Self {
            _dir: dir,
            shared,
            secrets: Arc::new(MemoryStore::new()),
        }
    }

    fn save_key(&self, connection: &str) {
        self.secrets
            .set(
                &SecretRef::new(format!("{connection}-key")).unwrap(),
                &SecretString::new(KEY),
            )
            .unwrap();
    }

    fn client(&self, app: &str) -> AiClient {
        AiClient::new(
            AppId::new(app).unwrap(),
            self.shared.clone(),
            self.secrets.clone(),
        )
    }

    fn picker(&self, role: &Role) -> PickerView {
        let options = craft_ai_features::setup_options(&craft_ai_features::builtin());
        self.client("photocraft").picker(role, &options).unwrap()
    }
}

fn titles(view: &PickerView) -> Vec<(String, GroupState)> {
    view.groups
        .iter()
        .map(|g| (g.title.clone(), g.state))
        .collect()
}

#[test]
fn models_are_listed_under_their_provider_and_only_if_they_can_do_the_tool() {
    let w = World::new(
        vec![
            connection(
                "box",
                "comfyui",
                false,
                vec![
                    segmenter("sam3", "SAM 3"),
                    // Cannot take the tool's text prompt, so it is not offered for it.
                    model("yolo", "YOLO", "image.segment", &[InputRole::Source]),
                    // Takes the same inputs but does another job.
                    model(
                        "gen",
                        "Painter",
                        "image.generate",
                        &[InputRole::Source, InputRole::Prompt],
                    ),
                ],
            ),
            connection(
                "cloud",
                "openai",
                true,
                vec![segmenter("seg-xl", "Segment XL")],
            ),
        ],
        vec![],
    );
    w.save_key("cloud");

    let view = w.picker(&select_subject());
    let listed: Vec<(String, Vec<String>)> = view
        .groups
        .iter()
        .filter(|g| g.state == GroupState::Ready)
        .map(|g| {
            (
                g.title.clone(),
                g.models.iter().map(|m| m.label.clone()).collect(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        vec![
            ("Label box".to_owned(), vec!["SAM 3".to_owned()]),
            ("Label cloud".to_owned(), vec!["Segment XL".to_owned()]),
        ]
    );
    assert_eq!(view.title, "Select subject");
}

#[test]
fn each_model_says_what_is_known_about_it_in_one_line() {
    let w = World::new(
        vec![
            connection("box", "comfyui", false, vec![segmenter("sam3", "SAM 3")]),
            connection(
                "cloud",
                "openai",
                true,
                vec![segmenter("seg-xl", "Segment XL")],
            ),
        ],
        vec![],
    );
    w.save_key("cloud");
    let view = w.picker(&select_subject());
    let line = |title: &str| {
        view.groups
            .iter()
            .find(|g| g.title == title)
            .unwrap()
            .models[0]
            .description
            .clone()
    };
    let local = line("Label box");
    assert!(local.contains("Runs on this computer"), "{local}");
    assert!(local.contains("free"), "{local}");
    assert!(local.contains("a source image and a prompt"), "{local}");
    let hosted = line("Label cloud");
    assert!(hosted.contains("Sent to the service"), "{hosted}");
    assert!(hosted.contains("billed per use"), "{hosted}");
    assert!(!local.contains('\n') && !hosted.contains('\n'));
}

#[test]
fn a_provider_with_no_key_or_switched_off_is_greyed_and_can_be_fixed_not_hidden() {
    let mut off = connection("off", "segment-co", false, vec![segmenter("a", "Alpha")]);
    off.enabled = false;
    let w = World::new(
        vec![
            connection("nokey", "openai", true, vec![segmenter("b", "Beta")]),
            off,
            connection("ok", "comfyui", false, vec![segmenter("c", "Gamma")]),
        ],
        vec![],
    );
    let view = w.picker(&select_subject());
    assert_eq!(
        titles(&view)[..3],
        [
            ("Label ok".to_owned(), GroupState::Ready),
            ("Label nokey".to_owned(), GroupState::NeedsKey),
            ("Label off".to_owned(), GroupState::SwitchedOff),
        ]
    );
    let fix = |id: &str| {
        Some(SetupLink::Fix {
            connection: ConnectionId::new(id).unwrap(),
        })
    };
    assert_eq!(view.groups[0].setup, None);
    assert_eq!(view.groups[1].setup, fix("nokey"));
    assert_eq!(view.groups[2].setup, fix("off"));
    // Their models are still listed, so a person can see what they would get.
    assert_eq!(view.groups[1].models[0].label, "Beta");
    assert!(
        view.groups[1].note.contains("key"),
        "{}",
        view.groups[1].note
    );
    assert!(
        view.groups[2].note.contains("off"),
        "{}",
        view.groups[2].note
    );
}

#[test]
fn a_provider_that_is_not_set_up_is_shown_with_a_set_up_link_until_it_is() {
    let fill = builtin_role("generative-fill");
    let w = World::new(vec![], vec![]);
    let view = w.picker(&fill);
    assert_eq!(titles(&view), [("OpenAI".to_owned(), GroupState::NotSetUp)]);
    assert!(view.groups[0].models.is_empty());
    assert_eq!(
        view.groups[0].setup,
        Some(SetupLink::New {
            option: "service:openai".into()
        })
    );

    // Once an OpenAI connection exists it is that connection's group, not a second greyed one.
    let w = World::new(
        vec![connection(
            "oa",
            "openai",
            true,
            vec![inpainter("img", "Image 1")],
        )],
        vec![],
    );
    w.save_key("oa");
    let view = w.picker(&fill);
    assert_eq!(titles(&view), [("Label oa".to_owned(), GroupState::Ready)]);
}

#[test]
fn the_local_model_is_greyed_until_a_local_connection_offers_one_for_the_tool() {
    let w = World::new(vec![], vec![]);
    let view = w.picker(&select_subject());
    assert_eq!(titles(&view), [("SAM 3".to_owned(), GroupState::NotSetUp)]);
    assert_eq!(view.groups[0].locality, Some(Locality::Local));
    assert_eq!(
        view.groups[0].setup,
        Some(SetupLink::New {
            option: "local:select-subject".into()
        })
    );

    let w = World::new(
        vec![connection(
            "box",
            "comfyui",
            false,
            vec![segmenter("sam3", "SAM 3")],
        )],
        vec![],
    );
    let view = w.picker(&select_subject());
    assert_eq!(titles(&view), [("Label box".to_owned(), GroupState::Ready)]);
}

#[test]
fn setup_options_for_other_tools_or_that_cannot_run_yet_are_not_offered() {
    let w = World::new(vec![], vec![]);
    let option = |id: &str, unlocks: &str, usable: bool| SetupOption {
        id: format!("local:{id}"),
        title: format!("Model {id}"),
        note: String::new(),
        route: Route::Local { feature: id.into() },
        unlocks: vec![unlocks.into()],
        usable,
        size_bytes: None,
    };
    let options = [
        option("other", "remove-object", true),
        option("later", "select-subject", false),
        option("now", "select-subject", true),
    ];
    let view = w
        .client("photocraft")
        .picker(&select_subject(), &options)
        .unwrap();
    assert_eq!(
        titles(&view),
        [("Model now".to_owned(), GroupState::NotSetUp)]
    );
}

#[test]
fn the_model_in_use_is_marked_and_a_choice_that_stopped_working_says_why() {
    let role = select_subject();
    // Working: marked, no problem.
    let w = World::new(
        vec![connection(
            "box",
            "comfyui",
            false,
            vec![segmenter("sam3", "SAM 3")],
        )],
        vec![bound(&role, "box", "sam3")],
    );
    let view = w.picker(&role);
    let current = view.current.clone().unwrap();
    assert_eq!((current.label.as_str(), current.problem), ("SAM 3", None));
    assert!(view.groups[0].models[0].selected);

    // No key saved: still marked, with the reason.
    let w = World::new(
        vec![connection(
            "cloud",
            "openai",
            true,
            vec![segmenter("x", "X")],
        )],
        vec![bound(&role, "cloud", "x")],
    );
    let view = w.picker(&role);
    let problem = view.current.clone().unwrap().problem.unwrap();
    assert!(problem.contains("no key"), "{problem}");
    assert!(view.groups[0].models[0].selected);

    // The connection was removed: nothing to mark, the reason is still shown.
    let w = World::new(vec![], vec![bound(&role, "gone", "x")]);
    let view = w.picker(&role);
    let current = view.current.clone().unwrap();
    assert_eq!(current.label, "x");
    assert!(current.problem.is_some());
    assert!(
        view.groups
            .iter()
            .all(|g| g.models.iter().all(|m| !m.selected))
    );
}

#[test]
fn with_no_choice_nothing_is_current_and_a_picker_never_carries_a_key() {
    let w = World::new(
        vec![connection(
            "cloud",
            "openai",
            true,
            vec![segmenter("x", "X")],
        )],
        vec![],
    );
    w.save_key("cloud");
    let view = w.picker(&select_subject());
    assert!(view.current.is_none());
    assert!(!format!("{view:?}").contains(KEY));
}

#[test]
fn an_apps_own_choice_is_the_one_marked_over_the_launcher_default() {
    let role = select_subject();
    let w = World::new(
        vec![connection(
            "box",
            "comfyui",
            false,
            vec![segmenter("sam3", "SAM 3"), segmenter("alt", "Alt")],
        )],
        vec![bound(&role, "box", "sam3")],
    );
    let client = w.client("lightcraft");
    client
        .bind(
            &role,
            &ConnectionId::new("box").unwrap(),
            &ModelId::new("alt").unwrap(),
        )
        .unwrap();
    let view = client.picker(&role, &[]).unwrap();
    let marked: Vec<_> = view.groups[0]
        .models
        .iter()
        .map(|m| (m.label.as_str(), m.selected))
        .collect();
    assert_eq!(marked, [("SAM 3", false), ("Alt", true)]);
}

#[test]
fn an_uninstalled_local_model_carries_its_download_size_and_a_hosted_service_none() {
    let w = World::new(vec![], vec![]);
    let local = w.picker(&select_subject());
    assert_eq!(local.groups[0].size_bytes, Some(3_449_860_632));
    let hosted = w.picker(&builtin_role("generative-fill"));
    assert_eq!(hosted.groups[0].size_bytes, None);

    // A connection that is only switched off downloads nothing to fix.
    let mut off = connection("off", "segment-co", false, vec![segmenter("a", "Alpha")]);
    off.enabled = false;
    let w = World::new(vec![off], vec![]);
    let view = w.picker(&select_subject());
    let off_group = view.groups.iter().find(|g| g.title == "Label off").unwrap();
    assert_eq!(off_group.size_bytes, None);
}

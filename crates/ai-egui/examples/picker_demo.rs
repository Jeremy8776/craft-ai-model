//! A window to look at the model picker with sample data. Your real connections are never read.
//!
//! ```text
//! cargo run -p craft-ai-egui --example picker_demo -j 8
//! ```
//!
//! The store is a temporary folder with made-up providers, so nothing here touches your keys,
//! your launcher choices or the network. Picking a model really binds it in that sample store
//! (through `ToolDesk::choose`), so the "in use" mark moves. Set up links only say what they would
//! open.

use std::sync::Arc;

use craft_ai_client::{AiClient, PickerView, SetupLink, ToolDesk};
use craft_ai_core::{
    AppId, Binding, CapabilityId, CapabilitySpec, Connection, ConnectionId, InputRole, ModelId,
    ModelInfo, Role, SecretRef, SecretString, StoreFile,
};
use craft_ai_egui::{PickerAction, PopupAction, missing_model_popup, model_picker};
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use serde_json::json;

const TOOLS: [&str; 2] = ["select-subject", "generative-fill"];

fn model(id: &str, label: &str, capability: &str, inputs: &[InputRole]) -> ModelInfo {
    let mut spec = CapabilitySpec::new(CapabilityId::parse(capability).unwrap_or_else(|_| {
        CapabilityId::parse("image.segment").expect("a built-in capability id parses")
    }));
    for input in inputs {
        spec = spec.accepting(input.clone(), 1);
    }
    ModelInfo::new(
        ModelId::new(id).expect("a literal model id is valid"),
        label,
    )
    .with(spec)
}

fn connection(
    id: &str,
    label: &str,
    provider: &str,
    remote: bool,
    on: bool,
    models: Vec<ModelInfo>,
) -> Connection {
    serde_json::from_value(json!({
        "id": id, "label": label, "provider": provider,
        "endpoint": if remote { "https://api.example.invalid/v1" } else { "http://127.0.0.1:9" },
        "locality": if remote { "remote" } else { "local" },
        "auth": if remote { json!({"kind": "api_key", "secret": format!("{id}-key")}) } else { json!({"kind": "none"}) },
        "billing": if remote { "metered" } else { "free" },
        "enabled": on,
        "models": models,
    }))
    .expect("the sample connection is well formed")
}

struct Sample {
    _dir: tempfile::TempDir,
    shared: SharedStore,
    secrets: Arc<MemoryStore>,
}

fn build(sam3_installed: bool) -> Sample {
    let seg = [InputRole::Source, InputRole::Prompt];
    let mut connections = Vec::new();
    if sam3_installed {
        connections.push(connection(
            "box",
            "On this computer",
            "comfyui",
            false,
            true,
            vec![
                model("sam3", "SAM 3", "image.segment", &seg),
                model(
                    "yolo",
                    "YOLO detector",
                    "image.segment",
                    &[InputRole::Source],
                ),
            ],
        ));
    }
    connections.push(connection(
        "cloud",
        "Segment Cloud",
        "segment-co",
        true,
        true,
        vec![model("xl", "Segment XL", "image.segment", &seg)],
    ));
    connections.push(connection(
        "hub",
        "Vision Hub",
        "vision-hub",
        true,
        true,
        vec![model("hub-seg", "Hub Segmenter", "image.segment", &seg)],
    ));
    connections.push(connection(
        "lab",
        "Old Lab",
        "old-lab",
        false,
        false,
        vec![model("lab-seg", "Lab Segmenter", "image.segment", &seg)],
    ));
    let dir = tempfile::tempdir().expect("a temporary folder");
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let choose = |role: &str, conn: &str, model: &str| Binding {
        role: craft_ai_core::RoleId::new(role).expect("a literal role id"),
        connection: ConnectionId::new(conn).expect("a literal id"),
        model: ModelId::new(model).expect("a literal id"),
    };
    // The launcher's choice points at a provider with no key, so the picker shows why it fails.
    let defaults = vec![choose("select-subject", "hub", "hub-seg")];
    let _ = shared.replace(&StoreFile {
        connections,
        defaults,
        ..StoreFile::default()
    });
    let secrets = Arc::new(MemoryStore::new());
    let _ = secrets.set(
        &SecretRef::new("cloud-key").expect("a literal ref"),
        &SecretString::new("sample"),
    );
    Sample {
        _dir: dir,
        shared,
        secrets,
    }
}

struct Demo {
    sample: Sample,
    sam3_installed: bool,
    tool: usize,
    as_popup: bool,
    dismissed: bool,
    view: PickerView,
    note: String,
}

impl Demo {
    fn role(&self) -> Role {
        craft_ai_features::builtin()
            .into_iter()
            .find(|f| f.id == TOOLS[self.tool])
            .and_then(|f| f.role())
            .expect("the tool is in the catalogue")
    }

    fn client(&self, app: &str) -> AiClient {
        AiClient::new(
            AppId::new(app).expect("a literal app id"),
            self.sample.shared.clone(),
            self.sample.secrets.clone(),
        )
    }

    fn refresh(&mut self) {
        let options = craft_ai_features::setup_options(&craft_ai_features::builtin());
        if let Ok(view) = self.client("demo").picker(&self.role(), &options) {
            self.view = view;
        }
    }
}

impl Demo {
    /// What an app does when its tool has no model: ask `ToolDesk::missing_model` and, if there is
    /// something to show, put the popup content in a window.
    fn popup_ui(&mut self, ui: &mut egui::Ui) {
        let desk = ToolDesk::new(self.client("photocraft"), self.sample.shared.clone(), None);
        let missing = desk.missing_model(&self.role(), &craft_ai_features::builtin());
        let Ok(Some(popup)) = missing else {
            ui.label(format!(
                "{} is ready, so the app shows no popup.",
                TOOLS[self.tool]
            ));
            return;
        };
        if self.dismissed {
            ui.label(format!("{} stays off for now (Not now).", popup.title));
            return;
        }
        let mut action = None;
        egui::Window::new("popup")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| action = missing_model_popup(ui, &popup));
        match action {
            Some(PopupAction::Choose { connection, model }) => {
                // Bound for every app, not just this one.
                self.note = match desk.choose(&self.role(), &connection, &model) {
                    Ok(()) => format!("Chose {model} on {connection} for every app."),
                    Err(e) => format!("Refused: {e}"),
                };
                self.refresh();
            }
            Some(PopupAction::SetUp(SetupLink::New { option })) => {
                self.note = format!("Would open set up for {option}.");
            }
            Some(PopupAction::SetUp(SetupLink::Fix { connection })) => {
                self.note = format!("Would open the launcher to fix {connection}.");
            }
            Some(PopupAction::NotNow) => self.dismissed = true,
            Some(PopupAction::InstallComfyUi(_)) => {
                self.note = "This sample does not install software.".into();
            }
            None => {}
        }
    }
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.label(
                egui::RichText::new("Model picker (sample data, nothing real is read)").weak(),
            );
            let before = (self.tool, self.sam3_installed);
            ui.horizontal(|ui| {
                for (i, id) in TOOLS.iter().enumerate() {
                    ui.selectable_value(&mut self.tool, i, *id);
                }
                ui.checkbox(&mut self.sam3_installed, "SAM 3 installed");
                if ui
                    .checkbox(&mut self.as_popup, "As the in-app popup")
                    .changed()
                {
                    self.dismissed = false;
                }
            });
            if before.1 != self.sam3_installed {
                self.sample = build(self.sam3_installed);
            }
            if before != (self.tool, self.sam3_installed) {
                self.refresh();
            }
            ui.separator();
            if self.as_popup {
                self.popup_ui(ui);
                if !self.note.is_empty() {
                    ui.separator();
                    ui.label(&self.note);
                }
                return;
            }
            match model_picker(ui, &self.view) {
                Some(PickerAction::Choose { connection, model }) => {
                    let desk =
                        ToolDesk::new(self.client("launcher"), self.sample.shared.clone(), None);
                    self.note = match desk.choose(&self.role(), &connection, &model) {
                        Ok(()) => format!("Chose {model} on {connection}."),
                        Err(e) => format!("Refused: {e}"),
                    };
                    self.refresh();
                }
                Some(PickerAction::SetUp(SetupLink::New { option })) => {
                    self.note = format!("Would open set up for {option}.");
                }
                Some(PickerAction::SetUp(SetupLink::Fix { connection })) => {
                    self.note = format!("Would open the launcher to fix {connection}.");
                }
                None => {}
            }
            if !self.note.is_empty() {
                ui.separator();
                ui.label(&self.note);
            }
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([560.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Craft AI model picker",
        options,
        Box::new(|_cc| {
            let mut demo = Demo {
                sample: build(true),
                sam3_installed: true,
                tool: 0,
                as_popup: false,
                dismissed: false,
                view: PickerView {
                    tool: craft_ai_core::RoleId::new("select-subject").expect("a literal role id"),
                    title: String::new(),
                    current: None,
                    presets: Vec::new(),
                    groups: Vec::new(),
                },
                note: String::new(),
            };
            demo.refresh();
            Ok(Box::new(demo))
        }),
    )
}

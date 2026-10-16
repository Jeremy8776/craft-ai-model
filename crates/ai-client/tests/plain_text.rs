//! Everything a person can read from the shared layer is plain language: picker titles, notes and
//! descriptions, the reason a tool cannot run, and the errors an app may show. Engine names, card
//! jargon and developer words stay in the data, never in a sentence.
use std::sync::Arc;

use craft_ai_client::{AiClient, ToolDesk};
use craft_ai_core::{AppId, ConnectionId, Role, StoreFile};
use craft_ai_features::{Feature, Gpu, GpuVendor, Os, SystemProfile};
use craft_ai_native::secrets::MemoryStore;
use craft_ai_native::shared::SharedStore;

const JARGON: &[&str] = &[
    "candle",
    "burn",
    "cuda",
    "metal",
    "wgpu",
    "vulkan",
    "directx",
    "rocm",
    "rdna",
    "vram",
    "mib",
    "adapter",
    "provider",
    "backend",
    "gpu",
    "cpu",
    "onnx",
    "ort",
    "wasm",
    "safetensors",
    "inference",
    "runtime",
    "tensor",
    "checkpoint",
    "weights",
    "accelerator",
    "endpoint",
    "connection",
    "native",
    "route",
];

fn jargon_in(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .filter(|w| JARGON.contains(&w.as_str()))
        .collect()
}

fn feature() -> Feature {
    craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == "select-subject")
        .unwrap()
}

fn role() -> Role {
    feature().role().unwrap()
}

struct World {
    dir: tempfile::TempDir,
    shared: SharedStore,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let shared = SharedStore::new(dir.path().join("connections.json"));
        shared.replace(&StoreFile::default()).unwrap();
        Self { dir, shared }
    }
    fn client(&self, app: &str) -> AiClient {
        AiClient::new(
            AppId::new(app).unwrap(),
            self.shared.clone(),
            Arc::new(MemoryStore::new()),
        )
    }
    fn desk(&self) -> ToolDesk {
        ToolDesk::new(self.client("launcher"), self.shared.clone(), None)
    }
    fn choose(&self, connection: &str) {
        let store = self.shared.load().unwrap();
        let model = store
            .connections
            .iter()
            .find(|c| c.id.as_str() == connection)
            .and_then(|c| c.models.first())
            .map(|m| m.id.clone())
            .unwrap();
        self.desk()
            .choose(&role(), &ConnectionId::new(connection).unwrap(), &model)
            .unwrap();
    }
}

/// Every connection of the select-subject tool: ComfyUI, and both built-in engines.
fn full_world() -> (World, std::path::PathBuf) {
    let w = World::new();
    let file = w.dir.path().join("sam3.safetensors");
    std::fs::write(&file, b"x").unwrap();
    let f = feature();
    w.desk().enable_local(&f, "http://127.0.0.1:8000").unwrap();
    w.desk().enable_native(&f, &file).unwrap();
    w.desk().enable_native_with(&f, "burn", &file).unwrap();
    (w, file)
}

fn picker_text(client: &AiClient) -> Vec<String> {
    let advice = client.engine_advice(
        &feature(),
        &SystemProfile {
            os: Os::Windows,
            gpus: vec![Gpu {
                vendor: GpuVendor::Nvidia,
                name: "Test Card".to_owned(),
                vram_mb: Some(8_192),
            }],
            comfyui_found: true,
        },
    );
    let view = client
        .picker_with_advice(
            &role(),
            &craft_ai_features::setup_options(&craft_ai_features::builtin()),
            &advice,
        )
        .unwrap();
    let mut out = vec![view.title.clone()];
    if let Some(current) = &view.current {
        out.push(current.label.clone());
        out.extend(current.problem.clone());
    }
    for g in &view.groups {
        out.push(g.title.clone());
        out.push(g.note.clone());
        for m in &g.models {
            out.push(m.label.clone());
            out.push(m.description.clone());
        }
    }
    out.extend(advice.into_iter().map(|a| a.reason));
    out.into_iter().filter(|s| !s.is_empty()).collect()
}

#[test]
fn the_picker_and_the_reason_a_tool_cannot_run_are_plain_whatever_is_chosen() {
    let (w, _file) = full_world();
    let mut texts = Vec::new();
    for connection in [
        "local-select-subject",
        "local-select-subject-candle",
        "local-select-subject-burn",
    ] {
        w.choose(connection);
        for app in ["photocraft", "lightcraft"] {
            let client = w.client(app);
            texts.extend(picker_text(&client));
            if let craft_ai_client::RoleStatus::Unavailable { reason } = client.status(&role()) {
                texts.push(reason);
            }
        }
    }
    assert!(
        texts.len() > 30,
        "the scenario must really run: {}",
        texts.len()
    );
    for t in &texts {
        let found = jargon_in(t);
        assert!(found.is_empty(), "{found:?} in \"{t}\"");
    }
}

/// The only sentences the route errors may use. A new message must be added here on purpose,
/// after someone has read it as a person would.
const ALLOWED_ERRORS: &[&str] = &[
    "The model you chose can't be run by this app yet. Choose another in Set up AI.",
    "The model you chose isn't fully set up. Open Set up AI to fix it.",
    "The file for the model you chose is no longer there. Open Set up AI to fix it.",
    "The model you chose runs online, not on this computer.",
];

#[test]
fn the_errors_an_app_may_show_are_plain() {
    let (w, file) = full_world();
    let client = w.client("photocraft");
    let mut texts = Vec::new();
    let ask = |texts: &mut Vec<String>| {
        for provider in ["candle", "burn"] {
            if let Err(e) = client.local_file_for(&role(), provider) {
                texts.push(e.message);
            }
        }
        if let Err(e) = client.endpoint_for(&role()) {
            texts.push(e.message);
        }
        if let Err(e) = client.local_route_for(&role()) {
            texts.push(e.message);
        }
    };
    // Each engine chosen, asked for by the wrong engine's adapter.
    for connection in [
        "local-select-subject",
        "local-select-subject-candle",
        "local-select-subject-burn",
    ] {
        w.choose(connection);
        ask(&mut texts);
    }
    // A weights file that has vanished.
    w.choose("local-select-subject-burn");
    std::fs::remove_file(&file).unwrap();
    ask(&mut texts);
    std::fs::write(&file, b"x").unwrap();
    // A built-in connection with no file recorded, and ComfyUI with no address.
    w.shared
        .update(|store| {
            for c in &mut store.connections {
                if c.provider == "burn" {
                    for m in &mut c.models {
                        m.local_file = None;
                    }
                }
                if c.provider == "comfyui" {
                    c.endpoint = None;
                }
            }
        })
        .unwrap();
    ask(&mut texts);
    w.choose("local-select-subject");
    ask(&mut texts);
    // A choice that has become remote.
    w.shared
        .update(|store| {
            for c in &mut store.connections {
                if c.provider == "comfyui" {
                    c.locality = craft_ai_core::Locality::Remote;
                    c.endpoint = Some("https://example.com".to_owned());
                }
            }
        })
        .unwrap();
    ask(&mut texts);
    for allowed in ALLOWED_ERRORS {
        assert!(
            texts.iter().any(|t| t == allowed),
            "the scenario never produced: {allowed}"
        );
    }
    for t in &texts {
        assert!(
            ALLOWED_ERRORS.contains(&t.as_str()),
            "unreviewed message: {t}"
        );
        assert!(jargon_in(t).is_empty(), "{t}");
    }
}

#[test]
fn the_plain_text_check_itself_catches_jargon() {
    assert_eq!(
        jargon_in("This app has no adapter for candle."),
        ["adapter", "candle"]
    );
    assert!(jargon_in("This app can't use this option yet.").is_empty());
}

//! The client turns the catalogue's engines and this app's adapters into advice, and advice never
//! changes a choice.
use std::sync::Arc;

use craft_ai_client::{AiClient, ProviderFactory, ToolDesk};
use craft_ai_core::{AiError, AppId, StoreFile};
use craft_ai_features::{Feature, Gpu, GpuVendor, Os, Pace, SystemProfile};
use craft_ai_native::secrets::MemoryStore;
use craft_ai_native::shared::SharedStore;

fn select_subject() -> Feature {
    craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == "select-subject")
        .unwrap()
}

fn windows_nvidia() -> SystemProfile {
    SystemProfile {
        os: Os::Windows,
        gpus: vec![Gpu {
            vendor: GpuVendor::Nvidia,
            name: "RTX 3090 Ti".to_owned(),
            vram_mb: Some(24_576),
        }],
        comfyui_found: true,
    }
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
    fn client(&self) -> AiClient {
        AiClient::new(
            AppId::new("photocraft").unwrap(),
            self.shared.clone(),
            Arc::new(MemoryStore::new()),
        )
    }
}

#[test]
fn an_app_that_only_has_comfyui_is_advised_comfyui_and_the_built_in_ones_are_listed_unusable() {
    let w = World::new();
    let advice = w
        .client()
        .engine_advice(&select_subject(), &windows_nvidia());
    let providers: Vec<&str> = advice.iter().map(|a| a.provider.as_str()).collect();
    assert_eq!(providers, ["comfyui", "candle", "burn"]);
    assert!(advice[0].recommended && advice[0].usable);
    assert!(advice[1..].iter().all(|a| !a.usable && !a.recommended));
}

#[test]
fn an_app_with_a_candle_adapter_is_advised_candle_on_nvidia() {
    let w = World::new();
    let mut factory = ProviderFactory::standard();
    factory.register("candle", |_, _| {
        Err(AiError::unsupported("advice never builds a provider"))
    });
    factory.declare_accelerators("candle", &[GpuVendor::Nvidia]);
    let client = w.client().with_factory(factory);
    let advice = client.engine_advice(&select_subject(), &windows_nvidia());
    assert_eq!(advice[0].provider, "candle");
    assert!(advice[0].recommended);
    assert_eq!(advice[0].pace, Some(Pace::Gpu));
    assert_eq!(
        advice.iter().filter(|a| a.recommended).count(),
        1,
        "{advice:?}"
    );
}

#[test]
fn a_tool_with_no_local_recipe_has_no_engine_advice() {
    let w = World::new();
    let generative = craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.local.is_none())
        .unwrap();
    assert!(
        w.client()
            .engine_advice(&generative, &windows_nvidia())
            .is_empty()
    );
    let mut f = select_subject();
    f.local.as_mut().unwrap().usable = false;
    assert!(w.client().engine_advice(&f, &windows_nvidia()).is_empty());
}

#[test]
fn advice_leaves_the_store_and_a_persons_choice_alone() {
    let w = World::new();
    let desk = ToolDesk::new(w.client(), w.shared.clone(), None);
    let file = w.dir.path().join("sam3.safetensors");
    std::fs::write(&file, b"x").unwrap();
    desk.enable_native_with(&select_subject(), "burn", &file)
        .unwrap();
    let before = std::fs::read(w.dir.path().join("connections.json")).unwrap();
    let advice = w
        .client()
        .engine_advice(&select_subject(), &windows_nvidia());
    assert!(advice.iter().any(|a| a.recommended));
    let after = std::fs::read(w.dir.path().join("connections.json")).unwrap();
    assert_eq!(before, after, "suggesting must not write anything");
    let role = select_subject().role().unwrap();
    assert_eq!(
        w.client().local_route_for(&role).unwrap().unwrap().provider,
        "burn",
        "the person's bound choice stays, whatever is suggested"
    );
}

#[test]
fn a_build_declared_cpu_only_is_advised_as_the_processor_and_loses_to_a_gpu_engine() {
    let w = World::new();
    let mut factory = ProviderFactory::standard();
    for name in ["candle", "burn"] {
        factory.register(name, |_, _| Err(AiError::unsupported("never built")));
    }
    // LightCraft's Candle is built with Metal only: nothing on Windows.
    factory.declare_accelerators("candle", &[]);
    factory.declare_accelerators("burn", &[GpuVendor::Nvidia]);
    let advice = w
        .client()
        .with_factory(factory)
        .engine_advice(&select_subject(), &windows_nvidia());
    let candle = advice.iter().find(|a| a.provider == "candle").unwrap();
    assert_eq!(candle.pace, Some(Pace::Cpu));
    assert_eq!(advice[0].provider, "burn");
    assert!(advice[0].recommended);
}

fn enabled_world() -> (World, craft_ai_core::Role) {
    let w = World::new();
    let file = w.dir.path().join("sam3.safetensors");
    std::fs::write(&file, b"x").unwrap();
    let desk = ToolDesk::new(w.client(), w.shared.clone(), None);
    desk.enable_local(&select_subject(), "http://127.0.0.1:8000")
        .unwrap();
    desk.enable_native_with(&select_subject(), "burn", &file)
        .unwrap();
    let role = select_subject().role().unwrap();
    (w, role)
}

#[test]
fn the_picker_marks_only_the_recommended_ready_provider_and_never_the_others() {
    let (w, role) = enabled_world();
    let mut factory = ProviderFactory::standard();
    factory.register("burn", |_, _| Err(AiError::unsupported("never built")));
    factory.declare_accelerators("burn", &[GpuVendor::Nvidia]);
    let client = w.client().with_factory(factory);
    let advice = client.engine_advice(&select_subject(), &windows_nvidia());
    assert_eq!(advice[0].provider, "burn");
    let view = client.picker_with_advice(&role, &[], &advice).unwrap();
    let suggested: Vec<&str> = view
        .groups
        .iter()
        .filter(|g| g.suggested)
        .map(|g| g.title.as_str())
        .collect();
    assert_eq!(suggested.len(), 1, "{suggested:?}");
    assert!(suggested[0].contains("any graphics card"), "{suggested:?}");
    assert!(
        client
            .picker(&role, &[])
            .unwrap()
            .groups
            .iter()
            .all(|g| !g.suggested),
        "the plain picker suggests nothing"
    );
}

#[test]
fn no_recommendation_marks_nothing_and_leaves_the_current_choice_selected() {
    let (w, role) = enabled_world();
    let client = w.client();
    let none = client.picker_with_advice(&role, &[], &[]).unwrap();
    assert!(none.groups.iter().all(|g| !g.suggested));
    let advice = client.engine_advice(&select_subject(), &windows_nvidia());
    let view = client.picker_with_advice(&role, &[], &advice).unwrap();
    let plain = client.picker(&role, &[]).unwrap();
    assert_eq!(
        view.current, plain.current,
        "a suggestion never changes the choice"
    );
}

#[test]
fn a_recommended_provider_that_is_switched_off_is_not_marked() {
    let (w, role) = enabled_world();
    w.shared
        .update(|store| {
            for c in &mut store.connections {
                if c.provider == "burn" {
                    c.enabled = false;
                }
            }
        })
        .unwrap();
    let mut factory = ProviderFactory::standard();
    factory.register("burn", |_, _| Err(AiError::unsupported("never built")));
    factory.declare_accelerators("burn", &[GpuVendor::Nvidia]);
    let client = w.client().with_factory(factory);
    let advice = client.engine_advice(&select_subject(), &windows_nvidia());
    assert_eq!(
        advice[0].provider, "burn",
        "advice itself knows nothing of switches"
    );
    let view = client.picker_with_advice(&role, &[], &advice).unwrap();
    assert!(
        view.groups.iter().all(|g| !g.suggested),
        "a switched-off provider cannot be suggested"
    );
}

#[test]
fn an_engine_the_app_registers_without_vouching_for_its_build_is_not_promised_the_card() {
    let w = World::new();
    let mut factory = ProviderFactory::standard();
    factory.register("candle", |_, _| Err(AiError::unsupported("never built")));
    let advice = w
        .client()
        .with_factory(factory)
        .engine_advice(&select_subject(), &windows_nvidia());
    let candle = advice.iter().find(|a| a.provider == "candle").unwrap();
    assert_eq!(candle.pace, Some(Pace::Unsure), "{}", candle.reason);
    assert_eq!(
        advice[0].provider, "comfyui",
        "a sure graphics card beats an unsure one"
    );
}

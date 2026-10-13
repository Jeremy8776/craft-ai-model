//! Engine advice: what is suggested for which machine, the rules that must never bend, and that
//! every sentence a person could read is plain language.
use craft_ai_features::{
    EngineAdvice, EngineOffer, Gpu, GpuVendor, Os, Pace, SystemProfile, advise, gpu_route,
};

fn gpu(vendor: GpuVendor, name: &str, vram_mb: Option<u32>) -> Gpu {
    Gpu {
        vendor,
        name: name.to_owned(),
        vram_mb,
    }
}

fn machine(os: Os, gpus: Vec<Gpu>, comfyui_found: bool) -> SystemProfile {
    SystemProfile {
        os,
        gpus,
        comfyui_found,
    }
}

const ALL_VENDORS: [GpuVendor; 5] = [
    GpuVendor::Nvidia,
    GpuVendor::Amd,
    GpuVendor::Intel,
    GpuVendor::Apple,
    GpuVendor::Other,
];

/// Offers for the three engines. The app has said nothing about its build.
fn offers(present: &[&str]) -> Vec<EngineOffer> {
    ["candle", "burn", "comfyui"]
        .iter()
        .map(|p| EngineOffer {
            provider: (*p).to_owned(),
            adapter_present: present.contains(p),
            accelerators: None,
        })
        .collect()
}

/// Offers where the app has stated its built-in engines reach what the table says they can.
fn built(present: &[&str], os: Os) -> Vec<EngineOffer> {
    let mut o = offers(present);
    for offer in &mut o {
        if offer.provider != "comfyui" {
            offer.accelerators = Some(
                ALL_VENDORS
                    .iter()
                    .copied()
                    .filter(|v| gpu_route(&offer.provider, os, *v))
                    .collect(),
            );
        }
    }
    o
}

fn pick(advice: &[EngineAdvice]) -> Option<&str> {
    let all: Vec<&EngineAdvice> = advice.iter().filter(|a| a.recommended).collect();
    assert!(all.len() <= 1, "at most one engine is recommended");
    all.first().map(|a| a.provider.as_str())
}

fn find<'a>(advice: &'a [EngineAdvice], provider: &str) -> &'a EngineAdvice {
    advice.iter().find(|a| a.provider == provider).unwrap()
}

#[test]
fn nvidia_on_windows_prefers_a_built_in_engine_over_the_comfyui_server() {
    let m = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Nvidia, "RTX 3090 Ti", Some(24_576))],
        true,
    );
    let advice = advise(&m, &built(&["candle", "burn", "comfyui"], m.os), None);
    assert_eq!(
        pick(&advice),
        Some("candle"),
        "the recipe lists candle first"
    );
    assert_eq!(advice[2].provider, "comfyui");
    assert!(advice.iter().all(|a| a.pace == Some(Pace::Gpu)));
    assert!(
        advice[0].reason.contains("RTX 3090 Ti"),
        "{}",
        advice[0].reason
    );
}

#[test]
fn an_amd_card_on_windows_gets_burn_and_comfyui_but_not_candle() {
    let m = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Amd, "Radeon RX 7800 XT", Some(16_384))],
        true,
    );
    let advice = advise(&m, &built(&["candle", "burn", "comfyui"], m.os), None);
    assert_eq!(
        pick(&advice),
        Some("burn"),
        "built in, and the card is reached"
    );
    assert_eq!(find(&advice, "candle").pace, Some(Pace::Cpu));
    let comfy = find(&advice, "comfyui");
    assert_eq!(
        comfy.pace,
        Some(Pace::Gpu),
        "ComfyUI documents AMD on Windows 11"
    );
    assert!(comfy.reason.contains("recent AMD"), "{}", comfy.reason);
}

#[test]
fn an_intel_card_gets_burn_and_apple_silicon_gets_a_built_in_engine() {
    let intel = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Intel, "Arc A770", Some(16_384))],
        false,
    );
    let advice = advise(&intel, &built(&["candle", "burn"], intel.os), None);
    assert_eq!(pick(&advice), Some("burn"));
    let mac = machine(
        Os::MacOs,
        vec![gpu(GpuVendor::Apple, "Apple M3", None)],
        false,
    );
    let advice = advise(&mac, &built(&["candle", "burn"], mac.os), None);
    assert_eq!(pick(&advice), Some("candle"));
    assert!(
        advice
            .iter()
            .filter(|a| a.usable)
            .all(|a| a.pace == Some(Pace::Gpu))
    );
}

#[test]
fn no_graphics_card_is_recommended_as_the_best_available_with_a_slow_warning() {
    let m = machine(Os::Linux, vec![], false);
    let advice = advise(&m, &built(&["candle", "burn"], m.os), None);
    assert_eq!(pick(&advice), Some("candle"));
    let top = &advice[0];
    assert_eq!(top.pace, Some(Pace::Cpu));
    assert!(
        top.reason.contains("No graphics card found"),
        "{}",
        top.reason
    );
    assert!(top.reason.contains("it will be slow"), "{}", top.reason);
}

#[test]
fn an_engine_this_app_cannot_run_is_listed_but_never_recommended() {
    let m = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Nvidia, "RTX 4070", Some(12_288))],
        true,
    );
    let advice = advise(&m, &built(&["burn"], m.os), None);
    assert_eq!(pick(&advice), Some("burn"));
    let candle = find(&advice, "candle");
    assert!(!candle.usable && !candle.recommended && candle.pace.is_none());
    assert_eq!(candle.reason, "This app can't use this option yet.");
    assert_eq!(advice.len(), 3);
    assert!(advice.iter().skip(1).all(|a| !a.usable));
}

#[test]
fn nothing_usable_means_nothing_is_recommended() {
    let m = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Nvidia, "RTX 4070", None)],
        true,
    );
    assert_eq!(pick(&advise(&m, &offers(&[]), None)), None);
}

#[test]
fn comfyui_needs_an_install_even_when_the_app_has_its_adapter() {
    let m = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Nvidia, "RTX 4070", None)],
        false,
    );
    let advice = advise(&m, &offers(&["comfyui"]), None);
    let comfy = find(&advice, "comfyui");
    assert!(!comfy.usable, "{}", comfy.reason);
    assert_eq!(comfy.reason, "ComfyUI isn't installed on this computer.");
    assert_eq!(pick(&advice), None);
}

#[test]
fn comfyui_alone_is_recommended_when_it_is_all_there_is() {
    let m = machine(Os::Linux, vec![gpu(GpuVendor::Amd, "Radeon", None)], true);
    let advice = advise(&m, &offers(&["comfyui"]), None);
    assert_eq!(pick(&advice), Some("comfyui"));
    assert_eq!(find(&advice, "comfyui").pace, Some(Pace::Gpu));
}

#[test]
fn too_little_graphics_memory_turns_a_gpu_engine_into_a_slow_one() {
    let m = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Nvidia, "GTX 1650", Some(4_096))],
        true,
    );
    let advice = advise(&m, &built(&["candle", "burn"], m.os), Some(8_000));
    for a in advice.iter().filter(|a| a.usable) {
        assert_eq!(a.pace, Some(Pace::Cpu), "{}", a.reason);
        assert!(a.reason.contains("enough memory"), "{}", a.reason);
        assert!(
            !a.reason.contains("4096"),
            "no figures for a person to decode"
        );
    }
    let unknown = machine(Os::Windows, vec![gpu(GpuVendor::Nvidia, "RTX", None)], true);
    let advice = advise(&unknown, &built(&["candle"], unknown.os), Some(8_000));
    assert_eq!(
        advice[0].pace,
        Some(Pace::Gpu),
        "unknown memory is not treated as too small"
    );
}

#[test]
fn a_gpu_engine_beats_a_cpu_engine_whatever_the_listing_order() {
    let m = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Amd, "Radeon", Some(8_192))],
        true,
    );
    // candle is listed first but has no AMD route; burn is listed second and has.
    let advice = advise(&m, &built(&["candle", "burn"], m.os), None);
    assert_eq!(advice[0].provider, "burn");
}

#[test]
fn the_second_card_counts_when_the_first_has_no_route() {
    let m = machine(
        Os::Windows,
        vec![
            gpu(GpuVendor::Intel, "Integrated", None),
            gpu(GpuVendor::Nvidia, "RTX 4090", Some(24_576)),
        ],
        false,
    );
    let advice = advise(&m, &built(&["candle"], m.os), None);
    assert_eq!(advice[0].pace, Some(Pace::Gpu));
    assert!(
        advice[0].reason.contains("RTX 4090"),
        "{}",
        advice[0].reason
    );
}

#[test]
fn an_unknown_engine_is_never_suggested_over_a_known_gpu_one() {
    assert!(!gpu_route("tract", Os::Windows, GpuVendor::Nvidia));
    let m = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Nvidia, "RTX", None)],
        false,
    );
    let mut o = built(&["burn"], m.os);
    o.insert(
        0,
        EngineOffer {
            provider: "tract".to_owned(),
            adapter_present: true,
            accelerators: None,
        },
    );
    let advice = advise(&m, &o, None);
    assert_eq!(pick(&advice), Some("burn"));
}

#[test]
fn the_advice_serialises_for_a_launcher_screen() {
    let m = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Nvidia, "RTX", Some(8_192))],
        true,
    );
    let advice = advise(&m, &built(&["candle"], m.os), None);
    let json = serde_json::to_string(&advice).unwrap();
    assert!(json.contains("\"pace\":\"gpu\""), "{json}");
    let back: Vec<EngineAdvice> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, advice);
}

#[test]
fn a_built_in_gpu_engine_beats_comfyui_even_when_comfyui_is_listed_first() {
    let m = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Nvidia, "RTX 4070", Some(12_288))],
        true,
    );
    let mut o = built(&["candle", "burn", "comfyui"], m.os);
    o.reverse(); // comfyui, burn, candle
    let advice = advise(&m, &o, None);
    assert_eq!(
        advice[0].provider, "burn",
        "first built-in in the order given"
    );
    assert_eq!(advice[2].provider, "comfyui");
    assert_eq!(pick(&advice), Some("burn"));
}

#[test]
fn an_app_that_states_its_build_overrides_the_table() {
    let m = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Nvidia, "RTX 3090", Some(24_576))],
        false,
    );
    // LightCraft builds Candle with Metal only, so on this computer it runs on the main chip.
    let mut cpu_only = built(&["candle", "burn"], m.os);
    cpu_only[0].accelerators = Some(vec![]);
    let advice = advise(&m, &cpu_only, None);
    assert_eq!(find(&advice, "candle").pace, Some(Pace::Cpu));
    assert_eq!(
        pick(&advice),
        Some("burn"),
        "the GPU engine wins once candle is stated CPU-only"
    );
    let mut cuda = offers(&["candle"]);
    cuda[0].accelerators = Some(vec![GpuVendor::Nvidia]);
    let advice = advise(&m, &cuda, None);
    assert_eq!(advice[0].pace, Some(Pace::Gpu));
}

#[test]
fn a_built_in_engine_the_app_has_not_vouched_for_is_unsure_not_promised() {
    let m = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Nvidia, "RTX 3090", Some(24_576))],
        true,
    );
    let advice = advise(&m, &offers(&["candle", "burn", "comfyui"]), None);
    for p in ["candle", "burn"] {
        let a = find(&advice, p);
        assert_eq!(a.pace, Some(Pace::Unsure), "{p}");
        assert!(a.reason.contains("hasn't confirmed"), "{}", a.reason);
    }
    assert_eq!(
        find(&advice, "comfyui").pace,
        Some(Pace::Gpu),
        "ComfyUI brings its own build"
    );
    assert_eq!(
        pick(&advice),
        Some("comfyui"),
        "a sure GPU beats an unsure one"
    );
}

#[test]
fn an_unsure_engine_still_beats_one_that_will_certainly_be_slow() {
    let m = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Nvidia, "RTX 3090", Some(24_576))],
        false,
    );
    let mut o = offers(&["candle", "burn"]);
    o[0].accelerators = Some(vec![]); // candle stated CPU-only
    let advice = advise(&m, &o, None);
    assert_eq!(advice[0].provider, "burn");
    assert_eq!(advice[0].pace, Some(Pace::Unsure));
    assert_eq!(advice[1].pace, Some(Pace::Cpu));
}

#[test]
fn comfyui_reaches_intel_arc_but_not_nvidia_on_a_mac() {
    let intel = machine(
        Os::Windows,
        vec![gpu(GpuVendor::Intel, "Arc A770", None)],
        true,
    );
    let advice = advise(&intel, &offers(&["comfyui"]), None);
    assert_eq!(advice[0].pace, Some(Pace::Gpu));
    assert!(advice[0].reason.contains("Arc"), "{}", advice[0].reason);
    let mac = machine(
        Os::MacOs,
        vec![gpu(GpuVendor::Nvidia, "Old Nvidia", None)],
        true,
    );
    let advice = advise(&mac, &offers(&["comfyui"]), None);
    assert_eq!(advice[0].pace, Some(Pace::Cpu));
}

/// Words a person should never have to read. Matched whole-word, any case.
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
    "sam",
    "inference",
    "runtime",
    "tensor",
    "checkpoint",
    "weights",
    "accelerator",
    "build",
];

fn jargon_in(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .filter(|w| JARGON.contains(&w.as_str()))
        .collect()
}

#[test]
fn every_sentence_a_person_can_read_is_plain_language() {
    let systems = [
        (
            Os::Windows,
            vec![gpu(GpuVendor::Nvidia, "Test Card One", Some(2_048))],
        ),
        (
            Os::Windows,
            vec![gpu(GpuVendor::Amd, "Test Card Two", None)],
        ),
        (
            Os::Linux,
            vec![gpu(GpuVendor::Intel, "Test Card Three", Some(16_384))],
        ),
        (Os::MacOs, vec![gpu(GpuVendor::Apple, "Test Chip", None)]),
        (
            Os::Linux,
            vec![gpu(GpuVendor::Other, "Test Card Four", None)],
        ),
        (Os::Windows, vec![]),
    ];
    let mut checked = 0;
    for (os, gpus) in systems {
        for comfyui_found in [true, false] {
            let m = machine(os, gpus.clone(), comfyui_found);
            let sets = [
                offers(&["candle", "burn", "comfyui"]),
                built(&["candle", "burn", "comfyui"], os),
                offers(&["burn"]),
                offers(&[]),
            ];
            for offers in sets {
                for min in [None, Some(8_000)] {
                    for a in advise(&m, &offers, min) {
                        let found = jargon_in(&a.reason);
                        assert!(found.is_empty(), "{found:?} in \"{}\"", a.reason);
                        assert!(a.reason.ends_with('.'), "{}", a.reason);
                        assert!(a.reason.len() < 170, "too long to read: {}", a.reason);
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked > 200, "the matrix must really run: {checked}");
}

#[test]
fn the_jargon_check_itself_catches_jargon() {
    assert_eq!(
        jargon_in("Runs on CUDA through Candle."),
        ["cuda", "candle"]
    );
    assert!(jargon_in("Should use your graphics card (RTX 3090).").is_empty());
    assert!(
        jargon_in("It will be slow, so sort the support out.").is_empty(),
        "whole words only"
    );
}

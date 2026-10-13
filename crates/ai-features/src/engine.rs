//! Which local engine suits this computer. Pure: the caller describes the machine and the engines
//! it can offer, and gets a ranked list with a reason for every line.
//!
//! This only ever suggests. It never binds a tool: a person can pick any engine, and a pick made
//! on purpose is left alone. It also states no speed winner.
//!
//! What a person reads is the `reason` sentence, so it is plain language: no engine names, no
//! hardware jargon, no numbers they would have to interpret. The engine ids (`candle`, `burn`,
//! `comfyui`) are data for the host to match on and are never put in a sentence.
//!
//! Three facts decide an engine's [`Pace`]. Which card makers an engine can drive is the table in
//! [`gpu_route`], read from each project's README on 13/10/2026 and not measured here. Whether an
//! app's own build includes that support is the app's to state with
//! [`EngineOffer::accelerators`] (LightCraft builds Candle with Metal only, so on Windows and Linux
//! its Candle runs on the main chip). An engine that is built into the app and has no statement is
//! [`Pace::Unsure`]: it is never promised the graphics card. ComfyUI brings its own build, so it
//! needs no statement. Between engines of the same pace, the one with fewer moving parts wins
//! (built into the app over a separate ComfyUI), then the order the recipe lists them in.

use serde::{Deserialize, Serialize};

/// The operating system, as far as engine choice cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    Windows,
    Linux,
    MacOs,
    Other,
}

/// Who made a graphics adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Other,
}

/// One graphics adapter that was found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gpu {
    pub vendor: GpuVendor,
    pub name: String,
    /// Dedicated memory in MiB when it could be read. Unknown is `None`, never zero.
    pub vram_mb: Option<u32>,
}

/// What was found on this computer. Every field is what was detected, never what was assumed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemProfile {
    pub os: Os,
    pub gpus: Vec<Gpu>,
    /// A ComfyUI install was found (only matters for the ComfyUI engine).
    pub comfyui_found: bool,
}

/// An engine an app could use for a tool, and whether this app can run it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineOffer {
    /// The adapter name on the connection: `comfyui`, `candle`, `burn`.
    pub provider: String,
    /// This app has an adapter that runs it.
    pub adapter_present: bool,
    /// The card makers this app's build of the engine can use, as the app states. `None` means the
    /// app said nothing: a built-in engine is then [`Pace::Unsure`].
    pub accelerators: Option<Vec<GpuVendor>>,
}

/// How fast an engine is expected to be here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pace {
    /// Expected to use the graphics card.
    Gpu,
    /// The card is one it can drive, but the app has not said its build does.
    Unsure,
    /// Runs on the computer's main chip, which is slow for large models.
    Cpu,
}

/// One engine, judged for this computer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineAdvice {
    pub provider: String,
    /// It can run now. False when this app cannot use it or ComfyUI is not installed.
    pub usable: bool,
    /// Expected pace when usable.
    pub pace: Option<Pace>,
    /// The suggestion. At most one engine is recommended, and never an unusable one.
    pub recommended: bool,
    /// One or two plain sentences, safe to show to anyone.
    pub reason: String,
}

/// Judges every offered engine for `profile`, best first. `min_vram_mb` is the recipe's figure for
/// graphics memory, where one is known.
pub fn advise(
    profile: &SystemProfile,
    offers: &[EngineOffer],
    min_vram_mb: Option<u32>,
) -> Vec<EngineAdvice> {
    let mut judged: Vec<(usize, EngineAdvice)> = offers
        .iter()
        .enumerate()
        .map(|(index, offer)| (index, judge(profile, offer, min_vram_mb)))
        .collect();
    judged.sort_by_key(|(index, a)| {
        (
            !a.usable,
            a.pace.unwrap_or(Pace::Cpu),
            a.provider == "comfyui",
            *index,
        )
    });
    let mut out: Vec<EngineAdvice> = judged.into_iter().map(|(_, a)| a).collect();
    if let Some(first) = out.first_mut().filter(|a| a.usable) {
        first.recommended = true;
        first.reason.push_str(if first.pace == Some(Pace::Cpu) {
            " Best choice here, but it will be slow."
        } else {
            " Suggested for your computer."
        });
    }
    out
}

fn judge(profile: &SystemProfile, offer: &EngineOffer, min_vram_mb: Option<u32>) -> EngineAdvice {
    let provider = offer.provider.clone();
    let blocked = |reason: &str| EngineAdvice {
        provider: provider.clone(),
        usable: false,
        pace: None,
        recommended: false,
        reason: reason.to_owned(),
    };
    if !offer.adapter_present {
        return blocked("This app can't use this option yet.");
    }
    if offer.provider == "comfyui" && !profile.comfyui_found {
        return blocked("ComfyUI isn't installed on this computer.");
    }
    // ComfyUI brings its own build, so the table is the whole answer. A built-in engine needs the
    // app to say what its build includes.
    let stated = offer.provider == "comfyui" || offer.accelerators.is_some();
    let gpu = profile.gpus.iter().find(|g| match &offer.accelerators {
        Some(vendors) => vendors.contains(&g.vendor),
        None => gpu_route(&offer.provider, profile.os, g.vendor),
    });
    let (pace, reason) = match gpu {
        Some(g) if too_small(g, min_vram_mb) => (
            Pace::Cpu,
            format!(
                "Your graphics card ({}) doesn't have enough memory for this, so it will be slow.",
                g.name
            ),
        ),
        Some(g) if stated => (
            Pace::Gpu,
            format!(
                "Should use your graphics card ({}).{}",
                g.name,
                card_note(offer, g)
            ),
        ),
        Some(g) => (
            Pace::Unsure,
            format!(
                "Might use your graphics card ({}), but this app hasn't confirmed it.",
                g.name
            ),
        ),
        None if profile.gpus.is_empty() => (
            Pace::Cpu,
            "No graphics card found, so this will be slow.".to_owned(),
        ),
        None => (
            Pace::Cpu,
            "Your graphics card can't speed this up, so it will be slow.".to_owned(),
        ),
    };
    EngineAdvice {
        provider,
        usable: true,
        pace: Some(pace),
        recommended: false,
        reason,
    }
}

/// A caution about the card itself, where the engine only supports recent ones.
fn card_note(offer: &EngineOffer, gpu: &Gpu) -> &'static str {
    if offer.provider != "comfyui" {
        return "";
    }
    match gpu.vendor {
        GpuVendor::Amd => " Needs a fairly recent AMD card.",
        GpuVendor::Intel => " Needs an Intel Arc card.",
        _ => "",
    }
}

fn too_small(gpu: &Gpu, min_vram_mb: Option<u32>) -> bool {
    matches!((gpu.vram_mb, min_vram_mb), (Some(have), Some(need)) if have < need)
}

/// Whether `provider` is expected to use a `vendor` card on `os`. An engine this table does not
/// know about is assumed to have no graphics route, so it is never suggested over one that has.
/// Read from each project's README on 13/10/2026, not measured on any computer here: review it
/// when an engine gains a backend. This is what the engine can do when built for it, not what a
/// given app's build does (see [`EngineOffer::accelerators`]).
pub fn gpu_route(provider: &str, os: Os, vendor: GpuVendor) -> bool {
    match provider {
        // Candle's README and crates: CUDA (Nvidia) and Metal (Apple) as build features, else CPU.
        "candle" => matches!(
            (os, vendor),
            (Os::Windows | Os::Linux, GpuVendor::Nvidia) | (Os::MacOs, GpuVendor::Apple)
        ),
        // Burn's README: CUDA, ROCm, Metal, Vulkan and WebGPU backends; Vulkan or WebGPU reach
        // Nvidia, AMD and Intel on every OS, Metal reaches Apple.
        "burn" => matches!(
            vendor,
            GpuVendor::Nvidia | GpuVendor::Amd | GpuVendor::Intel | GpuVendor::Apple
        ),
        // ComfyUI's README: Nvidia (CUDA), AMD (ROCm, Linux and Windows 11), Intel Arc (torch.xpu,
        // Windows and Linux), Apple silicon.
        "comfyui" => matches!(
            (os, vendor),
            (
                Os::Windows | Os::Linux,
                GpuVendor::Nvidia | GpuVendor::Amd | GpuVendor::Intel
            ) | (Os::MacOs, GpuVendor::Apple)
        ),
        _ => false,
    }
}

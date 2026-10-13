//! The "Set up for me" offer: everything one Install click would do, written for a person.
//!
//! Pure data and logic (it compiles for wasm). The caller describes the computer and what is
//! already installed; the offer lists the steps in plain language with their sizes, the licences
//! involved, the terms the click agrees to, and anything that stops it. Nothing here downloads or
//! installs: `craft-ai-comfy-env` carries the offer out, and only after the person has clicked.
//!
//! ComfyUI comes as the official Windows portable build, kept in the app's own folder: no
//! installer window, no admin rights, nothing outside that folder changes. The release is pinned
//! below (name, size and SHA-256 from the release page) so a download is checked before it is
//! unpacked. On other systems there is no portable build, so the offer says so and gives plain
//! steps instead of pretending.
//!
//! Every sentence here is read by people: no engine names beyond "ComfyUI" (which they must
//! install), no hardware jargon. `tests/setup.rs` scans for it.

use serde::{Deserialize, Serialize};

use crate::engine::{GpuVendor, Os, SystemProfile, gpu_route};
use crate::feature::Feature;
use crate::plan::{Inventory, plan};
use crate::recipe::Licence;

/// The ComfyUI release the portable builds are pinned to.
pub const COMFYUI_TAG: &str = "v0.39.0";
const COMFYUI_BASE: &str = "https://github.com/comfyanonymous/ComfyUI/releases/download";

/// Which way the tool will run, which decides whether ComfyUI is needed at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupRoute {
    /// Through a ComfyUI that runs in the background.
    ComfyUi,
    /// Inside the app itself: only the model is needed.
    BuiltIn,
}

/// One official portable ComfyUI download.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortableBuild {
    pub asset: String,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: String,
}

/// What a step is, so a screen can draw an icon and a program can carry it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    ComfyUi,
    Tools,
    Model,
    Finish,
}

/// One thing the Install click will do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupStep {
    pub kind: StepKind,
    pub title: String,
    /// One or two plain sentences.
    pub detail: String,
    pub download_bytes: u64,
}

/// What clicking Install means. `version` goes up whenever the wording changes, so an agreement
/// recorded earlier is never taken for agreement to new words.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agreement {
    pub version: u32,
    pub title: String,
    /// The line shown beside the button.
    pub summary: String,
    pub full_text: String,
}

/// The terms the Install click agrees to.
///
/// DRAFT WORDING. It was written to be short and plain and has not been checked by a lawyer.
/// Replace `full_text` with approved wording and raise `version` before this ships.
pub fn agreement() -> Agreement {
    Agreement {
        version: 1,
        title: "Terms for setting up".to_owned(),
        summary: "By clicking Install you agree to these terms.".to_owned(),
        full_text: "This will download and install software and models made by other people, \
            including ComfyUI and the tools and model needed for this feature. We did not make \
            them. They come with their own licences, which you also accept by clicking Install.\n\n\
            Everything is kept in this app's own folder, and you can remove it later. We check \
            that each download is exactly what its maker published, but we cannot promise that third-party \
            software is free of faults or safe for every computer.\n\n\
            You use it at your own risk. To the fullest extent the law allows, we are not \
            responsible for loss, damage or problems caused by software or models we did not \
            make, or by installing them. Nothing here limits rights you have by law."
            .to_owned(),
    }
}

/// Everything one Install click would do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupOffer {
    pub feature: String,
    pub route: SetupRoute,
    pub steps: Vec<SetupStep>,
    pub download_bytes: u64,
    /// Licences the click also accepts, once each.
    pub licences: Vec<Licence>,
    pub agreement: Agreement,
    /// Why Install cannot go ahead, in plain sentences. Empty when it can.
    pub blockers: Vec<String>,
    /// For a system with no portable ComfyUI: the plain steps to do by hand. Empty otherwise.
    pub manual_steps: Vec<String>,
    /// The download behind the ComfyUI step, for the program that carries it out.
    pub portable: Option<PortableBuild>,
}

impl SetupOffer {
    /// Install can be offered as a button.
    pub fn can_install(&self) -> bool {
        self.blockers.is_empty() && self.manual_steps.is_empty()
    }
}

/// The official portable build that suits `profile`, or `None` where there is none (anything but
/// Windows). Nvidia first, then AMD, then Intel; a computer with none of them gets the Nvidia
/// build, which the ComfyUI README says also runs on the main chip alone. Older Nvidia cards
/// (GTX 10 series and earlier) need the older build.
pub fn portable_for(profile: &SystemProfile) -> Option<PortableBuild> {
    if profile.os != Os::Windows {
        return None;
    }
    let usable = |vendor| {
        profile
            .gpus
            .iter()
            .find(|g| g.vendor == vendor && gpu_route("comfyui", profile.os, vendor))
    };
    let asset = if let Some(card) = usable(GpuVendor::Nvidia) {
        if is_old_nvidia(&card.name) {
            "ComfyUI_windows_portable_nvidia_cu126.7z"
        } else {
            "ComfyUI_windows_portable_nvidia.7z"
        }
    } else if usable(GpuVendor::Amd).is_some() {
        "ComfyUI_windows_portable_amd.7z"
    } else if usable(GpuVendor::Intel).is_some() {
        "ComfyUI_windows_portable_intel.7z"
    } else {
        "ComfyUI_windows_portable_nvidia.7z"
    };
    PINNED.iter().find(|p| p.0 == asset).map(|p| PortableBuild {
        asset: p.0.to_owned(),
        url: format!("{COMFYUI_BASE}/{COMFYUI_TAG}/{}", p.0),
        size_bytes: p.1,
        sha256: p.2.to_owned(),
    })
}

/// Name, size and SHA-256 of each asset of the pinned release, from its release page.
const PINNED: &[(&str, u64, &str)] = &[
    (
        "ComfyUI_windows_portable_nvidia.7z",
        2_003_629_308,
        "3dca347842ed1c6c833105fafb1669284685a3e54df41578d5f94bcc898ae039",
    ),
    (
        "ComfyUI_windows_portable_nvidia_cu126.7z",
        1_895_335_518,
        "c42e7d5f6a5e931f6551dcf102d739a55c5f4a4df921f748e1de9de9f648d6a2",
    ),
    (
        "ComfyUI_windows_portable_amd.7z",
        1_621_312_076,
        "19be1e3c8690c4f9eeefb0f2ee8f835a98e2fbf33cd9b80d0ca309f4c86d37c1",
    ),
    (
        "ComfyUI_windows_portable_intel.7z",
        1_436_509_991,
        "50f629c3743b1fb64509d8e582ae30b06105d9614587a514e79ac9f3c3f62939",
    ),
];

/// Only exact files from our checked release are accepted by the one-click installer.
pub fn is_official_build(build: &PortableBuild) -> bool {
    PINNED.iter().any(|(asset, size, digest)| {
        build.asset == *asset
            && build.url == format!("{COMFYUI_BASE}/{COMFYUI_TAG}/{asset}")
            && build.size_bytes == *size
            && build.sha256 == *digest
    })
}

fn is_old_nvidia(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "gtx 7", "gtx 9", "gtx 10", "titan x", "quadro p", "quadro m", "tesla p", "tesla k",
    ]
    .iter()
    .any(|old| lower.contains(old))
}

/// What one Install click would do for `feature` on this computer, or `None` when the feature has
/// no local recipe or everything it needs is already here.
pub fn setup_offer(
    feature: &Feature,
    profile: &SystemProfile,
    inventory: &Inventory,
    route: SetupRoute,
) -> Option<SetupOffer> {
    let recipe = feature.local.as_ref().filter(|r| r.usable)?;
    let mut steps = Vec::new();
    let mut portable = None;
    let mut manual_steps = Vec::new();
    let needs_comfyui = route == SetupRoute::ComfyUi && !inventory.comfyui_found;
    if needs_comfyui {
        match portable_for(profile) {
            Some(build) => {
                steps.push(SetupStep {
                    kind: StepKind::ComfyUi,
                    title: "Install ComfyUI".to_owned(),
                    detail: "A private copy kept in this app's folder. Nothing else on your \
                        computer is changed."
                        .to_owned(),
                    download_bytes: build.size_bytes,
                });
                portable = Some(build);
            }
            None => manual_steps = manual_install_steps(),
        }
    }
    // Compare against a computer that has ComfyUI, so the rest of the list is the same either way.
    let comfy_view = Inventory {
        comfyui_found: true,
        ..inventory.clone()
    };
    let compared = plan(feature, &comfy_view);
    if route == SetupRoute::ComfyUi && !compared.missing_packs.is_empty() {
        steps.push(SetupStep {
            kind: StepKind::Tools,
            title: format!("Add the {} tools", recipe.label),
            detail: "Small add-ons from their authors that let ComfyUI do this job.".to_owned(),
            download_bytes: 0,
        });
    }
    for file in &compared.missing_files {
        steps.push(SetupStep {
            kind: StepKind::Model,
            title: format!("Download the {} model", recipe.label),
            detail: format!(
                "The model that does the work ({}).",
                gigabytes(file.size_bytes)
            ),
            download_bytes: file.size_bytes,
        });
    }
    if route == SetupRoute::ComfyUi && !compared.missing_packs.is_empty() {
        steps.push(SetupStep {
            kind: StepKind::Finish,
            title: "Finish setting up".to_owned(),
            detail: "Prepares the add-ons so they are ready to use.".to_owned(),
            download_bytes: 0,
        });
    }
    if steps.is_empty() && manual_steps.is_empty() {
        return None;
    }
    let download_bytes = steps
        .iter()
        .fold(0u64, |sum, s| sum.saturating_add(s.download_bytes));
    let mut blockers = Vec::new();
    // A ComfyUI archive is kept while it is unpacked, so it needs room twice over.
    let needed = download_bytes.saturating_add(portable.as_ref().map_or(0, |p| p.size_bytes));
    if let Some(free) = inventory.free_disk_bytes
        && needed > free
    {
        blockers.push(format!(
            "There isn't enough free space on this computer. About {} is needed.",
            gigabytes(needed)
        ));
    }
    if !compared.blockers.is_empty() && blockers.is_empty() {
        for b in &compared.blockers {
            if let crate::plan::Blocker::NotEnoughVram { .. } = b {
                blockers.push(
                    "Your graphics card doesn't have enough memory for this model.".to_owned(),
                );
            }
        }
    }
    Some(SetupOffer {
        feature: feature.id.clone(),
        route,
        steps,
        download_bytes,
        licences: compared.licences,
        agreement: agreement(),
        blockers,
        manual_steps,
        portable,
    })
}

fn manual_install_steps() -> Vec<String> {
    vec![
        "ComfyUI has no ready-made copy for this kind of computer, so it can't be installed with \
         one click."
            .to_owned(),
        "Install ComfyUI from comfy.org, open it once, then come back here.".to_owned(),
    ]
}

/// "3.4 GB" or "820 MB", for sentences a person reads.
pub fn gigabytes(bytes: u64) -> String {
    const GB: u64 = 1_000_000_000;
    const MB: u64 = 1_000_000;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else {
        format!("{} MB", bytes.div_ceil(MB).max(1))
    }
}

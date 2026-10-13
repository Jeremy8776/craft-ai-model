//! The Install offer: what one click would do for each kind of computer, and that every sentence
//! in it is plain language.
use craft_ai_features::{
    Feature, Gpu, GpuVendor, InstalledFile, Inventory, Os, SetupOffer, SetupRoute, StepKind,
    SystemProfile, agreement, gigabytes, portable_for, setup_offer,
};

fn select_subject() -> Feature {
    craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == "select-subject")
        .unwrap()
}

fn card(vendor: GpuVendor, name: &str) -> Gpu {
    Gpu {
        vendor,
        name: name.to_owned(),
        vram_mb: Some(24_576),
    }
}

fn pc(os: Os, gpus: Vec<Gpu>) -> SystemProfile {
    SystemProfile {
        os,
        gpus,
        comfyui_found: false,
    }
}

fn empty() -> Inventory {
    Inventory {
        free_disk_bytes: Some(500_000_000_000),
        ..Inventory::default()
    }
}

fn offer(profile: &SystemProfile, inventory: &Inventory, route: SetupRoute) -> Option<SetupOffer> {
    setup_offer(&select_subject(), profile, inventory, route)
}

fn kinds(o: &SetupOffer) -> Vec<StepKind> {
    o.steps.iter().map(|s| s.kind).collect()
}

#[test]
fn a_windows_pc_with_nothing_gets_the_whole_list_in_order() {
    let p = pc(Os::Windows, vec![card(GpuVendor::Nvidia, "RTX 3090")]);
    let o = offer(&p, &empty(), SetupRoute::ComfyUi).unwrap();
    assert_eq!(
        kinds(&o),
        [
            StepKind::ComfyUi,
            StepKind::Tools,
            StepKind::Model,
            StepKind::Finish
        ]
    );
    assert!(o.can_install(), "{:?}", o.blockers);
    assert_eq!(
        o.portable.as_ref().unwrap().asset,
        "ComfyUI_windows_portable_nvidia.7z"
    );
    let sum: u64 = o.steps.iter().map(|s| s.download_bytes).sum();
    assert_eq!(o.download_bytes, sum);
    assert!(o.download_bytes > 5_000_000_000, "{}", o.download_bytes);
    assert_eq!(o.agreement, agreement());
    assert!(
        !o.licences.is_empty(),
        "the model's licence is part of the click"
    );
}

#[test]
fn the_build_follows_the_card_nvidia_first_and_older_cards_get_the_older_build() {
    let asset = |gpus: Vec<Gpu>| portable_for(&pc(Os::Windows, gpus)).unwrap().asset;
    assert_eq!(
        asset(vec![card(GpuVendor::Amd, "RX 7800")]),
        "ComfyUI_windows_portable_amd.7z"
    );
    assert_eq!(
        asset(vec![card(GpuVendor::Intel, "Arc A770")]),
        "ComfyUI_windows_portable_intel.7z"
    );
    assert_eq!(
        asset(vec![]),
        "ComfyUI_windows_portable_nvidia.7z",
        "runs on the main chip too"
    );
    assert_eq!(
        asset(vec![card(GpuVendor::Other, "Mystery")]),
        "ComfyUI_windows_portable_nvidia.7z"
    );
    assert_eq!(
        asset(vec![
            card(GpuVendor::Amd, "AMD Radeon(TM) Graphics"),
            card(GpuVendor::Nvidia, "RTX 3090")
        ]),
        "ComfyUI_windows_portable_nvidia.7z",
        "a discrete Nvidia card beats an AMD integrated one, whatever the order"
    );
    assert_eq!(
        asset(vec![card(GpuVendor::Nvidia, "NVIDIA GeForce GTX 1080")]),
        "ComfyUI_windows_portable_nvidia_cu126.7z"
    );
    assert_eq!(
        asset(vec![card(GpuVendor::Nvidia, "NVIDIA GeForce RTX 2060")]),
        "ComfyUI_windows_portable_nvidia.7z"
    );
}

#[test]
fn the_download_is_pinned_and_checkable() {
    for gpus in [
        vec![card(GpuVendor::Nvidia, "RTX")],
        vec![card(GpuVendor::Nvidia, "GTX 970")],
        vec![card(GpuVendor::Amd, "RX")],
        vec![card(GpuVendor::Intel, "Arc")],
    ] {
        let b = portable_for(&pc(Os::Windows, gpus)).unwrap();
        assert_eq!(
            b.url,
            format!(
                "https://github.com/comfyanonymous/ComfyUI/releases/download/{}/{}",
                craft_ai_features::COMFYUI_TAG,
                b.asset
            )
        );
        assert_eq!(b.sha256.len(), 64);
        assert!(
            b.sha256
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert!(b.size_bytes > 1_000_000_000);
    }
}

#[test]
fn other_systems_get_plain_steps_and_no_install_button() {
    for os in [Os::Linux, Os::MacOs, Os::Other] {
        let o = offer(&pc(os, vec![]), &empty(), SetupRoute::ComfyUi).unwrap();
        assert!(o.portable.is_none(), "{os:?}");
        assert!(!o.can_install(), "{os:?}");
        assert!(o.manual_steps.iter().any(|s| s.contains("comfy.org")));
        assert!(!kinds(&o).contains(&StepKind::ComfyUi), "{os:?}");
        assert!(portable_for(&pc(os, vec![])).is_none());
    }
}

#[test]
fn the_built_in_route_needs_only_the_model() {
    let p = pc(Os::Windows, vec![card(GpuVendor::Nvidia, "RTX 3090")]);
    let o = offer(&p, &empty(), SetupRoute::BuiltIn).unwrap();
    assert_eq!(kinds(&o), [StepKind::Model]);
    assert!(o.portable.is_none() && o.manual_steps.is_empty());
    assert!(o.can_install());
    // Even on a system with no ComfyUI build at all.
    let o = offer(&pc(Os::Linux, vec![]), &empty(), SetupRoute::BuiltIn).unwrap();
    assert_eq!(kinds(&o), [StepKind::Model]);
    assert!(o.can_install());
}

#[test]
fn what_is_already_there_is_not_offered_again() {
    let f = select_subject();
    let recipe = f.local.as_ref().unwrap();
    let have_model = Inventory {
        comfyui_found: true,
        files: recipe
            .models
            .iter()
            .map(|m| InstalledFile {
                folder: m.folder.clone(),
                filename: m.filename.clone(),
                size_bytes: m.size_bytes,
            })
            .collect(),
        node_folders: recipe
            .packs
            .iter()
            .flat_map(|p| p.folder_names.clone())
            .collect(),
        free_disk_bytes: Some(500_000_000_000),
        ..Inventory::default()
    };
    let p = pc(Os::Windows, vec![card(GpuVendor::Nvidia, "RTX 3090")]);
    assert!(setup_offer(&f, &p, &have_model, SetupRoute::ComfyUi).is_none());
    let no_tools = Inventory {
        node_folders: vec![],
        ..have_model.clone()
    };
    let o = setup_offer(&f, &p, &no_tools, SetupRoute::ComfyUi).unwrap();
    assert_eq!(kinds(&o), [StepKind::Tools, StepKind::Finish]);
    assert_eq!(o.download_bytes, 0);
    let no_model = Inventory {
        files: vec![],
        ..have_model
    };
    let o = setup_offer(&f, &p, &no_model, SetupRoute::ComfyUi).unwrap();
    assert_eq!(kinds(&o), [StepKind::Model]);
}

#[test]
fn a_tool_with_no_local_model_has_no_offer() {
    let p = pc(Os::Windows, vec![]);
    let hosted_only = craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.local.is_none())
        .unwrap();
    assert!(setup_offer(&hosted_only, &p, &empty(), SetupRoute::ComfyUi).is_none());
}

#[test]
fn too_little_free_space_blocks_the_button_and_says_how_much_is_needed() {
    let p = pc(Os::Windows, vec![card(GpuVendor::Nvidia, "RTX 3090")]);
    let tight = Inventory {
        free_disk_bytes: Some(3_000_000_000),
        ..empty()
    };
    let o = offer(&p, &tight, SetupRoute::ComfyUi).unwrap();
    assert!(!o.can_install());
    assert_eq!(o.blockers.len(), 1, "{:?}", o.blockers);
    assert!(o.blockers[0].contains("free space"), "{}", o.blockers[0]);
    assert!(o.blockers[0].contains("GB"), "{}", o.blockers[0]);
    let unknown = Inventory {
        free_disk_bytes: None,
        ..empty()
    };
    assert!(
        offer(&p, &unknown, SetupRoute::ComfyUi)
            .unwrap()
            .can_install()
    );
}

#[test]
fn sizes_read_as_a_person_would() {
    assert_eq!(gigabytes(3_449_860_632), "3.4 GB");
    assert_eq!(gigabytes(820_000_000), "820 MB");
    assert_eq!(gigabytes(1), "1 MB");
}

/// Words a person should not have to read in the offer. ComfyUI is named on purpose.
const JARGON: &[&str] = &[
    "candle",
    "burn",
    "cuda",
    "metal",
    "wgpu",
    "vulkan",
    "vram",
    "mib",
    "adapter",
    "provider",
    "backend",
    "gpu",
    "cpu",
    "onnx",
    "wasm",
    "safetensors",
    "inference",
    "runtime",
    "tensor",
    "checkpoint",
    "weights",
    "node",
    "nodes",
    "pack",
    "pip",
    "python",
    "archive",
    "sha256",
    "checksum",
    "7z",
    "extract",
    "unpack",
    "portable",
    "headless",
    "api",
    "token",
];

fn jargon_in(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .filter(|w| JARGON.contains(&w.as_str()))
        .collect()
}

#[test]
fn every_sentence_in_the_offer_is_plain_language() {
    let systems = [
        pc(Os::Windows, vec![card(GpuVendor::Nvidia, "Test Card")]),
        pc(Os::Windows, vec![]),
        pc(Os::Linux, vec![card(GpuVendor::Amd, "Test Card")]),
        pc(Os::MacOs, vec![card(GpuVendor::Apple, "Test Chip")]),
    ];
    let mut checked = 0;
    for p in &systems {
        for route in [SetupRoute::ComfyUi, SetupRoute::BuiltIn] {
            for free in [None, Some(1_000_000_000), Some(500_000_000_000)] {
                let inv = Inventory {
                    free_disk_bytes: free,
                    ..Inventory::default()
                };
                let Some(o) = offer(p, &inv, route) else {
                    continue;
                };
                let mut texts: Vec<String> = o.blockers.clone();
                texts.extend(o.manual_steps.clone());
                for s in &o.steps {
                    texts.push(s.title.clone());
                    texts.push(s.detail.clone());
                }
                texts.push(o.agreement.title.clone());
                texts.push(o.agreement.summary.clone());
                texts.push(o.agreement.full_text.clone());
                for t in texts {
                    let found = jargon_in(&t);
                    assert!(found.is_empty(), "{found:?} in \"{t}\"");
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 60, "the matrix must really run: {checked}");
}

#[test]
fn the_jargon_check_itself_catches_jargon() {
    assert_eq!(
        jargon_in("Unpack the 7z archive with pip."),
        ["unpack", "7z", "archive", "pip"]
    );
    assert!(jargon_in("Install ComfyUI.").is_empty());
}

#[test]
fn the_agreement_is_versioned_and_says_what_a_person_needs_to_know() {
    let a = agreement();
    assert!(a.version >= 1);
    for must in [
        "other people",
        "own licences",
        "your own risk",
        "responsible",
    ] {
        assert!(a.full_text.contains(must), "missing: {must}");
    }
    assert!(a.summary.contains("Install"), "{}", a.summary);
}

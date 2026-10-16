use craft_ai_core::PRESETS;
use craft_ai_features::*;
use std::collections::HashSet;

fn feature(id: &str) -> Feature {
    builtin().into_iter().find(|f| f.id == id).unwrap()
}

fn have_sam3() -> Inventory {
    Inventory {
        comfyui_found: true,
        files: vec![InstalledFile {
            folder: "sam3".into(),
            filename: "sam3.safetensors".into(),
            size_bytes: 3_449_860_632,
        }],
        node_folders: vec!["comfyui-sam3".into()],
        vram_mb: Some(24_576),
        free_disk_bytes: Some(500_000_000_000),
    }
}

fn bare() -> Inventory {
    Inventory {
        comfyui_found: true,
        ..Inventory::default()
    }
}

#[test]
fn the_catalogue_is_complete_and_consistent() {
    let all = builtin();
    assert_eq!(all.len(), 7, "a bad capability id empties the list");
    let ids: HashSet<_> = all.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids.len(), all.len());
    let presets: HashSet<_> = PRESETS.iter().map(|p| p.key).collect();
    for f in &all {
        assert!(
            !f.title.is_empty() && !f.summary.is_empty() && !f.uses.is_empty(),
            "{}",
            f.id
        );
        for c in &f.cloud {
            assert!(
                presets.contains(c.preset.as_str()),
                "{} -> {}",
                f.id,
                c.preset
            );
        }
        // Nothing may claim an app ships a tool before an app calls the shared layer.
        assert_eq!(f.shipped_in().count(), 0, "{}", f.id);
    }
}

#[test]
fn recipes_carry_checkable_files() {
    for f in builtin() {
        let Some(local) = f.local else { continue };
        for m in local.models {
            assert_eq!(m.sha256.len(), 64, "{}", m.filename);
            assert!(
                m.sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            );
            assert!(m.size_bytes > 0);
            assert!(m.source.url().starts_with("https://"));
            assert!(!m.filename.contains(['/', '\\']) && !m.folder.contains(['/', '\\', '.']));
        }
    }
}

#[test]
fn the_sam3_recipe_matches_the_real_file() {
    // Size and checksum of the file the Hugging Face copy serves and of the copy checked on the maintainer's machine.
    let m = &feature("select-subject").local.unwrap().models[0];
    assert_eq!(m.size_bytes, 3_449_860_632);
    assert_eq!(
        m.sha256,
        "127037a7a11169c63a210b8e3e9caad24a66abdd65d3a78bbc3a7d8577d57026"
    );
    assert_eq!(
        m.source.url(),
        "https://huggingface.co/apozz/sam3-safetensors/resolve/e88da2ed02aded4006f4711ee6a9c75d5aa38d0c/sam3.safetensors"
    );
    assert!(m.licence.accept_required);
}

#[test]
fn no_user_text_uses_an_em_dash() {
    let json = serde_json::to_string(&builtin()).unwrap();
    assert!(!json.contains('\u{2014}') && !json.contains('\u{2013}'));
}

#[test]
fn everything_present_means_ready() {
    let p = plan(&feature("select-subject"), &have_sam3());
    assert_eq!(p.state, State::Ready);
    assert!(p.missing_files.is_empty() && p.missing_packs.is_empty());
    assert_eq!(p.download_bytes, 0);
    assert!(p.licences.is_empty());
}

#[test]
fn a_bare_install_needs_the_model_and_the_pack() {
    let p = plan(&feature("select-subject"), &bare());
    assert_eq!(p.state, State::NeedsInstall);
    assert_eq!(p.download_bytes, 3_449_860_632);
    assert_eq!(p.missing_files.len(), 1);
    assert_eq!(p.missing_packs.len(), 1);
    assert_eq!(p.licences.len(), 1);
}

#[test]
fn a_file_of_the_wrong_size_counts_as_missing() {
    let mut inv = have_sam3();
    inv.files[0].size_bytes -= 1;
    let p = plan(&feature("select-subject"), &inv);
    assert_eq!(p.state, State::NeedsInstall);
    assert_eq!(p.missing_files.len(), 1);
    // Same size but the wrong folder is also missing.
    let mut inv = have_sam3();
    inv.files[0].folder = "sams".into();
    assert_eq!(
        plan(&feature("select-subject"), &inv).missing_files.len(),
        1
    );
}

#[test]
fn names_match_without_regard_to_case() {
    let mut inv = have_sam3();
    inv.files[0].filename = "SAM3.safetensors".into();
    inv.node_folders = vec!["ComfyUI-SAM3".into()];
    assert_eq!(plan(&feature("select-subject"), &inv).state, State::Ready);
}

#[test]
fn only_the_pack_missing_still_needs_install_but_downloads_nothing() {
    let mut inv = have_sam3();
    inv.node_folders.clear();
    let p = plan(&feature("select-subject"), &inv);
    assert_eq!(p.state, State::NeedsInstall);
    assert_eq!(p.download_bytes, 0);
    assert_eq!(p.missing_packs.len(), 1);
}

#[test]
fn a_licence_shared_by_two_files_is_asked_once() {
    let mut f = feature("select-subject");
    let local = f.local.as_mut().unwrap();
    let mut second = local.models[0].clone();
    second.filename = "other.safetensors".into();
    local.models.push(second);
    let p = plan(&f, &bare());
    assert_eq!(p.missing_files.len(), 2);
    assert_eq!(p.licences.len(), 1);
}

#[test]
fn not_enough_graphics_memory_or_disk_blocks() {
    let mut f = feature("select-subject");
    f.local.as_mut().unwrap().min_vram_mb = Some(12_000);
    let mut inv = have_sam3();
    inv.files.clear();
    inv.vram_mb = Some(8_000);
    inv.free_disk_bytes = Some(1_000);
    let p = plan(&f, &inv);
    assert_eq!(p.state, State::Blocked);
    assert!(p.blockers.contains(&Blocker::NotEnoughVram {
        needed_mb: 12_000,
        have_mb: 8_000
    }));
    assert!(p.blockers.contains(&Blocker::NotEnoughDisk {
        needed_bytes: 3_449_860_632,
        free_bytes: 1_000
    }));
}

#[test]
fn unknown_hardware_does_not_block() {
    let mut f = feature("select-subject");
    f.local.as_mut().unwrap().min_vram_mb = Some(12_000);
    let mut inv = bare();
    inv.vram_mb = None;
    inv.free_disk_bytes = None;
    assert_eq!(plan(&f, &inv).state, State::NeedsInstall);
}

#[test]
fn no_comfyui_and_no_recipe_are_reported_as_such() {
    assert_eq!(
        plan(&feature("select-subject"), &Inventory::default()).state,
        State::NoRuntime
    );
    // Even with ComfyUI present, a feature nobody has worked out locally stays that way.
    assert_eq!(
        plan(&feature("remove-object"), &have_sam3()).state,
        State::NoLocalRoute
    );
}

#[test]
fn plans_and_inventories_survive_a_json_round_trip() {
    let p = plan(&feature("select-subject"), &bare());
    let back: Plan = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(back, p);
    let inv = have_sam3();
    let back: Inventory = serde_json::from_str(&serde_json::to_string(&inv).unwrap()).unwrap();
    assert_eq!(back, inv);
}

#[test]
fn every_tool_has_a_role_that_names_its_inputs() {
    for f in builtin() {
        let role = f.role().unwrap_or_else(|| panic!("{} has no role", f.id));
        assert_eq!(role.id.as_str(), f.id);
        assert_eq!(role.capability, f.capability);
        assert!(!role.inputs.is_empty(), "{}", f.id);
    }
}

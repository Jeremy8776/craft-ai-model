#![allow(dead_code)]
//! Which of a recipe's node packs are in the person's ComfyUI: installed by hand, installed by this
//! app (with the commit it recorded), or missing. The report only reads folder names and the
//! registry, so these tests also pin that nothing is run, written or followed. Scripted localhost
//! servers and temporary folders only: no real repository is read and no real ComfyUI is touched.
mod nodepack_rig;

use std::path::Path;

use craft_ai_comfy_env::ComfyInstall;
use craft_ai_comfy_env::source::{PackReport, PackStatus, pack_status};
use craft_ai_features::NodePack;
use nodepack_rig::*;

fn wanted(label: &str, folders: &[&str]) -> NodePack {
    NodePack {
        label: label.into(),
        repo_url: "https://github.com/owner/Pack".into(),
        folder_names: folders.iter().map(|f| (*f).to_owned()).collect(),
    }
}

fn comfy(rig: &Rig) -> ComfyInstall {
    ComfyInstall::at(&rig.dir.path().join("Comfy")).unwrap()
}

fn report(rig: &Rig, packs: &[NodePack]) -> Vec<PackReport> {
    pack_status(&comfy(rig), &rig.registry(), packs).unwrap()
}

fn one(rig: &Rig, pack: &NodePack) -> PackReport {
    report(rig, std::slice::from_ref(pack)).remove(0)
}

/// Every path under `dir` with its bytes, so a before and after can be compared.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            let name = p
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if e.file_type().unwrap().is_dir() {
                out.push((name, Vec::new()));
                stack.push(p);
            } else {
                out.push((name, std::fs::read(&p).unwrap_or_default()));
            }
        }
    }
    out.sort();
    out
}

fn installed_by_us() -> (Rig, String) {
    let server = host(good_archive());
    let rig = Rig::new(&origin(&server));
    let candidate = rig.candidate(&origin(&server));
    go(&rig, &candidate).unwrap();
    (rig, candidate.commit)
}

#[test]
fn a_pack_that_is_not_there_is_missing() {
    let rig = Rig::new("http://127.0.0.1:9");
    let r = one(&rig, &wanted("Pack", &["pack"]));
    assert_eq!(r.status, PackStatus::Missing);
    assert_eq!(r.folder, None);
    assert_eq!(r.label, "Pack");
}

#[test]
fn a_folder_the_person_put_there_is_installed_but_not_ours() {
    let rig = Rig::new("http://127.0.0.1:9");
    std::fs::create_dir_all(rig.nodes().join("ComfyUI-Pack")).unwrap();
    let r = one(&rig, &wanted("Pack", &["comfyui-pack"]));
    assert_eq!(r.status, PackStatus::Installed);
    assert_eq!(
        r.folder.as_deref(),
        Some("ComfyUI-Pack"),
        "the real spelling is reported"
    );
}

#[test]
fn a_pack_this_app_installed_reports_the_commit_it_recorded() {
    let (rig, commit) = installed_by_us();
    let r = one(&rig, &wanted("Pack", &["pack"]));
    assert_eq!(r.status, PackStatus::InstalledHere { commit });
    assert_eq!(r.folder.as_deref(), Some("Pack"));
}

#[test]
fn any_of_the_known_folder_names_will_do() {
    let rig = Rig::new("http://127.0.0.1:9");
    std::fs::create_dir_all(rig.nodes().join("alias")).unwrap();
    let r = one(&rig, &wanted("Pack", &["pack", "alias"]));
    assert_eq!(r.status, PackStatus::Installed);
}

#[test]
fn a_record_whose_folder_has_gone_is_missing() {
    let (rig, _) = installed_by_us();
    std::fs::remove_dir_all(rig.nodes().join("Pack")).unwrap();
    assert_eq!(
        rig.registry().packs().unwrap().len(),
        1,
        "the record is still there"
    );
    let r = one(&rig, &wanted("Pack", &["pack"]));
    assert_eq!(r.status, PackStatus::Missing);
    assert_eq!(r.folder, None);
}

#[test]
fn a_file_with_the_packs_name_is_not_the_pack() {
    let rig = Rig::new("http://127.0.0.1:9");
    std::fs::write(rig.nodes().join("pack"), b"not a folder").unwrap();
    assert_eq!(
        one(&rig, &wanted("Pack", &["pack"])).status,
        PackStatus::Missing
    );
}

#[test]
fn hidden_and_cache_folders_are_not_packs() {
    let rig = Rig::new("http://127.0.0.1:9");
    for name in [".git", "__pycache__"] {
        std::fs::create_dir_all(rig.nodes().join(name)).unwrap();
        assert_eq!(
            one(&rig, &wanted("x", &[name])).status,
            PackStatus::Missing,
            "{name}"
        );
    }
}

#[cfg(any(unix, windows))]
fn link(from: &Path, to: &Path) -> bool {
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(from, to);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(from, to);
    if made.is_err() {
        // Creating a link needs a privilege this machine did not grant. Say so, do not pretend.
        eprintln!("SKIPPED: could not create a link here, so link handling is untested");
    }
    made.is_ok()
}

#[test]
fn a_linked_folder_counts_as_there_but_is_never_read_through() {
    let rig = Rig::new("http://127.0.0.1:9");
    let elsewhere = rig.dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    if !link(&elsewhere, &rig.nodes().join("Pack")) {
        return;
    }
    let r = one(&rig, &wanted("Pack", &["pack"]));
    assert_eq!(r.status, PackStatus::Installed);
}

#[test]
fn a_link_to_nowhere_is_not_a_pack() {
    let rig = Rig::new("http://127.0.0.1:9");
    let nowhere = rig.dir.path().join("does-not-exist");
    if !link(&nowhere, &rig.nodes().join("Pack")) {
        return;
    }
    assert_eq!(
        one(&rig, &wanted("Pack", &["pack"])).status,
        PackStatus::Missing
    );
}

#[test]
fn a_recorded_folder_swapped_for_a_link_is_no_longer_ours() {
    let (rig, _) = installed_by_us();
    let elsewhere = rig.dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::remove_dir_all(rig.nodes().join("Pack")).unwrap();
    if !link(&elsewhere, &rig.nodes().join("Pack")) {
        return;
    }
    let r = one(&rig, &wanted("Pack", &["pack"]));
    assert_eq!(
        r.status,
        PackStatus::Installed,
        "the record must not vouch for someone else's folder"
    );
}

#[test]
fn a_known_folder_name_cannot_point_outside_custom_nodes() {
    let rig = Rig::new("http://127.0.0.1:9");
    std::fs::create_dir_all(rig.dir.path().join("Comfy/outside")).unwrap();
    for name in ["../outside", "..", ".", "a/b", "..\\outside"] {
        assert_eq!(
            one(&rig, &wanted("x", &[name])).status,
            PackStatus::Missing,
            "{name}"
        );
    }
}

#[test]
fn a_damaged_record_is_an_error_not_an_empty_list() {
    let (rig, _) = installed_by_us();
    std::fs::write(rig.dir.path().join("model-sources.json"), b"{ not json").unwrap();
    assert!(pack_status(&comfy(&rig), &rig.registry(), &[wanted("Pack", &["pack"])]).is_err());
}

#[test]
fn asking_changes_nothing_on_disk() {
    let (rig, _) = installed_by_us();
    std::fs::write(rig.nodes().join("Pack/extra.py"), b"print(1)").unwrap();
    std::fs::create_dir_all(rig.nodes().join("ByHand")).unwrap();
    let before = snapshot(rig.dir.path());
    let packs = [
        wanted("A", &["pack"]),
        wanted("B", &["byhand"]),
        wanted("C", &["absent"]),
    ];
    let r = report(&rig, &packs);
    assert_eq!(
        r.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
        ["A", "B", "C"],
        "one report per wanted pack, in order"
    );
    assert!(matches!(r[0].status, PackStatus::InstalledHere { .. }));
    assert_eq!(
        r[1].status,
        PackStatus::Installed,
        "a folder with no record is not ours"
    );
    assert_eq!(r[2].status, PackStatus::Missing);
    assert_eq!(snapshot(rig.dir.path()), before);
}

#[test]
fn no_custom_nodes_folder_at_all_means_everything_is_missing() {
    let rig = Rig::new("http://127.0.0.1:9");
    let install = comfy(&rig);
    std::fs::remove_dir_all(rig.nodes()).unwrap();
    let r = pack_status(&install, &rig.registry(), &[wanted("Pack", &["pack"])]).unwrap();
    assert_eq!(r[0].status, PackStatus::Missing);
}

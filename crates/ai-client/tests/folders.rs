//! The model folder list through the shared store, as the launcher and an app would use it.

use std::path::Path;

use craft_ai_client::FolderDesk;
use craft_ai_comfy_env::parse_extra_paths;
use craft_ai_core::model_folders::{FolderOrigin, ModelFolder};
use craft_ai_native::shared::SharedStore;

struct World {
    _dir: tempfile::TempDir,
    shared: SharedStore,
    models: tempfile::TempDir,
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let models = tempfile::tempdir().unwrap();
    write(models.path(), "vae/ae.safetensors", b"x");
    write(models.path(), "Lora/style.safetensors", b"x");
    World {
        _dir: dir,
        shared,
        models,
    }
}

fn desk(w: &World) -> FolderDesk {
    FolderDesk::new(w.shared.clone())
}

fn listing(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            out.push(format!(
                "{} {}",
                e.path().display(),
                e.metadata().unwrap().len()
            ));
            if e.path().is_dir() {
                stack.push(e.path());
            }
        }
    }
    out.sort();
    out
}

#[test]
fn a_folder_is_read_stored_and_the_first_becomes_the_default() {
    let w = world();
    let added = desk(&w).add(w.models.path(), Some("My models")).unwrap();
    assert_eq!(added.folder.id, "my-models");
    assert!(added.folder.default && added.folder.origin == FolderOrigin::Chosen);
    assert_eq!(added.folder.kinds["vae"], ["vae"]);
    assert_eq!(added.folder.kinds["loras"], ["Lora"]);
    assert_eq!(added.scan.files.len(), 2);
    // Another app reading the same store sees it.
    let other = FolderDesk::new(SharedStore::new(w.shared.path()))
        .list()
        .unwrap();
    assert_eq!(other, vec![added.folder]);
}

#[test]
fn nothing_in_the_folder_is_changed_by_adding_rescanning_or_removing() {
    let w = world();
    let before = listing(w.models.path());
    let d = desk(&w);
    let id = d.add(w.models.path(), None).unwrap().folder.id;
    d.rescan(&id).unwrap();
    d.remove(&id).unwrap();
    assert_eq!(listing(w.models.path()), before);
}

#[test]
fn the_same_folder_cannot_be_added_twice_however_it_is_spelled() {
    let w = world();
    let d = desk(&w);
    d.add(w.models.path(), None).unwrap();
    let with_slash = format!("{}{}", w.models.path().display(), std::path::MAIN_SEPARATOR);
    let err = d.add(Path::new(&with_slash), None).unwrap_err();
    assert!(err.message.contains("already"), "{}", err.message);
    assert_eq!(d.list().unwrap().len(), 1);
}

#[test]
fn something_that_is_not_a_folder_is_refused_and_nothing_is_stored() {
    let w = world();
    let d = desk(&w);
    assert!(
        d.add(&w.models.path().join("vae/ae.safetensors"), None)
            .is_err()
    );
    assert!(d.add(&w.models.path().join("missing"), None).is_err());
    assert!(d.add(Path::new("relative"), None).is_err());
    assert!(d.list().unwrap().is_empty());
}

#[test]
fn a_rescan_sees_what_was_added_since() {
    let w = world();
    let d = desk(&w);
    let id = d.add(w.models.path(), None).unwrap().folder.id;
    write(w.models.path(), "unet/flux.gguf", b"x");
    let scan = d.rescan(&id).unwrap();
    assert_eq!(scan.files.len(), 3);
    assert_eq!(d.list().unwrap()[0].kinds["diffusion_models"], ["unet"]);
}

#[test]
fn the_default_moves_and_a_linked_folder_can_never_be_it() {
    let w = world();
    let d = desk(&w);
    let second = tempfile::tempdir().unwrap();
    let first = d.add(w.models.path(), Some("one")).unwrap().folder;
    let two = d.add(second.path(), Some("two")).unwrap().folder;
    assert!(first.default && !two.default);
    d.set_default(&two.id).unwrap();
    let now = d.list().unwrap();
    assert_eq!(
        now.iter()
            .filter(|f| f.default)
            .map(|f| f.id.as_str())
            .collect::<Vec<_>>(),
        ["two"]
    );
    w.shared
        .update(|s| {
            s.model_folders.push(ModelFolder {
                id: "comfy".into(),
                label: "ComfyUI".into(),
                path: second.path().to_string_lossy().into_owned(),
                default: false,
                origin: FolderOrigin::ComfyUi,
                kinds: Default::default(),
            });
        })
        .unwrap();
    assert!(
        d.set_default("comfy").is_err(),
        "a read only folder cannot receive downloads"
    );
    assert!(d.set_default("nope").is_err());
}

#[test]
fn removing_the_default_hands_it_to_the_next_chosen_folder() {
    let w = world();
    let d = desk(&w);
    let other = tempfile::tempdir().unwrap();
    let first = d.add(w.models.path(), Some("one")).unwrap().folder;
    d.add(other.path(), Some("two")).unwrap();
    d.remove(&first.id).unwrap();
    let left = d.list().unwrap();
    assert_eq!(left.len(), 1);
    assert!(left[0].default, "downloads still have somewhere to go");
    assert!(d.remove(&first.id).is_err());
}

#[test]
fn the_extra_paths_file_matches_the_folders_and_is_replaced_not_added_to() {
    let w = world();
    let d = desk(&w);
    d.add(w.models.path(), Some("one")).unwrap();
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("ai").join("comfy_extra_paths.yaml");
    d.write_extra_paths(&file).unwrap();
    let mut places = parse_extra_paths(&std::fs::read_to_string(&file).unwrap());
    places.sort();
    assert_eq!(
        places,
        vec![
            ("loras".to_owned(), w.models.path().join("Lora")),
            ("vae".to_owned(), w.models.path().join("vae"))
        ]
    );
    write(w.models.path(), "unet/flux.gguf", b"x");
    let id = d.list().unwrap()[0].id.clone();
    d.rescan(&id).unwrap();
    d.write_extra_paths(&file).unwrap();
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("diffusion_models")
    );
    let names: Vec<_> = std::fs::read_dir(file.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["comfy_extra_paths.yaml"],
        "no temporary file is left behind"
    );
}

#[test]
fn a_proposed_folder_that_does_not_exist_yet_is_made_when_accepted() {
    let w = world();
    let d = desk(&w);
    let proposed = w.models.path().join("ArtCraft").join("models");
    assert!(
        d.add(&proposed, None).is_err(),
        "a plain add never creates anything"
    );
    assert!(!proposed.exists());
    let added = d.add_new(&proposed, Some("ArtCraft models")).unwrap();
    assert!(proposed.is_dir() && added.folder.default && added.scan.files.is_empty());
    assert!(d.add_new(Path::new("relative/models"), None).is_err());
}

#[test]
fn a_very_long_folder_name_is_still_accepted_with_a_shortened_label() {
    let w = world();
    let long = w.models.path().join("m".repeat(130));
    write(&long, "vae/a.safetensors", b"x");
    let added = desk(&w).add(&long, None).unwrap();
    assert_eq!(added.folder.label.chars().count(), 120);
}

// macOS refuses a file name that is not valid text, so the test cannot even make its folder there.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn a_path_that_is_not_valid_text_is_refused_not_stored_as_another_folder() {
    use std::os::unix::ffi::OsStrExt;
    let w = world();
    let odd = w
        .models
        .path()
        .join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
    std::fs::create_dir_all(&odd).unwrap();
    assert!(desk(&w).add(&odd, None).is_err());
    assert!(desk(&w).list().unwrap().is_empty());
}

#[test]
fn removing_the_last_folder_leaves_none_not_the_old_list() {
    let w = world();
    let d = desk(&w);
    let id = d.add(w.models.path(), None).unwrap().folder.id;
    assert_eq!(d.list().unwrap().len(), 1);
    d.remove(&id).unwrap();
    assert!(d.list().unwrap().is_empty());
    // A second reader of the same file sees the same.
    assert!(
        SharedStore::new(w.shared.path())
            .load()
            .unwrap()
            .model_folders
            .is_empty()
    );
}

use super::*;

/// A real `.safetensors` file with the given tensor names and one byte of data each.
fn safetensors(tensors: &[&str]) -> Vec<u8> {
    let mut header = serde_json::Map::new();
    for (i, name) in tensors.iter().enumerate() {
        header.insert(
            (*name).to_owned(),
            serde_json::json!({"dtype": "U8", "shape": [1], "data_offsets": [i, i + 1]}),
        );
    }
    let json = serde_json::to_vec(&header).unwrap();
    let mut out = (json.len() as u64).to_le_bytes().to_vec();
    out.extend(json);
    out.extend(vec![0u8; tensors.len()]);
    out
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn kind_of(scan: &FolderScan, rel: &str) -> Option<(String, Basis)> {
    let f = scan
        .files
        .iter()
        .find(|f| f.rel == rel)
        .unwrap_or_else(|| panic!("{rel} not found in {:?}", scan.files));
    Some((f.kind.clone()?, f.basis?))
}

/// Everything under `root`, with sizes, to prove a scan changed nothing.
fn snapshot(root: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let meta = e.metadata().unwrap();
            out.push((e.path().to_string_lossy().into_owned(), meta.len()));
            if meta.is_dir() {
                stack.push(e.path());
            }
        }
    }
    out.sort();
    out
}

#[test]
fn folders_named_for_a_kind_decide_it_whatever_the_structure_around_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "Stable-Diffusion/sdxl/base.safetensors", b"x");
    write(root, "My Stuff/UNET/flux.gguf", b"x");
    write(root, "loras/style/a.safetensors", b"x");
    write(root, "vae/ae.safetensors", b"x");
    let scan = scan_model_folder(root, &[]).unwrap();
    assert_eq!(
        kind_of(&scan, "Stable-Diffusion/sdxl/base.safetensors"),
        Some(("checkpoints".into(), Basis::FolderName))
    );
    assert_eq!(
        kind_of(&scan, "My Stuff/UNET/flux.gguf"),
        Some(("diffusion_models".into(), Basis::FolderName))
    );
    assert_eq!(scan.kinds["checkpoints"], ["Stable-Diffusion/sdxl"]);
    assert_eq!(scan.kinds["loras"], ["loras/style"]);
    assert_eq!(scan.kinds["vae"], ["vae"]);
    assert_eq!(scan.unrecognised, 0);
}

#[test]
fn the_tensor_names_decide_when_the_folders_say_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "stuff/a.safetensors",
        &safetensors(&["model.diffusion_model.x", "first_stage_model.y"]),
    );
    write(
        root,
        "stuff/b.safetensors",
        &safetensors(&["lora_unet_x.lora_down.weight", "lora_unet_x.lora_up.weight"]),
    );
    write(
        root,
        "stuff/c.safetensors",
        &safetensors(&[
            "encoder.conv_in.weight",
            "decoder.conv_out.weight",
            "quant_conv.weight",
        ]),
    );
    write(
        root,
        "stuff/d.safetensors",
        &safetensors(&["text_model.encoder.layers.0.weight"]),
    );
    write(
        root,
        "stuff/e.safetensors",
        &safetensors(&["control_model.input_blocks.0.weight"]),
    );
    write(
        root,
        "stuff/f.safetensors",
        &safetensors(&["something.else"]),
    );
    let scan = scan_model_folder(root, &[]).unwrap();
    let got = |rel: &str| kind_of(&scan, rel);
    assert_eq!(
        got("stuff/a.safetensors"),
        Some(("checkpoints".into(), Basis::FileHeader))
    );
    assert_eq!(
        got("stuff/b.safetensors"),
        Some(("loras".into(), Basis::FileHeader))
    );
    assert_eq!(
        got("stuff/c.safetensors"),
        Some(("vae".into(), Basis::FileHeader))
    );
    assert_eq!(
        got("stuff/d.safetensors"),
        Some(("text_encoders".into(), Basis::FileHeader))
    );
    assert_eq!(
        got("stuff/e.safetensors"),
        Some(("controlnet".into(), Basis::FileHeader))
    );
    assert_eq!(
        got("stuff/f.safetensors"),
        None,
        "an unfamiliar file is left alone, never guessed"
    );
    assert_eq!(scan.unrecognised, 1);
    assert!(
        !scan.kinds.values().flatten().any(|d| d.is_empty()),
        "the files are all in stuff/"
    );
}

#[test]
fn a_known_name_and_size_wins_over_the_folder_it_sits_in() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "vae/sam3.safetensors", &[7u8; 10]);
    let known = [KnownFile {
        kind: "sam3",
        filename: "SAM3.safetensors",
        size: 10,
    }];
    let scan = scan_model_folder(dir.path(), &known).unwrap();
    assert_eq!(
        kind_of(&scan, "vae/sam3.safetensors"),
        Some(("sam3".into(), Basis::Recipe))
    );
    // The same name at another size is not the known file.
    let wrong = [KnownFile {
        kind: "sam3",
        filename: "sam3.safetensors",
        size: 11,
    }];
    let scan = scan_model_folder(dir.path(), &wrong).unwrap();
    assert_eq!(
        kind_of(&scan, "vae/sam3.safetensors"),
        Some(("vae".into(), Basis::FolderName))
    );
}

#[test]
fn files_directly_in_the_folder_map_to_the_folder_itself() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "a.safetensors",
        &safetensors(&["model.diffusion_model.x"]),
    );
    let scan = scan_model_folder(dir.path(), &[]).unwrap();
    assert_eq!(scan.kinds["checkpoints"], [""]);
}

#[test]
fn the_folders_own_name_counts_as_a_hint() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Checkpoints");
    write(&root, "a.safetensors", b"x");
    let scan = scan_model_folder(&root, &[]).unwrap();
    assert_eq!(
        kind_of(&scan, "a.safetensors"),
        Some(("checkpoints".into(), Basis::FolderName))
    );
}

#[test]
fn other_files_are_ignored_and_nothing_on_disk_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "vae/a.safetensors", b"x");
    write(root, "vae/readme.txt", b"hello");
    write(root, "vae/preview.png", b"png");
    let before = snapshot(root);
    let scan = scan_model_folder(root, &[]).unwrap();
    assert_eq!(scan.files.len(), 1);
    assert_eq!(snapshot(root), before, "a scan is read only");
    assert_eq!(
        std::fs::read(root.join("vae/readme.txt")).unwrap(),
        b"hello"
    );
}

#[test]
fn a_damaged_header_gives_no_guess_and_no_panic() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "x/huge.safetensors", &u64::MAX.to_le_bytes());
    write(root, "x/short.safetensors", &[1, 2, 3]);
    write(
        root,
        "x/junk.safetensors",
        &[&5u64.to_le_bytes()[..], b"notjs"].concat(),
    );
    write(root, "x/empty.safetensors", b"");
    write(root, "x/zero.safetensors", &0u64.to_le_bytes());
    let scan = scan_model_folder(root, &[]).unwrap();
    assert_eq!(scan.files.len(), 5);
    assert_eq!(scan.unrecognised, 5);
}

#[test]
fn a_folder_that_is_too_deep_is_cut_off_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a/b/c/d/e/f/ok.safetensors", b"x");
    write(dir.path(), "a/b/c/d/e/f/g/too_deep.safetensors", b"x");
    let scan = scan_model_folder(dir.path(), &[]).unwrap();
    assert!(scan.files.iter().any(|f| f.rel.ends_with("ok.safetensors")));
    assert!(
        !scan
            .files
            .iter()
            .any(|f| f.rel.ends_with("too_deep.safetensors"))
    );
    assert!(scan.truncated);
}

#[test]
fn something_that_is_not_a_folder_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "file.safetensors", b"x");
    assert_eq!(
        scan_model_folder(&dir.path().join("file.safetensors"), &[]),
        Err(ScanError::NotAFolder)
    );
    assert_eq!(
        scan_model_folder(&dir.path().join("missing"), &[]),
        Err(ScanError::NotAFolder)
    );
    assert_eq!(
        scan_model_folder(Path::new("relative/path"), &[]),
        Err(ScanError::NotAFolder)
    );
}

#[cfg(unix)]
#[test]
fn a_link_back_to_the_folder_does_not_loop() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "vae/a.safetensors", b"x");
    std::os::unix::fs::symlink(dir.path(), dir.path().join("vae/loop")).unwrap();
    let scan = scan_model_folder(dir.path(), &[]).unwrap();
    assert_eq!(scan.files.len(), 1);
}

#[cfg(unix)]
#[test]
fn a_linked_folder_is_followed_once() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    write(elsewhere.path(), "m.safetensors", b"x");
    std::os::unix::fs::symlink(elsewhere.path(), dir.path().join("loras")).unwrap();
    let scan = scan_model_folder(dir.path(), &[]).unwrap();
    assert_eq!(
        kind_of(&scan, "loras/m.safetensors"),
        Some(("loras".into(), Basis::FolderName))
    );
}

#[test]
fn a_folder_with_too_many_entries_ends_early_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..40 {
        std::fs::create_dir_all(dir.path().join(format!("d{i:02}"))).unwrap();
    }
    std::fs::write(dir.path().join("zz.safetensors"), b"x").unwrap();
    let scan = scan_with_limits(dir.path(), &[], MAX_FILES, 10).unwrap();
    assert!(scan.truncated, "only ten entries were looked at");
    let full = scan_with_limits(dir.path(), &[], MAX_FILES, 1000).unwrap();
    assert!(!full.truncated);
    assert_eq!(full.files.len(), 1);
}

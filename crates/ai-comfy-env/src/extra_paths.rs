//! Writes the model folders as a ComfyUI `extra_model_paths.yaml`, for a ComfyUI the suite starts.
//!
//! ComfyUI reads such a file when it is started with `--extra-model-paths-config <file>`, so the
//! suite can tell its own ComfyUI where the person's models are without touching the person's
//! ComfyUI settings. Folders linked from ComfyUI itself are left out: it already knows them.
//!
//! The text uses the small subset of YAML that [`parse_extra_paths`](crate::parse_extra_paths)
//! reads back, so what is written can be checked by reading it again.

use craft_ai_core::model_folders::{FolderOrigin, ModelFolder, validate_folders};

/// `'text'` with a quote inside doubled, which is how YAML writes one in single quotes.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The file text for these folders.
pub fn render_extra_paths(folders: &[ModelFolder]) -> String {
    let mut out = String::from(
        "# Written by ArtCraft from the model folders you chose. It is replaced whenever they change.\n",
    );
    // A folder that would not pass the store's checks (a newline in its path, say) is left out
    // rather than written into a file another program reads.
    let sound = |f: &&ModelFolder| validate_folders(std::slice::from_ref(*f)).is_ok();
    for folder in folders
        .iter()
        .filter(|f| f.origin == FolderOrigin::Chosen)
        .filter(sound)
    {
        out.push_str(&format!(
            "artcraft-{}:\n    base_path: {}\n",
            folder.id,
            quoted(&folder.path)
        ));
        if folder.default {
            out.push_str("    is_default: true\n");
        }
        for (kind, dirs) in &folder.kinds {
            let dirs: Vec<&str> = dirs
                .iter()
                .map(|d| if d.is_empty() { "." } else { d.as_str() })
                .collect();
            match dirs.as_slice() {
                [] => {}
                [one] => out.push_str(&format!("    {kind}: {}\n", quoted(one))),
                many => {
                    out.push_str(&format!("    {kind}: |\n"));
                    for d in many {
                        out.push_str(&format!("        {d}\n"));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_extra_paths;
    use std::collections::BTreeMap;

    fn folder(id: &str, path: &std::path::Path, kinds: &[(&str, &[&str])]) -> ModelFolder {
        ModelFolder {
            id: id.into(),
            label: id.into(),
            path: path.to_string_lossy().into_owned(),
            default: false,
            origin: FolderOrigin::Chosen,
            kinds: kinds
                .iter()
                .map(|(k, d)| ((*k).to_owned(), d.iter().map(|s| (*s).to_owned()).collect()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn what_is_written_reads_back_to_the_same_places() {
        let dir = tempfile::tempdir().unwrap();
        // A name with a space and an apostrophe, the cases a hand-written file gets wrong.
        let root = dir.path().join("Sam's models");
        for sub in ["sd/xl", "extra", "vae"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        let mut f = folder(
            "main",
            &root,
            &[
                ("checkpoints", &["sd/xl", "extra"]),
                ("vae", &["vae"]),
                ("loras", &[""]),
            ],
        );
        f.default = true;
        let text = render_extra_paths(&[f]);
        assert!(text.contains("is_default: true"));
        let mut read = parse_extra_paths(&text);
        read.sort();
        let mut expect = vec![
            ("checkpoints".to_owned(), root.join("sd/xl")),
            ("checkpoints".to_owned(), root.join("extra")),
            ("vae".to_owned(), root.join("vae")),
            ("loras".to_owned(), root.join(".")),
        ];
        expect.sort();
        assert_eq!(read, expect, "{text}");
    }

    #[test]
    fn a_folder_linked_from_comfyui_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let mut linked = folder("comfy", dir.path(), &[("vae", &[""])]);
        linked.origin = FolderOrigin::ComfyUi;
        let text = render_extra_paths(&[linked]);
        assert!(!text.contains("artcraft-comfy"), "{text}");
        assert!(parse_extra_paths(&text).is_empty());
    }

    #[test]
    fn a_folder_that_could_corrupt_the_file_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let mut evil = folder("evil", dir.path(), &[]);
        evil.path = format!(
            "{}
base_path: /etc",
            dir.path().display()
        );
        let mut escaping = folder("escape", dir.path(), &[("vae", &["../up"])]);
        escaping.default = true;
        let good = folder("good", dir.path(), &[("vae", &["vae"])]);
        let text = render_extra_paths(&[evil, escaping, good]);
        assert!(!text.contains("evil") && !text.contains("escape") && !text.contains("/etc"));
        assert!(text.contains("artcraft-good:"), "{text}");
    }

    #[test]
    fn nothing_listed_writes_only_the_header() {
        let dir = tempfile::tempdir().unwrap();
        let text = render_extra_paths(&[folder("empty", dir.path(), &[])]);
        assert!(text.starts_with('#') && text.contains("artcraft-empty:"));
        assert!(parse_extra_paths(&text).is_empty());
    }
}

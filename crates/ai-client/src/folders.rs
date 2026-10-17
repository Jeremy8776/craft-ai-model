//! The person's model folders: add, rescan, choose the default, remove, and hand the map to a
//! ComfyUI the suite starts.
//!
//! Nothing here changes a model file. Adding a folder reads it; removing one forgets it (the
//! files stay where they are). The map is stored in the shared file so every app and the
//! launcher see the same folders.

use std::path::{Path, PathBuf};

use craft_ai_comfy_env::{FolderScan, KnownFile, render_extra_paths, scan_model_folder};
use craft_ai_core::model_folders::{FolderOrigin, ModelFolder};
use craft_ai_core::{AiError, ErrorKind};
use craft_ai_native::shared::SharedStore;

/// What adding a folder found.
#[derive(Debug)]
pub struct FolderAdded {
    pub folder: ModelFolder,
    pub scan: FolderScan,
}

pub struct FolderDesk {
    shared: SharedStore,
}

impl FolderDesk {
    pub fn new(shared: SharedStore) -> Self {
        Self { shared }
    }

    pub fn list(&self) -> Result<Vec<ModelFolder>, AiError> {
        Ok(self.shared.load()?.model_folders)
    }

    /// Registers a folder the person chose and reads what is in it. The files are not touched.
    pub fn add(&self, path: &Path, label: Option<&str>) -> Result<FolderAdded, AiError> {
        // The path is stored as text, so one that is not valid text cannot be kept faithfully:
        // refuse it rather than store a different folder from the one chosen.
        let text = path
            .to_str()
            .ok_or_else(|| {
                AiError::invalid("that folder's name has characters this app cannot keep")
            })?
            .to_owned();
        let scan = scan(path)?;
        let name = label
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
            .map(|n| tidy_label(&n))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Models".to_owned());
        let identity = same_place_key(path);
        let added = self.shared.update(|store| {
            if store
                .model_folders
                .iter()
                .any(|f| same_place_key(Path::new(&f.path)) == identity)
            {
                return Err(AiError::new(
                    ErrorKind::InvalidRequest,
                    "that folder is already in the list",
                ));
            }
            let id = unique_id(&name, &store.model_folders);
            let default = !store
                .model_folders
                .iter()
                .any(|f| f.default && f.origin == FolderOrigin::Chosen);
            let folder = ModelFolder {
                id,
                label: name.clone(),
                path: text.clone(),
                default,
                origin: FolderOrigin::Chosen,
                kinds: scan.kinds.clone(),
            };
            store.model_folders.push(folder.clone());
            Ok(folder)
        })??;
        Ok(FolderAdded {
            folder: added,
            scan,
        })
    }

    /// Makes the folder (and any missing parents) if it is not there yet, then registers it. For
    /// the folder the launcher proposes, which does not exist until the person accepts it.
    pub fn add_new(&self, path: &Path, label: Option<&str>) -> Result<FolderAdded, AiError> {
        if !path.is_absolute() {
            return Err(AiError::new(
                ErrorKind::InvalidRequest,
                "that is not a full folder path",
            ));
        }
        std::fs::create_dir_all(path)
            .map_err(|_| AiError::new(ErrorKind::Internal, "that folder could not be created"))?;
        self.add(path, label)
    }

    /// Reads the folder again, for files added or moved since. Replaces its map.
    pub fn rescan(&self, id: &str) -> Result<FolderScan, AiError> {
        let folder = self.find(id)?;
        let scan = scan(Path::new(&folder.path))?;
        let kinds = scan.kinds.clone();
        self.shared.update(|store| {
            if let Some(f) = store.model_folders.iter_mut().find(|f| f.id == id) {
                f.kinds = kinds;
            }
        })?;
        Ok(scan)
    }

    /// Makes `id` the folder new downloads go to. A folder read from another program cannot be.
    pub fn set_default(&self, id: &str) -> Result<(), AiError> {
        if self.find(id)?.origin != FolderOrigin::Chosen {
            return Err(AiError::unsupported(
                "a folder read from another program is read only, so nothing can be saved to it",
            ));
        }
        self.shared.update(|store| {
            for f in &mut store.model_folders {
                f.default = f.id == id;
            }
        })?;
        Ok(())
    }

    /// Forgets the folder. Its files stay where they are. The default moves to the next folder
    /// the person chose, so downloads still have somewhere to go.
    pub fn remove(&self, id: &str) -> Result<(), AiError> {
        self.find(id)?;
        self.shared.update(|store| {
            store.model_folders.retain(|f| f.id != id);
            let has_default = store.model_folders.iter().any(|f| f.default);
            let next = store
                .model_folders
                .iter_mut()
                .find(|f| f.origin == FolderOrigin::Chosen);
            if let (false, Some(next)) = (has_default, next) {
                next.default = true;
            }
        })?;
        Ok(())
    }

    /// Writes the folders as a ComfyUI extra-paths file at `file` (replacing it), for a ComfyUI
    /// started with `--extra-model-paths-config`. Never touches any other file.
    pub fn write_extra_paths(&self, file: &Path) -> Result<(), AiError> {
        let text = render_extra_paths(&self.list()?);
        let io = |_| {
            AiError::new(
                ErrorKind::Internal,
                "the model folder list could not be written for ComfyUI",
            )
        };
        let dir = file.parent().ok_or_else(|| io(()))?;
        std::fs::create_dir_all(dir).map_err(|_| io(()))?;
        let mut temp = file.as_os_str().to_owned();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        std::fs::write(&temp, text).map_err(|_| io(()))?;
        std::fs::rename(&temp, file).map_err(|_| {
            let _ = std::fs::remove_file(&temp);
            io(())
        })
    }

    fn find(&self, id: &str) -> Result<ModelFolder, AiError> {
        self.list()?
            .into_iter()
            .find(|f| f.id == id)
            .ok_or_else(|| AiError::invalid("there is no model folder with that name"))
    }
}

/// Reads a folder, knowing the models the built-in tools name by file name and size, so a
/// folder holding one is recognised whatever it is called around it.
fn scan(path: &Path) -> Result<FolderScan, AiError> {
    let owned: Vec<(String, String, u64)> = craft_ai_features::builtin()
        .into_iter()
        .filter_map(|f| f.local)
        .flat_map(|recipe| recipe.models)
        .map(|m| (m.folder, m.filename, m.size_bytes))
        .collect();
    let known: Vec<KnownFile<'_>> = owned
        .iter()
        .map(|(kind, filename, size)| KnownFile {
            kind,
            filename,
            size: *size,
        })
        .collect();
    scan_model_folder(path, &known)
        .map_err(|e| AiError::new(ErrorKind::InvalidRequest, e.to_string()))
}

/// Two spellings of one place compare equal: resolved when the folder exists, else lower-cased.
fn same_place_key(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_owned())
        .to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_lowercase()
}

/// A label the store will accept: control characters dropped, at most 120 characters.
fn tidy_label(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// `My Models!` becomes `my-models`; a clash gets `-2`, `-3` ...
fn unique_id(name: &str, taken: &[ModelFolder]) -> String {
    let mut slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    slug = slug
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    slug.truncate(30);
    let slug = slug.trim_matches('-');
    let base = if slug.is_empty() { "models" } else { slug };
    let mut id = base.to_owned();
    let mut n = 2;
    while taken.iter().any(|f| f.id == id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_is_made_acceptable_not_refused() {
        assert_eq!(tidy_label("  My\tModels\n "), "MyModels");
        assert_eq!(tidy_label(&"é".repeat(300)).chars().count(), 120);
        assert_eq!(tidy_label("\x07"), "");
    }

    #[test]
    fn names_become_short_unique_ids() {
        let none: Vec<ModelFolder> = vec![];
        assert_eq!(unique_id("My Models!", &none), "my-models");
        assert_eq!(unique_id("!!!", &none), "models");
        assert_eq!(unique_id("Ünï", &none), "n");
        let taken = vec![ModelFolder {
            id: "models".into(),
            label: "m".into(),
            path: "/m".into(),
            default: false,
            origin: FolderOrigin::Chosen,
            kinds: Default::default(),
        }];
        assert_eq!(unique_id("Models", &taken), "models-2");
        assert!(unique_id(&"x".repeat(200), &none).len() <= 30);
    }
}

//! Folders on this computer where a person keeps model files.
//!
//! A person points the suite at a folder they already have, in whatever structure they like; the
//! suite never moves, renames or deletes anything in it. What is stored is a *map*: for each kind
//! of model (checkpoints, vae, sam3 ...) which sub-folders of that folder hold files of that kind.
//! The names are ComfyUI's, so a ComfyUI we start can be told the same map and the built-in
//! loaders read the same files.
//!
//! This module is pure data and checks. It never touches the disk (it compiles for wasm); the
//! native crates scan folders and fill in the map.
//!
//! Invariants:
//! - A path is stored as text and must be absolute. A sub-folder is relative, uses `/`, and never
//!   contains `..`, so a map cannot point outside its folder.
//! - A folder linked from another program (`FolderOrigin::ComfyUi`) is read only: it is never
//!   chosen to receive a download.
//! - At most one folder is the default for downloads.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Most folders a person can register.
pub const MAX_FOLDERS: usize = 32;
/// Most sub-folders listed per kind in one folder.
pub const MAX_DIRS_PER_KIND: usize = 64;

/// The model kinds the suite proposes a sub-folder for, using ComfyUI's folder names.
pub const KINDS: &[&str] = &[
    "checkpoints",
    "diffusion_models",
    "text_encoders",
    "vae",
    "loras",
    "controlnet",
    "clip_vision",
    "upscale_models",
    "embeddings",
    "sam3",
    "background_removal",
];

/// Where a folder came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderOrigin {
    /// The person pointed at it (or accepted the suggested one).
    #[default]
    Chosen,
    /// Read from another program's settings. Read only.
    ComfyUi,
}

/// One registered folder and what is known to be in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelFolder {
    /// A short name, unique among folders: lower-case letters, digits and `-`.
    pub id: String,
    pub label: String,
    /// Absolute path, as text.
    pub path: String,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub origin: FolderOrigin,
    /// Kind to the sub-folders of `path` that hold files of that kind (`""` is the folder itself).
    #[serde(default)]
    pub kinds: BTreeMap<String, Vec<String>>,
}

/// Why a folder list was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderError {
    TooMany(usize),
    BadId(String),
    DuplicateId(String),
    BadLabel(String),
    NotAbsolute(String),
    BadKind { folder: String, kind: String },
    BadSubfolder { folder: String, sub: String },
    TooManyDirs { folder: String, kind: String },
    SeveralDefaults,
}

impl std::fmt::Display for FolderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooMany(n) => write!(f, "{n} model folders exceeds the limit of {MAX_FOLDERS}"),
            Self::BadId(id) => write!(f, "\"{id}\" is not a valid folder name"),
            Self::DuplicateId(id) => write!(f, "model folder {id} appears twice"),
            Self::BadLabel(id) => write!(f, "model folder {id} needs a short name"),
            Self::NotAbsolute(id) => write!(f, "model folder {id} needs a full path"),
            Self::BadKind { folder, kind } => write!(
                f,
                "model folder {folder}: \"{kind}\" is not a kind of model"
            ),
            Self::BadSubfolder { folder, sub } => write!(
                f,
                "model folder {folder}: \"{sub}\" is not inside the folder"
            ),
            Self::TooManyDirs { folder, kind } => write!(
                f,
                "model folder {folder}: too many places listed for {kind}"
            ),
            Self::SeveralDefaults => write!(f, "only one model folder can be the default"),
        }
    }
}

impl std::error::Error for FolderError {}

/// A kind name: lower-case letters, digits and `_`, starting with a letter.
pub fn is_kind(name: &str) -> bool {
    name.len() <= 40
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A sub-folder: relative, `/` separated, no `..`, no `:` or control characters. `""` is allowed.
pub fn is_subfolder(sub: &str) -> bool {
    sub.len() <= 260
        && !sub.starts_with('/')
        && !sub.contains(['\\', ':'])
        && !sub.chars().any(char::is_control)
        && sub
            .split('/')
            .all(|part| part != ".." && part != "." && (part.is_empty() == sub.is_empty()))
}

/// Does this text look like a full path on Windows, macOS or Linux?
pub fn looks_absolute(path: &str) -> bool {
    let b = path.as_bytes();
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'\\' || b[2] == b'/'))
}

/// Checks a whole list before it is stored.
pub fn validate_folders(folders: &[ModelFolder]) -> Result<(), FolderError> {
    if folders.len() > MAX_FOLDERS {
        return Err(FolderError::TooMany(folders.len()));
    }
    if folders.iter().filter(|f| f.default).count() > 1 {
        return Err(FolderError::SeveralDefaults);
    }
    for (i, folder) in folders.iter().enumerate() {
        let id = &folder.id;
        let id_ok = !id.is_empty()
            && id.len() <= 40
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !id_ok {
            return Err(FolderError::BadId(id.clone()));
        }
        if folders.iter().skip(i + 1).any(|o| o.id == *id) {
            return Err(FolderError::DuplicateId(id.clone()));
        }
        if folder.label.trim().is_empty()
            || folder.label.len() > 120
            || folder.label.chars().any(char::is_control)
        {
            return Err(FolderError::BadLabel(id.clone()));
        }
        if !looks_absolute(&folder.path)
            || folder.path.chars().any(char::is_control)
            || folder.path.len() > 1024
        {
            return Err(FolderError::NotAbsolute(id.clone()));
        }
        for (kind, dirs) in &folder.kinds {
            if !is_kind(kind) {
                return Err(FolderError::BadKind {
                    folder: id.clone(),
                    kind: kind.clone(),
                });
            }
            if dirs.len() > MAX_DIRS_PER_KIND {
                return Err(FolderError::TooManyDirs {
                    folder: id.clone(),
                    kind: kind.clone(),
                });
            }
            if let Some(sub) = dirs.iter().find(|d| !is_subfolder(d)) {
                return Err(FolderError::BadSubfolder {
                    folder: id.clone(),
                    sub: sub.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Where a new file of `kind` should be saved: the default folder (else the first folder the
/// person chose), and inside it the first place already holding that kind, else a sub-folder
/// named for the kind. Linked folders never receive downloads.
pub fn download_target<'a>(
    folders: &'a [ModelFolder],
    kind: &str,
) -> Option<(&'a ModelFolder, String)> {
    let writable = |f: &&ModelFolder| f.origin == FolderOrigin::Chosen;
    let folder = folders
        .iter()
        .filter(writable)
        .find(|f| f.default)
        .or_else(|| folders.iter().find(writable))?;
    let sub = match folder.kinds.get(kind).and_then(|dirs| dirs.first()) {
        Some(mapped) => mapped.clone(),
        // No place yet: the kind names a new sub-folder, so it must be a safe one. A caller passing
        // `..` or a path gets nowhere to save rather than a file outside the folder.
        None if !kind.is_empty() && is_subfolder(kind) => kind.to_owned(),
        None => return None,
    };
    Some((folder, sub))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, path: &str) -> ModelFolder {
        ModelFolder {
            id: id.into(),
            label: id.into(),
            path: path.into(),
            default: false,
            origin: FolderOrigin::Chosen,
            kinds: BTreeMap::new(),
        }
    }

    #[test]
    fn a_sound_list_is_accepted() {
        let mut f = folder("main", "D:\\Models");
        f.kinds
            .insert("checkpoints".into(), vec!["".into(), "sd/xl".into()]);
        f.default = true;
        assert_eq!(
            validate_folders(&[f, folder("extra", "/mnt/models")]),
            Ok(())
        );
    }

    #[test]
    fn paths_must_be_full_paths() {
        assert!(
            looks_absolute("C:\\x")
                && looks_absolute("C:/x")
                && looks_absolute("/x")
                && looks_absolute("\\\\server\\share")
        );
        assert!(
            !looks_absolute("models")
                && !looks_absolute("..\\models")
                && !looks_absolute("C:models")
                && !looks_absolute("")
        );
        assert_eq!(
            validate_folders(&[folder("a", "models")]),
            Err(FolderError::NotAbsolute("a".into()))
        );
    }

    #[test]
    fn a_map_cannot_point_outside_its_folder() {
        for bad in [
            "..", "../x", "a/../b", "/abs", "C:/x", "a\\b", "./a", "a//b",
        ] {
            assert!(!is_subfolder(bad), "{bad}");
            let mut f = folder("a", "/m");
            f.kinds.insert("vae".into(), vec![bad.into()]);
            assert!(
                matches!(
                    validate_folders(&[f]),
                    Err(FolderError::BadSubfolder { .. })
                ),
                "{bad}"
            );
        }
        for good in ["", "vae", "sd/xl", "My Models/vae"] {
            assert!(is_subfolder(good), "{good}");
        }
    }

    #[test]
    fn names_and_counts_are_checked() {
        assert_eq!(
            validate_folders(&[folder("Bad Name", "/m")]),
            Err(FolderError::BadId("Bad Name".into()))
        );
        assert_eq!(
            validate_folders(&[folder("", "/m")]),
            Err(FolderError::BadId(String::new()))
        );
        assert_eq!(
            validate_folders(&[folder("a", "/m"), folder("a", "/n")]),
            Err(FolderError::DuplicateId("a".into()))
        );
        let mut many: Vec<_> = (0..=MAX_FOLDERS)
            .map(|i| folder(&format!("f{i}"), "/m"))
            .collect();
        assert_eq!(
            validate_folders(&many),
            Err(FolderError::TooMany(MAX_FOLDERS + 1))
        );
        many.truncate(2);
        many[0].default = true;
        many[1].default = true;
        assert_eq!(validate_folders(&many), Err(FolderError::SeveralDefaults));
        let mut f = folder("a", "/m");
        f.kinds.insert("Not A Kind".into(), vec![]);
        assert!(matches!(
            validate_folders(&[f]),
            Err(FolderError::BadKind { .. })
        ));
        let mut f = folder("a", "/m");
        f.label = "  ".into();
        assert_eq!(
            validate_folders(&[f]),
            Err(FolderError::BadLabel("a".into()))
        );
    }

    #[test]
    fn downloads_go_to_the_default_folder_and_never_to_a_linked_one() {
        let mut linked = folder("comfy", "/c/models");
        linked.origin = FolderOrigin::ComfyUi;
        linked.default = true;
        let mut mine = folder("mine", "/m");
        mine.kinds.insert("vae".into(), vec!["weights/vae".into()]);
        let list = [linked, folder("other", "/o"), mine];
        let (f, sub) = download_target(&list, "vae").unwrap();
        assert_eq!(
            f.id, "other",
            "the default is a linked folder, so the first chosen one is used"
        );
        assert_eq!(
            sub, "vae",
            "a kind with no place yet gets a sub-folder named for it"
        );
        let mut list = list;
        list[2].default = true;
        list[0].default = false;
        let (f, sub) = download_target(&list, "vae").unwrap();
        assert_eq!(
            (f.id.as_str(), sub.as_str()),
            ("mine", "weights/vae"),
            "an existing place is reused"
        );
        assert!(
            download_target(&list[..1], "vae").is_none(),
            "only linked folders: nowhere to save"
        );
    }

    #[test]
    fn a_kind_that_would_leave_the_folder_is_never_a_place_to_save() {
        let list = [folder("mine", "/m")];
        for bad in ["..", "../x", "/etc", "a/../b", "C:/x", ""] {
            assert!(download_target(&list, bad).is_none(), "{bad:?}");
        }
        assert_eq!(download_target(&list, "sam3").unwrap().1, "sam3");
    }

    #[test]
    fn the_proposed_kinds_are_valid_names() {
        assert!(KINDS.iter().all(|k| is_kind(k)));
    }

    #[test]
    fn an_older_store_without_folders_loads_and_a_bad_list_is_refused() {
        use crate::store::{StoreError, StoreFile};
        let old = serde_json::to_string(&StoreFile::default()).unwrap();
        assert!(
            !old.contains("model_folders"),
            "an empty list is not written, so older apps read the file"
        );
        assert_eq!(StoreFile::parse(&old).unwrap().model_folders, vec![]);
        let mut store = StoreFile::default();
        store.model_folders.push(folder("main", "/models"));
        let text = serde_json::to_string(&store).unwrap();
        assert_eq!(StoreFile::parse(&text).unwrap().model_folders.len(), 1);
        store.model_folders.push(folder("main", "/other"));
        let bad = serde_json::to_string(&store).unwrap();
        assert!(matches!(
            StoreFile::parse(&bad),
            Err(StoreError::Folders(FolderError::DuplicateId(_)))
        ));
    }

    #[test]
    fn a_folder_survives_json() {
        let mut f = folder("main", "D:\\Models");
        f.kinds.insert("sam3".into(), vec!["sam3".into()]);
        let back: ModelFolder = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(back, f);
        let minimal: ModelFolder =
            serde_json::from_str(r#"{"id":"a","label":"A","path":"/m"}"#).unwrap();
        assert_eq!(
            (minimal.default, minimal.origin),
            (false, FolderOrigin::Chosen)
        );
    }
}

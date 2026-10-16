//! Which of a recipe's node packs are in the person's ComfyUI.
//!
//! This only reads. It lists the folder names in `custom_nodes` and the registry of packs this app
//! installed, and compares names. It never opens a file inside a pack, never reads through a link
//! (a link is only checked for pointing at a folder), never joins a name from a recipe onto a
//! path, and never runs, installs or changes anything.

use craft_ai_core::AiError;
use craft_ai_features::NodePack;

use super::super::registry::Registry;
use crate::detect::ComfyInstall;

/// Where one node pack stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackStatus {
    /// A folder with the pack's name is in `custom_nodes`, but this app did not put it there (or
    /// it is a link, or the folder is no longer the one this app wrote).
    Installed,
    /// This app installed it, and the folder it wrote is still there. `commit` is the full commit
    /// it recorded at the time.
    InstalledHere { commit: String },
    /// No folder with any of the pack's known names.
    Missing,
}

/// One line of the report, in the order the packs were asked about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackReport {
    /// The pack's name for people, from the recipe.
    pub label: String,
    /// The folder as it is spelt on disk, when one was found.
    pub folder: Option<String>,
    pub status: PackStatus,
}

struct Folder {
    /// As spelt on disk.
    name: String,
    /// A real folder, not a link to somewhere else.
    real: bool,
}

/// Reports `installed`, `installed here` or `missing` for each pack a recipe names.
///
/// A damaged or oversized registry is an error, never read as "nothing installed here".
pub fn pack_status(
    install: &ComfyInstall,
    registry: &Registry,
    wanted: &[NodePack],
) -> Result<Vec<PackReport>, AiError> {
    let recorded = registry.packs()?;
    let present = folders(install);
    Ok(wanted
        .iter()
        .map(|pack| {
            let found = pack
                .folder_names
                .iter()
                .find_map(|known| present.iter().find(|f| f.name.eq_ignore_ascii_case(known)));
            let Some(folder) = found else {
                return PackReport {
                    label: pack.label.clone(),
                    folder: None,
                    status: PackStatus::Missing,
                };
            };
            // Only a real folder can be the one this app wrote.
            let ours = folder
                .real
                .then(|| {
                    recorded
                        .iter()
                        .find(|r| r.folder.eq_ignore_ascii_case(&folder.name))
                })
                .flatten();
            PackReport {
                label: pack.label.clone(),
                folder: Some(folder.name.clone()),
                status: match ours {
                    Some(entry) => PackStatus::InstalledHere {
                        commit: entry.commit.clone(),
                    },
                    None => PackStatus::Installed,
                },
            }
        })
        .collect())
}

/// The folders (and links to folders) directly inside `custom_nodes`. Hidden folders and
/// `__pycache__` are not packs, as in [`crate::scan`].
fn folders(install: &ComfyInstall) -> Vec<Folder> {
    let mut out: Vec<Folder> = std::fs::read_dir(&install.custom_nodes)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let kind = entry.file_type().ok()?;
            let name = entry.file_name().into_string().ok()?;
            if name.starts_with('.') || name == "__pycache__" {
                return None;
            }
            // `file_type` does not follow a link, so a link is told apart from a real folder.
            let real = kind.is_dir();
            let linked_folder = kind.is_symlink() && entry.path().is_dir();
            (real || linked_folder).then_some(Folder { name, real })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

//! Where the AI files live on disk. This is the per-OS location the launcher writes and every app
//! reads (a proposal until it is settled with the launcher's maintainers).
//!
//! | OS | Directory |
//! |---|---|
//! | Windows | `%LOCALAPPDATA%\ArtCraft\ai` |
//! | macOS | `~/Library/Application Support/ArtCraft/ai` |
//! | Linux | `$XDG_CONFIG_HOME/ArtCraft/ai` (default `~/.config/ArtCraft/ai`) |
//!
//! Local, not roaming, on Windows: the connections list should not follow a user to another
//! machine where the keychain entries it points at do not exist.
//!
//! `CRAFT_AI_DIR` overrides the directory (headless runs, tests, portable installs).

use std::path::PathBuf;

/// Environment variable that replaces the default directory.
pub const DIR_ENV: &str = "CRAFT_AI_DIR";
/// The launcher's shared store, a [`craft_ai_core::StoreFile`] as JSON.
pub const STORE_FILE: &str = "connections.json";
/// The passphrase-encrypted secret file, used when no keychain is available.
pub const SECRETS_FILE: &str = "secrets.enc.json";
/// The shared request log, one JSON line per record.
pub const HISTORY_FILE: &str = "jobs.jsonl";
/// The curated model picks, when updated without an app release.
pub const CATALOG_FILE: &str = "catalog.json";
/// The ComfyUI workflow library, one JSON file per imported workflow.
pub const WORKFLOW_DIR: &str = "workflows";

/// The directory holding the AI files, or `None` if the OS reports no config directory.
pub fn ai_dir() -> Option<PathBuf> {
    pick_dir(
        std::env::var_os(DIR_ENV).map(PathBuf::from),
        dirs::config_local_dir(),
    )
}

fn pick_dir(over: Option<PathBuf>, base: Option<PathBuf>) -> Option<PathBuf> {
    match over {
        Some(p) if !p.as_os_str().is_empty() => Some(p),
        _ => base.map(|b| b.join("ArtCraft").join("ai")),
    }
}

/// Full path of the shared store file.
pub fn store_path() -> Option<PathBuf> {
    ai_dir().map(|d| d.join(STORE_FILE))
}

/// Full path of the encrypted secret file.
pub fn secrets_path() -> Option<PathBuf> {
    ai_dir().map(|d| d.join(SECRETS_FILE))
}

/// Full path of the curated picks file that sits over the one built into the apps.
pub fn catalog_path() -> Option<PathBuf> {
    ai_dir().map(|d| d.join(CATALOG_FILE))
}

/// Full path of the request log.
pub fn history_path() -> Option<PathBuf> {
    ai_dir().map(|d| d.join(HISTORY_FILE))
}

/// Default directory for the local ComfyUI workflow library (one JSON file per workflow).
/// `None` when the OS reports no config directory; callers fall back to asking the user.
pub fn workflow_dir() -> Option<PathBuf> {
    ai_dir().map(|d| d.join(WORKFLOW_DIR))
}

/// Default folder for the settings files of data-described services (one file per connection).
pub fn specs_dir() -> Option<PathBuf> {
    ai_dir().map(|d| d.join("specs"))
}

/// The model folder the launcher proposes when a person has none: `models` beside the `ai`
/// directory (so `%LOCALAPPDATA%\\ArtCraft\\models` on Windows). It is only a suggestion; the person
/// can choose any folder instead, and nothing is created until they accept it.
pub fn suggested_models_dir() -> Option<PathBuf> {
    pick_models(
        std::env::var_os(DIR_ENV).map(PathBuf::from),
        dirs::config_local_dir(),
    )
}

fn pick_models(over: Option<PathBuf>, base: Option<PathBuf>) -> Option<PathBuf> {
    match over {
        Some(p) if !p.as_os_str().is_empty() => Some(p.join("models")),
        _ => base.map(|b| b.join("ArtCraft").join("models")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_and_empty_override_is_ignored() {
        let base = Some(PathBuf::from("/cfg"));
        assert_eq!(
            pick_dir(Some(PathBuf::from("/x")), base.clone()),
            Some(PathBuf::from("/x"))
        );
        assert_eq!(
            pick_dir(Some(PathBuf::new()), base),
            Some(PathBuf::from("/cfg").join("ArtCraft").join("ai"))
        );
        assert_eq!(pick_dir(None, None), None);
    }

    #[test]
    fn the_suggested_models_folder_sits_beside_the_ai_folder() {
        assert_eq!(
            pick_models(None, Some(PathBuf::from("/cfg"))),
            Some(PathBuf::from("/cfg").join("ArtCraft").join("models"))
        );
        assert_eq!(
            pick_models(Some(PathBuf::from("/x")), Some(PathBuf::from("/cfg"))),
            Some(PathBuf::from("/x").join("models")),
            "a test or portable run keeps its models with its other files"
        );
        assert_eq!(
            pick_models(Some(PathBuf::new()), Some(PathBuf::from("/cfg"))),
            Some(PathBuf::from("/cfg").join("ArtCraft").join("models"))
        );
        assert_eq!(pick_models(None, None), None);
    }
}

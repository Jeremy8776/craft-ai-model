//! How a feature can run on this computer: which model files and node packs it needs.
//!
//! A recipe is data only. It names files by folder and file name, with the exact size and SHA-256
//! the installer checks after a download. Nothing here touches the disk or the network.

use serde::{Deserialize, Serialize};

/// Where a model file comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// A file in a Hugging Face repository, pinned to a revision when one is known.
    HuggingFace {
        repo: String,
        file: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<String>,
    },
    /// A direct https address.
    Url { url: String },
}

impl Source {
    /// The https address of the file.
    pub fn url(&self) -> String {
        match self {
            Self::Url { url } => url.clone(),
            Self::HuggingFace {
                repo,
                file,
                revision,
            } => format!(
                "https://huggingface.co/{repo}/resolve/{}/{file}",
                revision.as_deref().unwrap_or("main")
            ),
        }
    }
}

/// The terms a model is distributed under, as far as we know them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Licence {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The user must accept before the file is downloaded.
    pub accept_required: bool,
    /// Anything the user should know that the name does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One model file a recipe needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelFile {
    /// Folder under the ComfyUI `models` directory, such as `sam3` or `diffusion_models`.
    pub folder: String,
    pub filename: String,
    pub size_bytes: u64,
    /// Lower-case hex SHA-256 of the whole file.
    pub sha256: String,
    pub source: Source,
    pub licence: Licence,
}

/// A ComfyUI custom node pack the workflow needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePack {
    pub label: String,
    pub repo_url: String,
    /// Folder names the pack is known to install as under `custom_nodes`, lower case.
    pub folder_names: Vec<String>,
}

/// The same model run by an app's own built-in runtime instead of ComfyUI, from the same weights
/// file. The file is found by the launcher (it is the one in the ComfyUI models folder) and
/// recorded on the connection; the app that owns the runtime reads it from there. A recipe may
/// list several runtimes (Candle, Burn); each is its own connection and its own choice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeRoute {
    /// Adapter name on the connection, such as `candle` or `burn`.
    pub provider: String,
    /// Model id on that connection.
    pub model: String,
    /// What the person sees next to the ComfyUI route.
    pub label: String,
}

/// Running a feature on this computer through ComfyUI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalRecipe {
    /// What the model is called, for people: "SAM 3".
    pub label: String,
    /// Apps can run it today. False when the files can be downloaded but no workflow runs them yet.
    #[serde(default = "yes")]
    pub usable: bool,
    /// Id of the workflow template the adapter runs.
    pub workflow: String,
    pub models: Vec<ModelFile>,
    pub packs: Vec<NodePack>,
    /// Graphics memory the workflow needs, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_vram_mb: Option<u32>,
    /// Built-in runtimes that read the same weights file, where an app has one. The first is the
    /// one [`ToolDesk::enable_native`] registers; none means the tool has no built-in runtime.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native: Vec<NativeRoute>,
}

fn yes() -> bool {
    true
}

impl LocalRecipe {
    pub fn total_bytes(&self) -> u64 {
        self.models
            .iter()
            .fold(0u64, |sum, m| sum.saturating_add(m.size_bytes))
    }
}

/// A hosted way to get the same result when this computer cannot run the models.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudOption {
    /// The connection preset key (see `craft_ai_core::PRESETS`) that adds it.
    pub preset: String,
    pub label: String,
    /// What the user should know: billing, and whether the route has been tried.
    pub note: String,
}

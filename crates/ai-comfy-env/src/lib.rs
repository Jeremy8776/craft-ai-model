//! Finds an existing ComfyUI install, says what is in it, and downloads missing model files.
//!
//! - [`ComfyInstall`]: where ComfyUI Desktop (or a folder you point at) keeps models and node packs.
//! - [`scan`]: an [`craft_ai_features::Inventory`] of what is installed, for `craft_ai_features::plan`.
//! - [`Downloader`]: resumable, checksummed model downloads that never send a token to a
//!   storage host. [`verify_file`] re-checks a file already on disk.
//!
//! Native only. Nothing here starts ComfyUI, installs a node pack, or changes an existing file.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod detect;
mod download;
mod hardware;
pub mod portable;
mod scan;
pub mod source;
pub mod system;
mod verify;

pub use detect::{ComfyInstall, parse_extra_paths};
pub use download::{DownloadConfig, Downloader, Phase, Progress};
pub use hardware::{free_disk_bytes, parse_nvidia_smi, vram_mb};
pub use scan::scan;
pub use verify::{hash_file, verify_file};

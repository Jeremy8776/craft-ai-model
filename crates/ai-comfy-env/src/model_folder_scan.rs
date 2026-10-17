//! Looks through a folder of model files a person already has and works out what is in it.
//!
//! Read only: nothing is moved, renamed, written or deleted. The result is advisory. A kind is
//! only ever a best reading, and says on what basis it was made:
//!
//! 1. `Recipe`: the file name and size match a model the suite knows (strongest, still not a hash).
//! 2. `FolderName`: a folder on the way to it is named for a kind (`vae`, `unet`, `Lora` ...).
//! 3. `FileHeader`: the tensor names in a `.safetensors` header look like one kind of model.
//!
//! A file none of these recognise is counted and left out of the map, never guessed.
//!
//! Limits (a hostile or enormous folder must not hang the launcher): depth 6, 20 000 model files,
//! 200 000 entries looked at in all (so choosing a whole drive ends quickly), a 16 MiB header cap,
//! 64 places per kind, and a symlink or junction is followed only once.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use craft_ai_core::model_folders::{KINDS, MAX_DIRS_PER_KIND};

const MAX_DEPTH: usize = 6;
const MAX_FILES: usize = 20_000;
const MAX_ENTRIES: usize = 200_000;
const MAX_HEADER_BYTES: u64 = 16 * 1024 * 1024;
const MODEL_EXTENSIONS: &[&str] = &["safetensors", "gguf", "onnx", "ckpt", "pt", "pth", "bin"];

/// A model the suite knows by name and size, supplied by the caller (for example from the tool
/// recipes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownFile<'a> {
    pub kind: &'a str,
    pub filename: &'a str,
    pub size: u64,
}

/// How a kind was decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    Recipe,
    FolderName,
    FileHeader,
}

/// One model file found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundFile {
    /// Path inside the scanned folder, `/` separated.
    pub rel: String,
    pub size: u64,
    pub kind: Option<String>,
    pub basis: Option<Basis>,
}

/// What a scan found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderScan {
    pub files: Vec<FoundFile>,
    /// Kind to the sub-folders (relative, `""` for the folder itself) that hold files of it.
    pub kinds: BTreeMap<String, Vec<String>>,
    /// Model files no rule recognised.
    pub unrecognised: usize,
    /// The scan stopped early (too deep, too many files, or too many places for a kind).
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanError {
    NotAFolder,
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("that is not a folder this app can read")
    }
}

impl std::error::Error for ScanError {}

/// The kind a folder name stands for, if any.
fn kind_for_folder_name(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    let kind = match n.as_str() {
        "checkpoints" | "checkpoint" | "stable-diffusion" | "stable_diffusion" => "checkpoints",
        "diffusion_models" | "unet" | "unets" => "diffusion_models",
        "text_encoders" | "clip" | "t5" => "text_encoders",
        "vae" => "vae",
        "loras" | "lora" | "lycoris" => "loras",
        "controlnet" => "controlnet",
        "clip_vision" => "clip_vision",
        "upscale_models" | "esrgan" | "realesrgan" | "swinir" => "upscale_models",
        "embeddings" | "textual_inversion" => "embeddings",
        "sam3" => "sam3",
        "background_removal" | "birefnet" | "inspyrenet" | "rmbg" => "background_removal",
        _ => return None,
    };
    KINDS.contains(&kind).then_some(kind)
}

/// Reads a `.safetensors` header (never the weights) and guesses the kind from its tensor names.
pub fn guess_from_header(path: &Path) -> Option<&'static str> {
    let mut file = std::fs::File::open(path).ok()?;
    let total = file.metadata().ok()?.len();
    let mut len = [0u8; 8];
    file.read_exact(&mut len).ok()?;
    let n = u64::from_le_bytes(len);
    if n == 0 || n > MAX_HEADER_BYTES || n.checked_add(8)? > total {
        return None;
    }
    let mut json = vec![0u8; usize::try_from(n).ok()?];
    file.read_exact(&mut json).ok()?;
    let header: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&json).ok()?;
    let names: Vec<&str> = header
        .keys()
        .map(String::as_str)
        .filter(|k| *k != "__metadata__")
        .collect();
    let any = |f: &dyn Fn(&str) -> bool| names.iter().any(|k| f(k));
    if any(&|k| {
        k.contains("lora_down")
            || k.contains("lora_up")
            || k.contains(".lora_A.")
            || k.contains(".lora_B.")
    }) {
        return Some("loras");
    }
    if any(&|k| k.starts_with("control_model.") || k.contains("input_hint_block")) {
        return Some("controlnet");
    }
    if any(&|k| k.starts_with("model.diffusion_model.") || k.starts_with("first_stage_model.")) {
        return Some("checkpoints");
    }
    if any(&|k| k.starts_with("decoder.") || k.starts_with("encoder."))
        && any(&|k| k.contains("quant_conv") || k.contains("conv_in"))
    {
        return Some("vae");
    }
    if any(&|k| {
        k.contains("text_model.encoder")
            || k.starts_with("encoder.block.")
            || k.starts_with("shared.weight")
    }) {
        return Some("text_encoders");
    }
    None
}

fn is_model_file(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| MODEL_EXTENSIONS.iter().any(|m| m.eq_ignore_ascii_case(e)))
}

/// Looks through `root` and everything under it. Never writes.
pub fn scan_model_folder(root: &Path, known: &[KnownFile<'_>]) -> Result<FolderScan, ScanError> {
    scan_with_limits(root, known, MAX_FILES, MAX_ENTRIES)
}

fn scan_with_limits(
    root: &Path,
    known: &[KnownFile<'_>],
    max_files: usize,
    max_entries: usize,
) -> Result<FolderScan, ScanError> {
    if !root.is_absolute() || !std::fs::metadata(root).is_ok_and(|m| m.is_dir()) {
        return Err(ScanError::NotAFolder);
    }
    let root_name = root.file_name().and_then(|n| n.to_str()).map(str::to_owned);
    let mut scan = FolderScan::default();
    let mut places: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    seen.insert(std::fs::canonicalize(root).unwrap_or_else(|_| root.to_owned()));
    // (directory, names of the folders below the root on the way to it)
    let mut stack: Vec<(PathBuf, Vec<String>)> = vec![(root.to_owned(), Vec::new())];
    let mut looked_at = 0usize;
    'walk: while let Some((dir, parts)) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = read.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            looked_at += 1;
            if looked_at > max_entries {
                scan.truncated = true;
                break 'walk;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let path = entry.path();
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                if parts.len() >= MAX_DEPTH {
                    scan.truncated = true;
                    continue;
                }
                let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                if seen.insert(canonical) {
                    let mut below = parts.clone();
                    below.push(name);
                    stack.push((path, below));
                }
                continue;
            }
            if !meta.is_file() || !is_model_file(&name) {
                continue;
            }
            if scan.files.len() >= max_files {
                scan.truncated = true;
                break;
            }
            let (kind, basis) = classify(
                &path,
                &name,
                meta.len(),
                &parts,
                root_name.as_deref(),
                known,
            );
            let sub = parts.join("/");
            if let Some(kind) = kind {
                let set = places.entry(kind.to_owned()).or_default();
                if set.len() < MAX_DIRS_PER_KIND || set.contains(&sub) {
                    set.insert(sub.clone());
                } else {
                    scan.truncated = true;
                }
            } else {
                scan.unrecognised += 1;
            }
            let rel = if sub.is_empty() {
                name
            } else {
                format!("{sub}/{name}")
            };
            scan.files.push(FoundFile {
                rel,
                size: meta.len(),
                kind: kind.map(str::to_owned),
                basis,
            });
        }
    }
    scan.files.sort_by(|a, b| a.rel.cmp(&b.rel));
    scan.kinds = places
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect();
    Ok(scan)
}

fn classify(
    path: &Path,
    name: &str,
    size: u64,
    parts: &[String],
    root_name: Option<&str>,
    known: &[KnownFile<'_>],
) -> (Option<&'static str>, Option<Basis>) {
    let from_recipe = known
        .iter()
        .filter(|k| k.size == size && k.filename.eq_ignore_ascii_case(name))
        .find_map(|k| KINDS.iter().copied().find(|c| *c == k.kind));
    if let Some(kind) = from_recipe {
        return (Some(kind), Some(Basis::Recipe));
    }
    if let Some(kind) = parts
        .iter()
        .rev()
        .map(String::as_str)
        .chain(root_name)
        .find_map(kind_for_folder_name)
    {
        return (Some(kind), Some(Basis::FolderName));
    }
    let is_safetensors = Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("safetensors"));
    if let Some(kind) = is_safetensors.then(|| guess_from_header(path)).flatten() {
        return (Some(kind), Some(Basis::FileHeader));
    }
    (None, None)
}

#[cfg(test)]
#[path = "model_folder_scan_tests.rs"]
mod tests;

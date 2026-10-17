//! Finds an existing ComfyUI installation and the folders it reads models from.
//!
//! ComfyUI Desktop records its base folder in `config.json` under the user's config directory and
//! may add extra model folders in `extra_models_config.yaml`. A plain ComfyUI checkout can be
//! pointed at directly. Nothing here starts ComfyUI or changes anything on disk.

use std::path::{Component, Path, PathBuf};

const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const MAX_EXTRA_DIRS: usize = 400;

/// A ComfyUI installation on this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComfyInstall {
    pub base: PathBuf,
    /// `(folder, directory)` pairs: the folder name ComfyUI uses, such as `sam3`, and where its
    /// files live. A folder can appear more than once when extra paths add to it.
    pub model_dirs: Vec<(String, PathBuf)>,
    pub custom_nodes: PathBuf,
}

impl ComfyInstall {
    /// The ComfyUI Desktop install recorded for this user, if any.
    pub fn discover() -> Option<Self> {
        Self::discover_in(&dirs::config_dir()?.join("ComfyUI"))
    }

    /// Like [`discover`](Self::discover) with an explicit Desktop config directory.
    pub fn discover_in(config_dir: &Path) -> Option<Self> {
        let config = read_small(&config_dir.join("config.json"))?;
        let value: serde_json::Value = serde_json::from_str(&config).ok()?;
        let base = PathBuf::from(value.get("basePath")?.as_str()?);
        let mut install = Self::at(&base)?;
        if let Some(yaml) = read_small(&config_dir.join("extra_models_config.yaml")) {
            install.add_extra(parse_extra_paths(&yaml));
        }
        Some(install)
    }

    /// A ComfyUI folder with a `models` or `custom_nodes` directory in it.
    pub fn at(base: &Path) -> Option<Self> {
        let models = base.join("models");
        let custom_nodes = base.join("custom_nodes");
        if !base.is_absolute() || !(models.is_dir() || custom_nodes.is_dir()) {
            return None;
        }
        let mut install = Self {
            base: base.to_owned(),
            model_dirs: subdirs(&models)
                .into_iter()
                .filter_map(|p| Some((p.file_name()?.to_str()?.to_owned(), p)))
                .collect(),
            custom_nodes,
        };
        if let Some(yaml) = read_small(&base.join("extra_model_paths.yaml")) {
            install.add_extra(parse_extra_paths(&yaml));
        }
        Some(install)
    }

    fn add_extra(&mut self, extra: Vec<(String, PathBuf)>) {
        for entry in extra {
            if self.model_dirs.len() < MAX_EXTRA_DIRS && !self.model_dirs.contains(&entry) {
                self.model_dirs.push(entry);
            }
        }
    }

    /// Where a new file for `folder` should be saved: its first known directory, else
    /// `models/<folder>` under the base. `None` for a folder name that is not a plain name.
    pub fn models_dir_for(&self, folder: &str) -> Option<PathBuf> {
        if folder.is_empty() || folder.contains(['/', '\\', '.', ':']) {
            return None;
        }
        Some(
            self.model_dirs
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(folder))
                .map_or_else(|| self.base.join("models").join(folder), |(_, p)| p.clone()),
        )
    }
}

fn read_small(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect()
}

/// Reads the small YAML subset ComfyUI uses for extra model paths: top-level section names,
/// then indented `base_path: dir` and `folder: path` lines (a path may also follow `|` on the
/// next lines). Relative paths resolve against the section's `base_path`. Only directories that
/// exist are returned.
pub fn parse_extra_paths(text: &str) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut base: Option<PathBuf> = None;
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut block_key: Option<String> = None;
    let flush = |base: &Option<PathBuf>,
                 entries: &mut Vec<(String, String)>,
                 out: &mut Vec<(String, PathBuf)>| {
        for (key, value) in entries.drain(..) {
            let path = PathBuf::from(clean(&value));
            let resolved = if path.is_absolute() {
                Some(path)
            } else {
                base.as_ref().map(|b| b.join(path))
            };
            if let Some(dir) = resolved
                && !dir.components().any(|c| c == Component::ParentDir)
                && dir.is_dir()
            {
                out.push((key, dir));
            }
        }
    };
    for raw in text.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if indent == 0 && trimmed.ends_with(':') {
            flush(&base, &mut entries, &mut out);
            base = None;
            block_key = None;
            continue;
        }
        if let (Some(key), true) = (
            &block_key,
            !trimmed.contains(": ") && !trimmed.ends_with(':'),
        ) {
            entries.push((key.clone(), trimmed.to_owned()));
            continue;
        }
        block_key = None;
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "base_path" => base = Some(PathBuf::from(clean(value))),
            "is_default" => {}
            _ if value == "|" || value == ">" => block_key = Some(key.to_owned()),
            _ if !value.is_empty() => entries.push((key.to_owned(), value.to_owned())),
            _ => {}
        }
    }
    flush(&base, &mut entries, &mut out);
    out.truncate(MAX_EXTRA_DIRS);
    out
}

fn clean(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return value[1..value.len() - 1]
            .replace("''", "'")
            .trim_end_matches(['/', '\\'])
            .to_owned();
    }
    value
        .trim_matches(['"', '\''])
        .trim_end_matches(['/', '\\'])
        .to_owned()
}

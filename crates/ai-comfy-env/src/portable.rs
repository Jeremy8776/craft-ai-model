//! The official portable ComfyUI: fetch it, check it, unpack it safely, and how to start it quietly.
//!
//! The pinned release (name, size, SHA-256) comes from `craft_ai_features::portable_for`. The
//! archive is downloaded with the usual checked, resumable [`Downloader`]; a download that does not
//! match is never unpacked. Unpacking treats the archive as hostile: nothing may land outside the
//! folder (no absolute paths, `..`, drive letters or links), names Windows treats specially are
//! refused, two names that differ only in case are refused, files are created new and never over
//! something, and the limits are counted as the data is read. Nothing in it is run.
//!
//! Everything goes under one folder the caller names (the app's own data folder). The unpacked
//! folder never replaces one that exists. Nothing outside it is touched.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use craft_ai_core::{AiError, ErrorKind};
use craft_ai_features::{Licence, ModelFile, PortableBuild, Source, is_official_build};
use sevenz_rust2::{ArchiveReader, Password};

use crate::detect::ComfyInstall;
use crate::download::{DownloadConfig, Downloader, Progress};
use crate::verify::verify_file;

/// The folder the official archive unpacks to.
pub const PORTABLE_DIR: &str = "ComfyUI_windows_portable";

/// The only hosts the archive may come from, including where GitHub redirects release assets.
pub fn release_hosts() -> Vec<String> {
    vec!["github.com".to_owned(), ".githubusercontent.com".to_owned()]
}

/// How much an archive may be, counted as it is read.
#[derive(Clone, Debug)]
pub struct ExtractLimits {
    pub max_files: u32,
    pub max_bytes: u64,
    pub max_file: u64,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_files: 300_000,
            max_bytes: 20 * 1024 * 1024 * 1024,
            max_file: 8 * 1024 * 1024 * 1024,
        }
    }
}

/// What was unpacked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extracted {
    pub files: u32,
    pub bytes: u64,
}

const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

fn refuse(why: &str) -> AiError {
    AiError::new(ErrorKind::Provider, format!("unsafe archive: {why}"))
}

/// A plain relative path from an archive name, or `None` if it could land anywhere unexpected.
pub fn safe_relative(name: &str) -> Option<PathBuf> {
    if name.is_empty()
        || name.len() > 240
        || name.contains(['\\', '\0', ':', '?', '*', '|', '<', '>', '"'])
    {
        return None;
    }
    let trimmed = name.trim_end_matches('/');
    let mut out = PathBuf::new();
    for part in trimmed.split('/') {
        let lower = part.to_ascii_lowercase();
        let stem = lower.split('.').next().unwrap_or_default();
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || RESERVED.contains(&stem)
        {
            return None;
        }
        out.push(part);
    }
    out.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then_some(out)
}

enum Problem {
    Cancelled,
    Unsafe(&'static str),
    Io(std::io::Error),
}

/// Unpacks `archive` into the new folder `dest` (it must not exist).
pub fn extract_7z(
    archive: &Path,
    dest: &Path,
    limits: &ExtractLimits,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<Extracted, AiError> {
    let mut reader = ArchiveReader::open(archive, Password::empty())
        .map_err(|_| refuse("it could not be read"))?;
    fs::create_dir(dest).map_err(|e| AiError::new(ErrorKind::Provider, e.to_string()))?;
    let mut seen: HashSet<String> = HashSet::new();
    let (mut files, mut bytes) = (0u32, 0u64);
    let mut problem: Option<Problem> = None;
    let result = reader.for_each_entries(|entry, data| {
        if problem.is_some() {
            return Ok(false);
        }
        let mut stop = |p: Problem| {
            problem = Some(p);
            Ok(false)
        };
        if cancel.load(Ordering::Relaxed) {
            return stop(Problem::Cancelled);
        }
        if entry.is_anti_item {
            return stop(Problem::Unsafe("it holds a deletion entry"));
        }
        if entry.has_windows_attributes {
            let attr = entry.windows_attributes;
            let unix_link = attr & 0x8000 != 0 && (attr >> 16) & 0o170_000 == 0o120_000;
            if attr & 0x400 != 0 || unix_link {
                return stop(Problem::Unsafe("it holds a link"));
            }
        }
        let Some(relative) = safe_relative(&entry.name) else {
            return stop(Problem::Unsafe("a file name is not a plain relative path"));
        };
        if !seen.insert(relative.to_string_lossy().to_ascii_lowercase()) {
            return stop(Problem::Unsafe("two names differ only in case"));
        }
        let target = dest.join(&relative);
        if entry.is_directory {
            return match fs::create_dir_all(&target) {
                Ok(()) => Ok(true),
                Err(e) => stop(Problem::Io(e)),
            };
        }
        files += 1;
        if files > limits.max_files {
            return stop(Problem::Unsafe("it holds too many files"));
        }
        if let Some(parent) = target.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            return stop(Problem::Io(e));
        }
        let mut out = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
        {
            Ok(f) => f,
            Err(e) => return stop(Problem::Io(e)),
        };
        let mut buf = vec![0u8; 1 << 20];
        let mut this_file = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return stop(Problem::Cancelled);
            }
            let n = match data.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n as u64,
                Err(e) => return stop(Problem::Io(e)),
            };
            this_file += n;
            bytes += n;
            if this_file > limits.max_file || bytes > limits.max_bytes {
                return stop(Problem::Unsafe("it unpacks to more than allowed"));
            }
            if let Err(e) = out.write_all(buf.get(..n as usize).unwrap_or_default()) {
                return stop(Problem::Io(e));
            }
            progress(bytes);
        }
        Ok(true)
    });
    match (problem, result) {
        (Some(Problem::Cancelled), _) => Err(AiError::new(ErrorKind::Cancelled, "cancelled")),
        (Some(Problem::Unsafe(why)), _) => Err(refuse(why)),
        (Some(Problem::Io(e)), _) => Err(AiError::new(ErrorKind::Provider, e.to_string())),
        (None, Err(_)) => Err(refuse("it is damaged")),
        (None, Ok(())) => Ok(Extracted { files, bytes }),
    }
}

/// Downloads the pinned build, checks it, unpacks it under `root` and returns the install. An
/// install already there is returned as it is: nothing is downloaded or replaced.
pub fn install_portable(
    build: &PortableBuild,
    root: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<ComfyInstall, AiError> {
    if !is_official_build(build) {
        return Err(AiError::new(
            ErrorKind::Provider,
            "This ComfyUI download is not the one offered by the app.",
        ));
    }
    if !cfg!(target_os = "windows") || !root.is_absolute() {
        return Err(AiError::new(
            ErrorKind::Provider,
            "This ComfyUI copy can only be installed into an app folder on Windows.",
        ));
    }
    let downloader = Downloader::new(DownloadConfig {
        allowed_hosts: Some(release_hosts()),
        auth_host: String::new(),
        ..DownloadConfig::default()
    });
    let final_dir = root.join(PORTABLE_DIR);
    let comfy = final_dir.join("ComfyUI");
    if let Some(existing) = ComfyInstall::at(&comfy) {
        return Ok(existing);
    }
    if final_dir.exists() {
        return Err(AiError::new(
            ErrorKind::Provider,
            "a folder is already where ComfyUI would go, and it is not left over from a finished install",
        ));
    }
    let staging = root.join(".staging");
    fs::create_dir_all(&staging).map_err(|e| AiError::new(ErrorKind::Provider, e.to_string()))?;
    let file = ModelFile {
        folder: "staging".to_owned(),
        filename: build.asset.clone(),
        size_bytes: build.size_bytes,
        sha256: build.sha256.clone(),
        source: Source::Url {
            url: build.url.clone(),
        },
        licence: Licence {
            name: "ComfyUI".to_owned(),
            url: None,
            accept_required: false,
            note: None,
        },
    };
    let archive = downloader.fetch(&file, &staging, None, cancel, progress)?;
    // `fetch` accepts an existing file of the right size. Hash it again before unpacking.
    verify_file(&archive, &file, cancel, progress)?;
    let temp = tempfile::Builder::new()
        .prefix(".unpacking-")
        .tempdir_in(root)
        .map_err(|e| AiError::new(ErrorKind::Provider, e.to_string()))?;
    let unpacking = temp.path().join("contents");
    extract_7z(
        &archive,
        &unpacking,
        &ExtractLimits::default(),
        cancel,
        &mut |_| {},
    )?;
    let inner = unpacking.join(PORTABLE_DIR);
    if !inner.join("ComfyUI").is_dir() || !inner.join("python_embeded/python.exe").is_file() {
        return Err(refuse("it does not hold a complete ComfyUI"));
    }
    fs::rename(&inner, &final_dir).map_err(|e| AiError::new(ErrorKind::Provider, e.to_string()))?;
    let _ = fs::remove_file(&archive);
    ComfyInstall::at(&comfy).ok_or_else(|| refuse("it does not hold ComfyUI"))
}

/// How to start the portable ComfyUI with no window and no browser, for one app to use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// The command that runs the portable ComfyUI under `root` on this computer only, on `port`.
/// `cpu_only` is for a computer with no graphics card it can use.
pub fn headless_launch(root: &Path, port: u16, cpu_only: bool) -> LaunchSpec {
    let base = root.join(PORTABLE_DIR);
    let mut args = vec![
        "-s".to_owned(),
        "ComfyUI/main.py".to_owned(),
        "--windows-standalone-build".to_owned(),
        "--listen".to_owned(),
        "127.0.0.1".to_owned(),
        "--port".to_owned(),
        port.to_string(),
        "--disable-auto-launch".to_owned(),
    ];
    if cpu_only {
        args.push("--cpu".to_owned());
    }
    LaunchSpec {
        program: base.join("python_embeded").join("python.exe"),
        args,
        cwd: base,
    }
}

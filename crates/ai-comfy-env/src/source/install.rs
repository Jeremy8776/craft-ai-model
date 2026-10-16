//! Installing a described file into the ComfyUI models folder.
//!
//! Everything that can be refused is refused before a byte is fetched ([`Installer::check`]),
//! each with its own [`Refusal`] so a screen can say why. After that:
//!
//! 1. The file is downloaded (or copied, for a local file) into a private staging folder, never
//!    straight into `models`. A staging folder is named from the source, so two files with the
//!    same name from different places cannot resume each other's partial download.
//! 2. With a published checksum the download must match it. Without one, the checksum of what
//!    arrived is recorded as *observed*, which is only possible because the person agreed.
//! 3. The first bytes are read. A file that is not what its name says is deleted.
//! 4. Only then is it moved into the folder, and it never replaces a file that is already there.
//! 5. The install is recorded, with the licence acceptance if one was needed.
//!
//! Nothing here starts, repairs or edits ComfyUI; it adds one file under its `models` folder.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use craft_ai_core::{AiError, ErrorKind, SecretString};
use sha2::{Digest, Sha256};

use super::candidate::{Candidate, MAX_FILE_BYTES};
use super::format::{Format, FormatProblem, SNIFF_BYTES};
use super::registry::{Checksum, Entry, LicenceAcceptance, Registry};
use super::settings::SourceSettings;
use super::spec::plain_name;
use crate::detect::ComfyInstall;
use crate::download::{Downloader, Progress};
use crate::verify::hash_file;

/// Free space required beyond the file itself.
const MARGIN_BYTES: u64 = 64 * 1024 * 1024;

/// What the person has agreed to for this install.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Consents {
    pub licence: bool,
    pub unverified: bool,
    pub pickle: bool,
}

/// Why an install did not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The folder name is not a plain folder under `models`.
    Folder,
    /// Not a file type this app installs.
    Unsupported,
    /// The file is not what its name says.
    Mislabelled {
        claims: Format,
        found: Format,
    },
    TooLarge,
    /// A pickle format, which can run code when loaded, needs consent.
    Pickle,
    Licence,
    /// The host published no checksum; installing needs consent.
    Unverified,
    /// The repository is gated; a token is needed.
    Token,
    /// A file with that name is already there and is never replaced.
    NameTaken,
    NoSpace {
        needed: u64,
        available: u64,
    },
    /// The list of installed models and accepted licences cannot be read, so nothing is
    /// installed until that is sorted out.
    Record,
}

#[derive(Debug)]
pub enum InstallError {
    Refused(Refusal),
    Failed(AiError),
}

impl From<AiError> for InstallError {
    fn from(e: AiError) -> Self {
        Self::Failed(e)
    }
}

type FreeSpace = Box<dyn Fn(&Path) -> Option<u64> + Send + Sync>;

pub struct Installer {
    pub(super) comfy: ComfyInstall,
    pub(super) settings: SourceSettings,
    pub(super) registry: Registry,
    pub(super) staging: PathBuf,
    free_space: FreeSpace,
    pub(super) pack_limits: super::pack::PackLimits,
}

impl std::fmt::Debug for Installer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Installer({})", self.comfy.base.display())
    }
}

impl Installer {
    pub fn new(
        comfy: ComfyInstall,
        settings: SourceSettings,
        registry: Registry,
        staging: PathBuf,
    ) -> Self {
        Self {
            comfy,
            settings,
            registry,
            staging,
            free_space: Box::new(crate::hardware::free_disk_bytes),
            pack_limits: super::pack::PackLimits::default(),
        }
    }

    /// Replaces how free space is read. For tests.
    pub fn with_free_space(mut self, free: FreeSpace) -> Self {
        self.free_space = free;
        self
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Every refusal that can be known before fetching anything. Returns the folder the file
    /// would land in.
    pub fn check(
        &self,
        candidate: &Candidate,
        folder: &str,
        consents: &Consents,
        has_token: bool,
    ) -> Result<PathBuf, Refusal> {
        let target_dir = self.comfy.models_dir_for(folder).ok_or(Refusal::Folder)?;
        // Better to say so now than after a download that could not be recorded.
        self.registry.list().map_err(|_| Refusal::Record)?;
        if !plain_name(&candidate.filename)
            || Format::from_name(&candidate.filename) == Format::Unknown
        {
            return Err(Refusal::Unsupported);
        }
        if candidate.size_bytes == 0 || candidate.size_bytes > MAX_FILE_BYTES {
            return Err(Refusal::TooLarge);
        }
        let needs = candidate.needs();
        if needs.pickle && !consents.pickle {
            return Err(Refusal::Pickle);
        }
        if needs.licence
            && !consents.licence
            && !self
                .registry
                .licence_accepted(candidate)
                .map_err(|_| Refusal::Record)?
        {
            return Err(Refusal::Licence);
        }
        if needs.unverified && !consents.unverified {
            return Err(Refusal::Unverified);
        }
        if needs.token && !has_token {
            return Err(Refusal::Token);
        }
        if std::fs::symlink_metadata(target_dir.join(&candidate.filename)).is_ok() {
            return Err(Refusal::NameTaken);
        }
        let needed = candidate.size_bytes.saturating_add(MARGIN_BYTES);
        let available = [&self.staging, &target_dir]
            .into_iter()
            .filter_map(|dir| (self.free_space)(nearest_existing(dir)))
            .min();
        if let Some(available) = available
            && available < needed
        {
            return Err(Refusal::NoSpace { needed, available });
        }
        Ok(target_dir)
    }

    /// Checks, fetches, verifies, places and records one file.
    pub fn install(
        &self,
        candidate: &Candidate,
        folder: &str,
        consents: &Consents,
        token: Option<&SecretString>,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<Entry, InstallError> {
        let target_dir = self
            .check(candidate, folder, consents, token.is_some())
            .map_err(InstallError::Refused)?;
        if candidate.licence.accept_required && consents.licence {
            self.registry.accept_licence(candidate, unix_seconds())?;
        }
        let stage = self.staging_dir_for(candidate);
        std::fs::create_dir_all(&stage)
            .map_err(|_| internal("could not create the staging folder"))?;
        let staged_file = stage.join(&candidate.filename);
        // Anything already under the final name here was not put there by this run.
        let _ = std::fs::remove_file(&staged_file);

        let (digest, checksum) = self.bring_in(candidate, &stage, token, cancel, progress)?;
        let format = match self.read_format(&staged_file, &candidate.filename) {
            Ok(format) => format,
            Err(refusal) => {
                let _ = std::fs::remove_file(&staged_file);
                return Err(refusal);
            }
        };
        std::fs::create_dir_all(&target_dir)
            .map_err(|_| internal("could not create the model folder"))?;
        let target = target_dir.join(&candidate.filename);
        if std::fs::symlink_metadata(&target).is_ok() {
            let _ = std::fs::remove_file(&staged_file);
            return Err(InstallError::Refused(Refusal::NameTaken));
        }
        move_into_place(&staged_file, &target)?;
        let _ = std::fs::remove_dir(&stage);

        let now = unix_seconds();
        let accepted = candidate
            .licence
            .accept_required
            .then(|| LicenceAcceptance {
                name: candidate.licence.name.clone(),
                url: candidate.licence.url.clone(),
                at: now,
            });
        let entry = Entry {
            kind: candidate.kind,
            address: candidate
                .url
                .clone()
                .or_else(|| {
                    candidate
                        .local_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                })
                .unwrap_or_default(),
            pin: candidate.pin.clone(),
            folder: folder.to_owned(),
            filename: candidate.filename.clone(),
            size_bytes: candidate.size_bytes,
            sha256: digest,
            checksum,
            format,
            licence: candidate.licence.clone(),
            licence_accepted: accepted,
            pickle_consented: format.runs_code_when_loaded(),
            installed_at: now,
        };
        self.registry.record(entry.clone()).map_err(|_| {
            internal(
                "the file was installed but could not be added to the list of installed models",
            )
        })?;
        Ok(entry)
    }

    /// Puts the bytes in `stage` and returns their checksum and how it was arrived at.
    fn bring_in(
        &self,
        candidate: &Candidate,
        stage: &Path,
        token: Option<&SecretString>,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<(String, Checksum), InstallError> {
        if let Some(source) = &candidate.local_path {
            let staged = stage.join(&candidate.filename);
            std::fs::copy(source, &staged).map_err(|_| internal("could not copy the file"))?;
            let (size, digest) = hash_file(&staged, cancel, progress).inspect_err(|_| {
                let _ = std::fs::remove_file(&staged);
            })?;
            if size != candidate.size_bytes {
                let _ = std::fs::remove_file(&staged);
                return Err(internal("the file changed while it was being copied"));
            }
            return Ok((digest, Checksum::Local));
        }
        let file = candidate
            .model_file(&candidate.folder)
            .ok_or_else(|| internal("this source has no download address"))?;
        let downloader = Downloader::new(self.settings.download_config(candidate.kind));
        if let Some(published) = &candidate.sha256 {
            downloader.fetch(&file, stage, token, cancel, progress)?;
            Ok((published.clone(), Checksum::Published))
        } else {
            let (_, observed) =
                downloader.fetch_unverified(&file, stage, token, cancel, progress)?;
            Ok((observed, Checksum::Observed))
        }
    }

    /// Reads the first bytes and checks the file is what its name says.
    fn read_format(&self, path: &Path, name: &str) -> Result<Format, InstallError> {
        let mut head = vec![0u8; SNIFF_BYTES];
        let read = std::fs::File::open(path)
            .and_then(|mut f| f.read(&mut head))
            .map_err(|_| internal("could not read the file to check it"))?;
        head.truncate(read);
        Format::check(name, &head).map_err(|problem| {
            InstallError::Refused(match problem {
                FormatProblem::UnsupportedType => Refusal::Unsupported,
                FormatProblem::Mislabelled { claims, found } => {
                    Refusal::Mislabelled { claims, found }
                }
            })
        })
    }

    /// The staging folder for this source, named from where the file comes from, so equal file
    /// names from different places never share a partial download.
    pub fn staging_dir_for(&self, candidate: &Candidate) -> PathBuf {
        let mut hasher = Sha256::new();
        for part in [
            format!("{:?}", candidate.kind),
            candidate.url.clone().unwrap_or_default(),
            candidate
                .local_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            candidate.pin.clone(),
            candidate.filename.clone(),
        ] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
        let name: String = hasher
            .finalize()
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect();
        self.staging.join(name)
    }
}

/// The deepest folder on the way to `path` that exists, so free space can be asked of a folder
/// that is about to be created.
fn nearest_existing(path: &Path) -> &Path {
    let mut current = path;
    while !current.exists() {
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    current
}

fn move_into_place(from: &Path, to: &Path) -> Result<(), InstallError> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    // Another disk: copy, then remove the staged file.
    std::fs::copy(from, to).map_err(|_| {
        let _ = std::fs::remove_file(to);
        internal("could not move the finished file into place")
    })?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

fn internal(why: &str) -> InstallError {
    InstallError::Failed(AiError::new(ErrorKind::Internal, why))
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

//! What has been installed from user-chosen sources, and which licences were accepted.
//!
//! One small JSON file the caller places (the launcher keeps it beside its other AI settings).
//! It holds file names, sizes, checksums, addresses and licence acceptances, and never a token.
//! It is written to a temporary file and renamed into place, so a crash leaves the old one.
//!
//! Licences the person accepted are kept in the same file but apart from the installed files
//! (`licences.rs`): an acceptance holds for the same kind of source, the same pinned version and
//! the same licence name and link, and a new version or changed terms are asked about again.

mod licences;

use std::path::PathBuf;

use craft_ai_core::{AiError, ErrorKind};
use craft_ai_features::Licence;
use serde::{Deserialize, Serialize};

use super::candidate::Candidate;
use super::format::Format;
use super::hosts::SourceKind;
use super::pack::PackEntry;

pub use licences::LicenceRecord;

const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 5000;

/// How the file's checksum was arrived at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Checksum {
    /// The host published it and the download matched.
    Published,
    /// The host published none; this is what arrived. Nobody vouched for it.
    Observed,
    /// A file from this computer, copied; this is its checksum.
    Local,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenceAcceptance {
    pub name: String,
    pub url: Option<String>,
    /// Seconds since 1970 (UTC).
    pub at: u64,
}

/// One installed file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub kind: SourceKind,
    /// Where it came from: the pinned address, or the path of the local file.
    pub address: String,
    pub pin: String,
    pub folder: String,
    pub filename: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub checksum: Checksum,
    pub format: Format,
    pub licence: Licence,
    pub licence_accepted: Option<LicenceAcceptance>,
    pub pickle_consented: bool,
    pub installed_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Document {
    version: u32,
    entries: Vec<Entry>,
    #[serde(default)]
    packs: Vec<PackEntry>,
    #[serde(default)]
    licences: Vec<LicenceRecord>,
}

#[derive(Clone, Debug)]
pub struct Registry {
    path: PathBuf,
}

impl Registry {
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Every record; empty when nothing was ever installed. A damaged or oversized file is an
    /// error, never silently emptied.
    pub fn list(&self) -> Result<Vec<Entry>, AiError> {
        Ok(self.read()?.entries)
    }

    /// Every installed node pack.
    pub fn packs(&self) -> Result<Vec<PackEntry>, AiError> {
        Ok(self.read()?.packs)
    }

    /// Adds or replaces the record for a node pack folder.
    pub fn record_pack(&self, pack: PackEntry) -> Result<(), AiError> {
        let mut document = self.read()?;
        document.packs.retain(|p| p.folder != pack.folder);
        document.packs.push(pack);
        if document.packs.len() > MAX_ENTRIES {
            return Err(AiError::new(
                ErrorKind::Internal,
                "the list of installed packs is full",
            ));
        }
        self.write(document)
    }

    /// Forgets a node pack record. The folder is not touched here.
    pub fn remove_pack(&self, folder: &str) -> Result<bool, AiError> {
        let mut document = self.read()?;
        let before = document.packs.len();
        document.packs.retain(|p| p.folder != folder);
        if document.packs.len() == before {
            return Ok(false);
        }
        self.write(document)?;
        Ok(true)
    }

    fn read(&self) -> Result<Document, AiError> {
        let meta = match std::fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Document {
                    version: 1,
                    ..Document::default()
                });
            }
            Err(_) => return Err(unreadable()),
        };
        if meta.len() > MAX_FILE_BYTES {
            return Err(unreadable());
        }
        let text = std::fs::read_to_string(&self.path).map_err(|_| unreadable())?;
        serde_json::from_str(&text).map_err(|_| unreadable())
    }

    /// Adds or replaces the record for this folder and file name.
    pub fn record(&self, entry: Entry) -> Result<(), AiError> {
        let mut document = self.read()?;
        document
            .entries
            .retain(|e| !(e.folder == entry.folder && e.filename == entry.filename));
        document.entries.push(entry);
        if document.entries.len() > MAX_ENTRIES {
            return Err(AiError::new(
                ErrorKind::Internal,
                "the list of installed models is full",
            ));
        }
        self.write(document)
    }

    /// Forgets a record. The file stays where it is. `false` when there was none.
    pub fn remove(&self, folder: &str, filename: &str) -> Result<bool, AiError> {
        let mut document = self.read()?;
        let before = document.entries.len();
        document
            .entries
            .retain(|e| !(e.folder == folder && e.filename == filename));
        if document.entries.len() == before {
            return Ok(false);
        }
        self.write(document)?;
        Ok(true)
    }

    fn write(&self, mut document: Document) -> Result<(), AiError> {
        document.version = 1;
        let failed = || {
            AiError::new(
                ErrorKind::Internal,
                "could not save the list of installed models",
            )
        };
        let text = serde_json::to_string_pretty(&document).map_err(|_| failed())?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| failed())?;
        }
        let mut temporary = self.path.clone().into_os_string();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        std::fs::write(&temporary, text).map_err(|_| failed())?;
        std::fs::rename(&temporary, &self.path).map_err(|_| {
            let _ = std::fs::remove_file(&temporary);
            failed()
        })
    }
}

fn unreadable() -> AiError {
    AiError::new(
        ErrorKind::Internal,
        "the list of installed models could not be read",
    )
}

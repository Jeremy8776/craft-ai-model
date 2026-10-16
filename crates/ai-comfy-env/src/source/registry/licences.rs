//! The record of licences a person accepted.
//!
//! Kept beside the list of installed files but not tied to it: an acceptance is a fact about a
//! person and some terms, so it does not vanish when a file is removed, and it is not made up
//! when none was needed. A record names the kind of source, the pinned version, the licence name
//! and link, and when the person said yes. For a file on this computer, which has no version, it
//! also names the file: each one is its own question.
//!
//! Nothing secret is stored here. A damaged file is an error, never read as "accepted" or as
//! "nothing accepted".

use craft_ai_core::{AiError, ErrorKind};
use serde::{Deserialize, Serialize};

use super::{Candidate, Entry, MAX_ENTRIES, Registry, SourceKind};

/// One licence a person accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenceRecord {
    pub kind: SourceKind,
    /// The version the person accepted the terms for.
    pub pin: String,
    /// Set for a file from this computer only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    pub name: String,
    pub url: Option<String>,
    /// Seconds since 1970 (UTC).
    pub at: u64,
}

impl LicenceRecord {
    fn covers(&self, c: &Candidate) -> bool {
        self.kind == c.kind
            && self.pin == c.pin
            && self.name == c.licence.name
            && self.url == c.licence.url
            && (c.kind != SourceKind::Local || self.filename.as_deref() == Some(&c.filename))
    }
}

impl Registry {
    /// Notes that the person accepted this candidate's licence at `at`. Does nothing when the
    /// licence needs no acceptance, or when the same terms for the same version are already on
    /// record (the first time stays).
    pub fn accept_licence(&self, candidate: &Candidate, at: u64) -> Result<(), AiError> {
        if !candidate.licence.accept_required {
            return Ok(());
        }
        let mut document = self.read()?;
        if document.licences.iter().any(|r| r.covers(candidate)) {
            return Ok(());
        }
        if document.licences.len() >= MAX_ENTRIES {
            return Err(AiError::new(
                ErrorKind::Internal,
                "the list of accepted licences is full",
            ));
        }
        document.licences.push(LicenceRecord {
            kind: candidate.kind,
            pin: candidate.pin.clone(),
            filename: (candidate.kind == SourceKind::Local).then(|| candidate.filename.clone()),
            name: candidate.licence.name.clone(),
            url: candidate.licence.url.clone(),
            at,
        });
        self.write(document)
    }

    /// Whether this exact version under these exact terms was accepted before. A changed name or
    /// link, a new version, another kind of source and (for a local file) another file all say no.
    ///
    /// Installs made before the record existed carry their acceptance on the installed entry,
    /// which still counts.
    pub fn licence_accepted(&self, candidate: &Candidate) -> Result<bool, AiError> {
        let document = self.read()?;
        Ok(document.licences.iter().any(|r| r.covers(candidate))
            || document
                .entries
                .iter()
                .any(|e| e.licence_accepted.is_some() && entry_covers(e, candidate)))
    }

    /// Every licence on record, oldest first.
    pub fn licences(&self) -> Result<Vec<LicenceRecord>, AiError> {
        Ok(self.read()?.licences)
    }

    /// Takes back an acceptance, so the person is asked again. Returns whether there was one.
    pub fn forget_licence(&self, candidate: &Candidate) -> Result<bool, AiError> {
        let mut document = self.read()?;
        let before = document.licences.len();
        document.licences.retain(|r| !r.covers(candidate));
        let mut changed = document.licences.len() != before;
        for e in &mut document.entries {
            if entry_covers(e, candidate) && e.licence_accepted.take().is_some() {
                changed = true;
            }
        }
        if changed {
            self.write(document)?;
        }
        Ok(changed)
    }
}

/// Whether an installed entry was installed under the terms this candidate carries.
fn entry_covers(e: &Entry, c: &Candidate) -> bool {
    e.kind == c.kind
        && e.pin == c.pin
        && e.licence.name == c.licence.name
        && e.licence.url == c.licence.url
        && (c.kind != SourceKind::Local || e.filename == c.filename)
}

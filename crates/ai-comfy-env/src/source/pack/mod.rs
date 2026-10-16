//! Node packs from a GitHub address (task u4).
//!
//! A node pack is Python that ComfyUI imports and runs with the person's own access, so this
//! module is built around one rule: **nothing runs, ever, during an install.** What it does:
//!
//! - [`parse_pack`] understands `https://github.com/owner/repo` (optionally `/tree/<ref>`).
//! - [`Resolver::resolve_pack`](super::Resolver::resolve_pack) pins the address to one commit and
//!   describes it for the person: the commit, its first line, its date, the licence, and whether
//!   the repository is archived. Only public repositories.
//! - [`Installer::install_pack`](super::Installer::install_pack) needs a [`PackConsent`] naming
//!   that exact commit, downloads the archive of that commit, unpacks it with strict rules
//!   (`archive.rs`) into a staging folder, and only then moves it into `custom_nodes`. It never
//!   replaces a folder that exists, never runs `pip`, `install.py` or any script (it only reports
//!   that a `requirements.txt` or `install.py` is present), and records the commit it installed.
//! - [`Installer::remove_pack`](super::Installer::remove_pack) removes a pack this app installed,
//!   and refuses a folder it did not.

mod archive;
mod install;
mod resolve;

use craft_ai_core::AiError;
use serde::{Deserialize, Serialize};
use url::Url;

use super::spec::{SpecError, revision_ok, segments};

pub(super) use archive::unpack;

/// What a pasted GitHub address names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackSpec {
    pub owner: String,
    pub repo: String,
    /// A branch or tag; the repository's default branch when absent.
    pub reference: Option<String>,
}

fn simple_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 100
        && !text.starts_with('.')
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Understands one pasted repository address. Plain https `github.com` only.
pub fn parse_pack(input: &str) -> Result<PackSpec, SpecError> {
    let url = Url::parse(input.trim()).map_err(|_| SpecError::NotSupported)?;
    match url.scheme() {
        "https" => {}
        "http" => return Err(SpecError::Insecure),
        _ => return Err(SpecError::NotSupported),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SpecError::Insecure);
    }
    if url.host_str() != Some("github.com") || url.port().is_some() {
        return Err(SpecError::NotSupported);
    }
    let segments = segments(&url)?;
    let parts: Vec<&str> = segments.iter().map(String::as_str).collect();
    let (owner, repo, reference) = match parts.as_slice() {
        [owner, repo] => (*owner, *repo, None),
        [owner, repo, "tree", reference] => (*owner, *repo, Some(*reference)),
        _ => return Err(SpecError::NotSupported),
    };
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    if !simple_name(owner) || !simple_name(repo) {
        return Err(SpecError::BadName);
    }
    if reference.is_some_and(|r| !revision_ok(r)) {
        return Err(SpecError::BadName);
    }
    Ok(PackSpec {
        owner: owner.to_owned(),
        repo: repo.to_owned(),
        reference: reference.map(str::to_owned),
    })
}

/// A pack pinned to a commit, with what the person is shown before agreeing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackCandidate {
    pub owner: String,
    pub repo: String,
    /// The branch or tag that was resolved.
    pub reference: String,
    /// The full commit the install will fetch. It cannot move once shown.
    pub commit: String,
    /// The first line of the commit message, one short clean line.
    pub subject: String,
    pub committed_at: String,
    pub licence: String,
    pub archived: bool,
    /// The folder under `custom_nodes` it would be installed as.
    pub folder: String,
    pub archive_url: String,
}

/// The person's agreement to run this pack's code, for one commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackConsent {
    pub commit: String,
}

/// An installed pack, as recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackEntry {
    pub owner: String,
    pub repo: String,
    pub reference: String,
    pub commit: String,
    pub folder: String,
    pub files: u32,
    pub bytes: u64,
    /// Checksum of the archive as received. GitHub does not publish one, so it is observed.
    pub archive_sha256: String,
    /// The pack ships a `requirements.txt`. Nothing was installed from it.
    pub has_requirements: bool,
    /// The pack ships `install.py` or `setup.py`. Nothing was run.
    pub has_install_script: bool,
    pub licence: String,
    pub installed_at: u64,
}

/// How much a pack archive may be.
#[derive(Clone, Debug)]
pub struct PackLimits {
    pub max_download: u64,
    pub max_expanded: u64,
    pub max_file: u64,
    pub max_entries: u32,
    pub max_depth: usize,
}

impl Default for PackLimits {
    fn default() -> Self {
        Self {
            max_download: 150 * 1024 * 1024,
            max_expanded: 500 * 1024 * 1024,
            max_file: 200 * 1024 * 1024,
            max_entries: 20_000,
            max_depth: 24,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackRefusal {
    /// No consent, or consent for a different commit.
    NeedsConsent,
    /// A folder with that name exists in `custom_nodes`; it is never replaced.
    FolderTaken,
    /// The folder was not installed by this app.
    NotRecorded,
    TooLarge,
    /// The archive or the request is not safe to unpack; the text says what.
    Unsafe(&'static str),
}

#[derive(Debug)]
pub enum PackError {
    Refused(PackRefusal),
    Failed(AiError),
}

impl From<AiError> for PackError {
    fn from(e: AiError) -> Self {
        Self::Failed(e)
    }
}

pub(super) fn unsafe_archive(why: &'static str) -> PackError {
    PackError::Refused(PackRefusal::Unsafe(why))
}

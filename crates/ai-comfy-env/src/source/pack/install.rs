//! Installing and removing a node pack. See the module docs of `pack` for the rules.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use craft_ai_core::{AiError, ErrorKind};
use sha2::{Digest, Sha256};

use super::{
    PackCandidate, PackConsent, PackEntry, PackError, PackLimits, PackRefusal, unpack,
    unsafe_archive,
};
use crate::download::Downloader;
use crate::source::hosts::SourceKind;
use crate::source::install::Installer;
use crate::source::spec::plain_name;

impl Installer {
    pub fn set_pack_limits(&mut self, limits: PackLimits) {
        self.pack_limits = limits;
    }

    /// Downloads, unpacks and records the pack at the commit the person agreed to. Runs nothing.
    pub fn install_pack(
        &self,
        candidate: &PackCandidate,
        consent: Option<&PackConsent>,
        cancel: &AtomicBool,
    ) -> Result<PackEntry, PackError> {
        if !consent.is_some_and(|c| c.commit == candidate.commit) {
            return Err(PackError::Refused(PackRefusal::NeedsConsent));
        }
        if !plain_name(&candidate.folder) {
            return Err(unsafe_archive("the folder name is not a plain name"));
        }
        let target = self.comfy.custom_nodes.join(&candidate.folder);
        if std::fs::symlink_metadata(&target).is_ok() {
            return Err(PackError::Refused(PackRefusal::FolderTaken));
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(PackError::Failed(AiError::new(
                ErrorKind::Cancelled,
                "install cancelled",
            )));
        }
        let downloader = Downloader::new(self.settings.download_config(SourceKind::NodePack));
        let archive =
            downloader.get_bytes(&candidate.archive_url, self.pack_limits.max_download, None)?;
        let digest: String = Sha256::digest(&archive)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();

        let stage = self.pack_stage(candidate);
        let _ = std::fs::remove_dir_all(&stage);
        std::fs::create_dir_all(&stage)
            .map_err(|_| internal("could not create the staging folder"))?;
        let unpacked = match unpack(&archive, &self.pack_limits, &stage, &candidate.folder) {
            Ok(unpacked) => unpacked,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&stage);
                return Err(e);
            }
        };
        if let Err(e) = self.place_pack(&stage.join(&candidate.folder), &target) {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(e);
        }
        let _ = std::fs::remove_dir_all(&stage);

        let entry = PackEntry {
            owner: candidate.owner.clone(),
            repo: candidate.repo.clone(),
            reference: candidate.reference.clone(),
            commit: candidate.commit.clone(),
            folder: candidate.folder.clone(),
            files: unpacked.files,
            bytes: unpacked.bytes,
            archive_sha256: digest,
            has_requirements: unpacked.top_files.iter().any(|n| n == "requirements.txt"),
            has_install_script: unpacked
                .top_files
                .iter()
                .any(|n| n == "install.py" || n == "setup.py"),
            licence: candidate.licence.clone(),
            installed_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        };
        self.registry.record_pack(entry.clone()).map_err(|_| {
            internal("the pack was installed but could not be added to the list of installed packs")
        })?;
        Ok(entry)
    }

    /// Removes a pack this app installed, and its record. Returns how many files it held.
    /// A folder that is not on the list, or is a link, is refused and left alone.
    pub fn remove_pack(&self, folder: &str) -> Result<u32, PackError> {
        if !plain_name(folder) {
            return Err(unsafe_archive("the folder name is not a plain name"));
        }
        if !self.registry.packs()?.iter().any(|p| p.folder == folder) {
            return Err(PackError::Refused(PackRefusal::NotRecorded));
        }
        let path = self.comfy.custom_nodes.join(folder);
        let mut files = 0;
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(unsafe_archive("that folder is a link"));
            }
            Ok(meta) if meta.is_dir() => {
                files = count_files(&path);
                std::fs::remove_dir_all(&path)
                    .map_err(|_| internal("could not remove the folder"))?;
            }
            _ => {}
        }
        self.registry.remove_pack(folder)?;
        Ok(files)
    }

    fn pack_stage(&self, candidate: &PackCandidate) -> std::path::PathBuf {
        let mut hasher = Sha256::new();
        for part in [&candidate.owner, &candidate.repo, &candidate.commit] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
        let name: String = hasher
            .finalize()
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect();
        self.staging.join(format!("pack-{name}"))
    }

    fn place_pack(&self, from: &Path, to: &Path) -> Result<(), PackError> {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|_| internal("could not create custom_nodes"))?;
        }
        if std::fs::symlink_metadata(to).is_ok() {
            return Err(PackError::Refused(PackRefusal::FolderTaken));
        }
        if std::fs::rename(from, to).is_ok() {
            return Ok(());
        }
        // Another disk: copy the checked tree, and undo it if the copy fails.
        copy_tree(from, to).map_err(|_| {
            let _ = std::fs::remove_dir_all(to);
            internal("could not move the pack into place")
        })
    }
}

fn internal(why: &str) -> PackError {
    PackError::Failed(AiError::new(ErrorKind::Internal, why))
}

fn count_files(dir: &Path) -> u32 {
    std::fs::read_dir(dir).map_or(0, |read| {
        read.flatten()
            .map(|e| match e.file_type() {
                Ok(t) if t.is_dir() => count_files(&e.path()),
                Ok(t) if t.is_file() => 1,
                _ => 0,
            })
            .sum()
    })
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            let mut source = std::fs::File::open(entry.path())?;
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            std::io::copy(&mut source.by_ref(), &mut out)?;
        }
    }
    Ok(())
}

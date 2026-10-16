//! Checks a model file against the size and SHA-256 its recipe promises.

use craft_ai_core::{AiError, ErrorKind};
use craft_ai_features::ModelFile;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::download::{Phase, Progress};

/// Hashes `path` and compares it with the recipe. Reports progress as `Verifying`. Fails with
/// `Cancelled` when `cancel` is set, and with `Provider` when the size or hash differs.
pub fn verify_file(
    path: &Path,
    file: &ModelFile,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<(), AiError> {
    // A file of the wrong size is refused without being read.
    let size = std::fs::metadata(path)
        .map(|m| m.len())
        .map_err(unreadable)?;
    if size != file.size_bytes {
        return Err(mismatch());
    }
    let (_, digest) = hash_file(path, cancel, progress)?;
    if digest == file.sha256.to_ascii_lowercase() {
        Ok(())
    } else {
        Err(mismatch())
    }
}

fn unreadable<E>(_: E) -> AiError {
    AiError::new(
        ErrorKind::Internal,
        "could not read the model file to check it",
    )
}

/// The size and lower-case hex SHA-256 of the file at `path`, read once. Reports progress as
/// `Verifying`; fails with `Cancelled` when `cancel` is set.
pub fn hash_file(
    path: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<(u64, String), AiError> {
    let mut input = std::fs::File::open(path).map_err(unreadable)?;
    let size = input.metadata().map_err(unreadable)?.len();
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut done = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(AiError::new(ErrorKind::Cancelled, "check cancelled"));
        }
        let n = input.read(&mut buffer).map_err(unreadable)?;
        if n == 0 {
            break;
        }
        hasher.update(buffer.get(..n).unwrap_or_default());
        done += n as u64;
        progress(Progress {
            phase: Phase::Verifying,
            done,
            total: size,
        });
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((size, digest))
}

fn mismatch() -> AiError {
    AiError::new(
        ErrorKind::Provider,
        "the model file does not match the size and checksum it should have",
    )
}

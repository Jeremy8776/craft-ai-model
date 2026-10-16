//! Exclusive, bounded ownership of a slot in the server's SAM 3 hover cache.
//!
//! The installed interactive node keeps one cached image per graph node id in a plain dictionary:
//! no eviction, no owner check, and the hover route trusts whatever id it is given. A fixed id
//! shared by every session lets one image silently replace another's; a fresh id per session
//! never frees server memory. So each prepared image holds one of a few fixed ids ("slots"),
//! guarded by an operating-system file lock. The lock works across processes (several Craft
//! apps share one ComfyUI) and is released if the holder crashes, so a dead app cannot leak a
//! slot. Nothing here can free the server's cached tensors; the bound on slots is the bound on
//! what this client can leave behind there.
use craft_ai_core::{AiError, ErrorKind};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

/// Prepared images one server will hold for Craft clients at once.
pub(crate) const SLOTS: usize = 4;

/// Holding the open, locked file is holding the slot; dropping it releases the slot.
pub(crate) struct SlotLease {
    _file: File,
    slot: usize,
}

impl SlotLease {
    /// Lock the lowest free slot for `server` (a short name unique per server address).
    pub(crate) fn acquire(dir: &Path, server: &str) -> Result<Self, AiError> {
        std::fs::create_dir_all(dir)
            .map_err(|_| failed("could not create the SAM 3 lock folder"))?;
        for slot in 0..SLOTS {
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(dir.join(format!("slot-{server}-{slot}.lock")))
                .map_err(|_| failed("could not open a SAM 3 lock file"))?;
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file, slot }),
                Err(TryLockError::WouldBlock) => continue,
                Err(TryLockError::Error(_)) => {
                    return Err(failed("could not take a SAM 3 lock"));
                }
            }
        }
        Err(AiError::new(
            ErrorKind::RateLimited,
            "All SAM 3 image slots on this server are in use by other Craft apps or documents. \
             Close Object Selection in one of them and try again",
        ))
    }

    pub(crate) fn slot(&self) -> usize {
        self.slot
    }

    /// The graph node id that names this slot on the server.
    pub(crate) fn node_id(&self) -> String {
        format!("craft_ai_sam3_{}", self.slot)
    }
}

fn failed(message: &str) -> AiError {
    AiError::new(ErrorKind::Internal, message)
}

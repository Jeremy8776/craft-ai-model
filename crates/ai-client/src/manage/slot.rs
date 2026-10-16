//! Names for key slots: unique without asking the file, so a key can be written before the lock.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use craft_ai_core::{AiError, ConnectionId, SecretRef};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// `<connection id>.key.<time>-<process>-<count>`: still recognisable in a keychain listing, and
/// different for every call, across processes (time and process id) and within one (counter).
pub(super) fn fresh(id: &ConnectionId) -> Result<SecretRef, AiError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let name: String = id.as_str().chars().take(60).collect();
    SecretRef::new(format!(
        "{name}.key.{millis:x}-{:x}-{n:x}",
        std::process::id()
    ))
    .map_err(|_| AiError::invalid("That name cannot be used for the saved key."))
}

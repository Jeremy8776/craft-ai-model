//! Secret stores: where an API key lives between the moment a user types it and the moment a
//! provider needs it.
//!
//! The data model keeps only a [`SecretRef`] (a locator) in the shared store file. A
//! [`SecretStore`] turns that locator into a [`SecretString`]. Stores here:
//!
//! | Store | Backing | Writable |
//! |---|---|---|
//! | [`KeychainStore`] | Windows Credential Manager, macOS Keychain, Secret Service | yes |
//! | [`EncryptedFileStore`] | one file, Argon2id key from a passphrase, XChaCha20-Poly1305 | yes |
//! | [`EnvStore`] | `CRAFT_AI_SECRET_<REF>` variables, for headless runs | no |
//! | [`MemoryStore`] | a map, for tests and for apps' own test suites | yes |
//! | [`LayeredStore`] | an ordered chain of the above | yes |
//!
//! Errors never contain a secret value or a passphrase.

mod crypto;
mod env;
mod file;
mod keychain;
mod layered;
mod memory;

use craft_ai_core::{AiError, Auth, Connection, ErrorKind, SecretRef, SecretString};

pub use crypto::KdfParams;
pub use env::EnvStore;
pub use file::EncryptedFileStore;
pub use keychain::KeychainStore;
pub use layered::LayeredStore;
pub use memory::MemoryStore;

/// A place that can hold secrets by reference.
pub trait SecretStore: Send + Sync {
    /// Short name for messages and diagnostics, never a secret.
    fn name(&self) -> &'static str;

    /// The secret for `key`, or `Ok(None)` if this store has none.
    fn get(&self, key: &SecretRef) -> Result<Option<SecretString>, AiError>;

    /// Saves `value` under `key`, replacing any earlier value.
    ///
    /// Read-only stores return an [`ErrorKind::Unsupported`] error.
    fn set(&self, key: &SecretRef, value: &SecretString) -> Result<(), AiError>;

    /// Removes `key`. Removing a key that is not there is not an error.
    fn delete(&self, key: &SecretRef) -> Result<(), AiError>;
}

/// The error a read-only store gives for `set` and `delete`.
pub(crate) fn read_only(store: &str) -> AiError {
    AiError::unsupported(format!("the {store} secret store is read-only"))
}

/// A storage fault (disk, keychain service, corrupt file). Never carries a secret.
pub(crate) fn storage_error(store: &str, what: &str) -> AiError {
    AiError::new(ErrorKind::Internal, format!("{store} secret store: {what}"))
}

/// The secret a connection needs, or `None` for a connection with no credentials.
///
/// A connection that needs a secret the store does not hold is an [`ErrorKind::Auth`] error that
/// says which connection to fix, so an app can send the user to the launcher.
pub fn secret_for(
    store: &dyn SecretStore,
    connection: &Connection,
) -> Result<Option<SecretString>, AiError> {
    let Some(key) = connection.auth.secret_ref() else {
        return Ok(None);
    };
    match store.get(key)? {
        Some(secret) if !secret.is_empty() => Ok(Some(secret)),
        _ => {
            let route = match &connection.auth {
                Auth::Subscription { .. } => "sign-in",
                _ => "key",
            };
            Err(AiError::new(
                ErrorKind::Auth,
                format!(
                    "no {route} is saved for connection \"{}\"; add it in the ArtCraft Launcher",
                    connection.label
                ),
            ))
        }
    }
}

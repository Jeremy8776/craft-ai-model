//! OS keychain store: Windows Credential Manager, macOS Keychain, or a Secret Service on Linux.
//!
//! Each secret is one entry: service `craft-ai` (changeable for tests), user name the
//! [`SecretRef`]. Where no keychain exists (a headless Linux box with no Secret Service), every
//! call returns [`ErrorKind::Unsupported`] so a [`LayeredStore`](super::LayeredStore) moves on to
//! the next layer.

use craft_ai_core::{AiError, ErrorKind, SecretRef, SecretString};
use keyring::Error as KeyError;

use super::{SecretStore, storage_error};

/// The service name entries are filed under.
pub const DEFAULT_SERVICE: &str = "craft-ai";

/// Secrets held by the operating system.
#[derive(Clone, Debug)]
pub struct KeychainStore {
    service: String,
}

impl KeychainStore {
    pub fn new() -> Self {
        Self::with_service(DEFAULT_SERVICE)
    }

    /// Files entries under a different service name, so tests never touch real keys.
    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// Whether this machine has a usable keychain.
    pub fn is_available() -> bool {
        keyring::Entry::store_status().is_ok()
    }

    fn entry(&self, key: &SecretRef) -> Result<keyring::Entry, AiError> {
        if !Self::is_available() {
            return Err(AiError::unsupported("no OS keychain is available"));
        }
        keyring::Entry::new(&self.service, key.as_str()).map_err(map_error)
    }
}

impl Default for KeychainStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Maps a keychain error to ours. Only variants that cannot carry secret bytes are described.
fn map_error(e: KeyError) -> AiError {
    match e {
        KeyError::NoDefaultStore | KeyError::NotSupportedByStore(_) => {
            AiError::unsupported("no OS keychain is available")
        }
        KeyError::NoStorageAccess(err) => AiError::new(
            ErrorKind::Auth,
            format!("the OS keychain refused access: {err}"),
        ),
        KeyError::PlatformFailure(err) => storage_error("keychain", &format!("failed: {err}")),
        KeyError::TooLong(..) => {
            storage_error("keychain", "the value is too long for the keychain")
        }
        KeyError::Ambiguous(_) => storage_error("keychain", "more than one entry matches"),
        _ => storage_error("keychain", "stored data could not be read"),
    }
}

impl SecretStore for KeychainStore {
    fn name(&self) -> &'static str {
        "keychain"
    }

    fn get(&self, key: &SecretRef) -> Result<Option<SecretString>, AiError> {
        match self.entry(key)?.get_password() {
            Ok(value) => Ok(Some(SecretString::new(value))),
            Err(KeyError::NoEntry) => Ok(None),
            Err(e) => Err(map_error(e)),
        }
    }

    fn set(&self, key: &SecretRef, value: &SecretString) -> Result<(), AiError> {
        self.entry(key)?
            .set_password(value.expose())
            .map_err(map_error)
    }

    fn delete(&self, key: &SecretRef) -> Result<(), AiError> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(KeyError::NoEntry) => Ok(()),
            Err(e) => Err(map_error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_that_could_hold_secret_bytes_are_not_described() {
        let e = map_error(KeyError::BadEncoding(b"sk-live-123".to_vec()));
        assert!(!e.message.contains("sk-live"));
        assert!(!format!("{e:?}").contains("sk-live"));
    }

    #[test]
    fn missing_store_is_unsupported_so_layers_move_on() {
        assert_eq!(
            map_error(KeyError::NoDefaultStore).kind,
            ErrorKind::Unsupported
        );
    }

    /// Talks to the real OS keychain, so it is opt-in:
    /// `cargo test -p craft-ai-native -- --ignored keychain_round_trip`
    #[test]
    #[ignore = "uses the real OS keychain"]
    fn keychain_round_trip() {
        if !KeychainStore::is_available() {
            return;
        }
        let store = KeychainStore::with_service("craft-ai-test");
        let key = SecretRef::new("round-trip").unwrap();
        store.delete(&key).unwrap();
        assert!(store.get(&key).unwrap().is_none());
        store.set(&key, &SecretString::new("first")).unwrap();
        store.set(&key, &SecretString::new("second")).unwrap();
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "second");
        store.delete(&key).unwrap();
        assert!(store.get(&key).unwrap().is_none());
    }
}

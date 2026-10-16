//! Passphrase-encrypted file store: the fallback where no OS keychain exists.
//!
//! The whole map of secrets is one encrypted blob ([`super::crypto`]). Every write decrypts,
//! changes, re-encrypts with a fresh salt and nonce, and replaces the file atomically under a
//! lock. A wrong passphrase on a write is an error and leaves the file untouched.
//!
//! Where the passphrase comes from is the caller's decision (the launcher prompts for it). This
//! crate never stores it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use craft_ai_core::{AiError, ErrorKind, SecretRef, SecretString};
use serde::{Deserialize, Serialize};

use super::crypto::{self, CryptoError, Envelope, KdfParams};
use super::{SecretStore, storage_error};
use crate::fsutil;

/// Largest secret file accepted, in bytes.
const MAX_FILE_BYTES: usize = 1024 * 1024;

#[derive(Default, Serialize, Deserialize)]
struct Plain {
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

/// Secrets in one passphrase-encrypted file.
pub struct EncryptedFileStore {
    path: PathBuf,
    passphrase: SecretString,
    params: KdfParams,
}

impl EncryptedFileStore {
    /// A store at `path`, locked with `passphrase`, using the default cost settings.
    pub fn new(path: impl Into<PathBuf>, passphrase: SecretString) -> Self {
        Self::with_params(path, passphrase, KdfParams::default())
    }

    /// As [`new`](Self::new) with explicit cost settings (small ones for tests).
    pub fn with_params(
        path: impl Into<PathBuf>,
        passphrase: SecretString,
        params: KdfParams,
    ) -> Self {
        Self {
            path: path.into(),
            passphrase,
            params,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn check_passphrase(&self) -> Result<(), AiError> {
        if self.passphrase.is_empty() {
            return Err(AiError::invalid("the secret file needs a passphrase"));
        }
        Ok(())
    }

    /// Reads and decrypts the file. A missing file is an empty map.
    fn load(&self) -> Result<Plain, AiError> {
        let Some(bytes) = fsutil::read_capped(&self.path, MAX_FILE_BYTES)
            .map_err(|_| storage_error("encrypted file", "the file could not be read"))?
        else {
            return Ok(Plain::default());
        };
        let env: Envelope = serde_json::from_slice(&bytes)
            .map_err(|_| storage_error("encrypted file", "the file is not a Craft secret file"))?;
        let plain = crypto::open(&self.passphrase, &env).map_err(map_crypto)?;
        serde_json::from_slice(&plain)
            .map_err(|_| storage_error("encrypted file", "the decrypted contents are invalid"))
    }

    fn save(&self, plain: &Plain) -> Result<(), AiError> {
        let json = serde_json::to_vec(plain)
            .map_err(|_| storage_error("encrypted file", "could not encode secrets"))?;
        let env = crypto::seal(&self.passphrase, self.params, &json).map_err(map_crypto)?;
        let out = serde_json::to_vec_pretty(&env)
            .map_err(|_| storage_error("encrypted file", "could not encode the file"))?;
        fsutil::atomic_write(&self.path, &out)
            .map_err(|_| storage_error("encrypted file", "the file could not be written"))
    }

    /// Read, change, write, all under the lock.
    fn modify(&self, f: impl FnOnce(&mut Plain)) -> Result<(), AiError> {
        self.check_passphrase()?;
        let outcome = fsutil::with_lock(&self.path, || {
            Ok(self.load().and_then(|mut p| {
                f(&mut p);
                self.save(&p)
            }))
        });
        match outcome {
            Ok(inner) => inner,
            Err(_) => Err(storage_error(
                "encrypted file",
                "could not take the write lock",
            )),
        }
    }
}

fn map_crypto(e: CryptoError) -> AiError {
    match e {
        CryptoError::WrongKeyOrDamaged => AiError::new(
            ErrorKind::Auth,
            "wrong passphrase, or the secret file is damaged",
        ),
        CryptoError::TooNew(v) => storage_error(
            "encrypted file",
            &format!("the file is version {v}, newer than this app understands; update the app"),
        ),
        CryptoError::BadFormat => {
            storage_error("encrypted file", "the file is not a Craft secret file")
        }
        CryptoError::Failed => storage_error("encrypted file", "encryption failed"),
    }
}

impl std::fmt::Debug for EncryptedFileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptedFileStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl SecretStore for EncryptedFileStore {
    fn name(&self) -> &'static str {
        "encrypted file"
    }

    fn get(&self, key: &SecretRef) -> Result<Option<SecretString>, AiError> {
        self.check_passphrase()?;
        Ok(self
            .load()?
            .secrets
            .get(key.as_str())
            .map(SecretString::new))
    }

    fn set(&self, key: &SecretRef, value: &SecretString) -> Result<(), AiError> {
        self.modify(|p| {
            p.secrets
                .insert(key.as_str().to_owned(), value.expose().to_owned());
        })
    }

    fn delete(&self, key: &SecretRef) -> Result<(), AiError> {
        if !self.path.exists() {
            return Ok(());
        }
        self.modify(|p| {
            p.secrets.remove(key.as_str());
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> SecretRef {
        SecretRef::new(s).unwrap()
    }

    fn store(dir: &Path, pass: &str) -> EncryptedFileStore {
        EncryptedFileStore::with_params(
            dir.join("secrets.enc.json"),
            SecretString::new(pass),
            KdfParams::new(64, 1, 1).unwrap(),
        )
    }

    #[test]
    fn set_get_delete_and_persist_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), "pw");
        assert!(s.get(&key("a")).unwrap().is_none());
        s.set(&key("a"), &SecretString::new("one")).unwrap();
        s.set(&key("b"), &SecretString::new("two")).unwrap();
        s.set(&key("a"), &SecretString::new("uno")).unwrap();
        let again = store(dir.path(), "pw");
        assert_eq!(again.get(&key("a")).unwrap().unwrap().expose(), "uno");
        assert_eq!(again.get(&key("b")).unwrap().unwrap().expose(), "two");
        again.delete(&key("a")).unwrap();
        assert!(s.get(&key("a")).unwrap().is_none());
        assert!(s.get(&key("b")).unwrap().is_some());
    }

    #[test]
    fn file_on_disk_has_no_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), "pw");
        s.set(&key("conn-1"), &SecretString::new("sk-live-needle-value"))
            .unwrap();
        let raw = std::fs::read_to_string(s.path()).unwrap();
        assert!(!raw.contains("needle"));
        assert!(!raw.contains("conn-1"));
    }

    #[test]
    fn wrong_passphrase_is_an_auth_error_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), "right")
            .set(&key("a"), &SecretString::new("keep-me"))
            .unwrap();
        let before = std::fs::read(dir.path().join("secrets.enc.json")).unwrap();
        let wrong = store(dir.path(), "wrong");
        assert_eq!(wrong.get(&key("a")).unwrap_err().kind, ErrorKind::Auth);
        assert_eq!(
            wrong
                .set(&key("b"), &SecretString::new("x"))
                .unwrap_err()
                .kind,
            ErrorKind::Auth
        );
        assert_eq!(wrong.delete(&key("a")).unwrap_err().kind, ErrorKind::Auth);
        let after = std::fs::read(dir.path().join("secrets.enc.json")).unwrap();
        assert_eq!(before, after);
        assert_eq!(
            store(dir.path(), "right")
                .get(&key("a"))
                .unwrap()
                .unwrap()
                .expose(),
            "keep-me"
        );
    }

    #[test]
    fn errors_and_debug_never_contain_secret_or_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), "pass-needle");
        s.set(&key("a"), &SecretString::new("value-needle"))
            .unwrap();
        let wrong = store(dir.path(), "other-needle");
        let err = wrong.get(&key("a")).unwrap_err();
        for text in [format!("{err}"), format!("{err:?}"), format!("{s:?}")] {
            assert!(!text.contains("needle"), "{text}");
        }
    }

    #[test]
    fn empty_passphrase_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), "");
        assert_eq!(
            s.set(&key("a"), &SecretString::new("x")).unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
        assert!(!s.path().exists());
    }

    #[test]
    fn delete_on_missing_file_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), "pw");
        s.delete(&key("a")).unwrap();
        assert!(!s.path().exists());
    }

    #[test]
    fn garbage_truncated_and_oversize_files_are_errors_not_panics() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), "pw");
        s.set(&key("a"), &SecretString::new("v")).unwrap();
        let good = std::fs::read(s.path()).unwrap();
        for bad in [
            b"".to_vec(),
            b"not json".to_vec(),
            b"{}".to_vec(),
            good[..good.len() / 2].to_vec(),
            vec![b'x'; MAX_FILE_BYTES + 1],
        ] {
            std::fs::write(s.path(), bad).unwrap();
            assert!(s.get(&key("a")).is_err());
            assert!(s.set(&key("b"), &SecretString::new("x")).is_err());
        }
    }

    #[test]
    fn two_threads_writing_do_not_lose_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let s = store(&path, "pw");
                    s.set(&key(&format!("k{i}")), &SecretString::new(format!("v{i}")))
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
        let s = store(&path, "pw");
        for i in 0..4 {
            assert_eq!(
                s.get(&key(&format!("k{i}"))).unwrap().unwrap().expose(),
                format!("v{i}")
            );
        }
    }
}

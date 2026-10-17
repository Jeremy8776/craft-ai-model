//! [`SharedStore`]: the launcher's connections file on disk.
//!
//! The launcher writes it; every app reads it. The document format and its validation live in
//! [`craft_ai_core::StoreFile`]; this type adds the file handling around it:
//!
//! - **Reads** never block on a writer. Writes replace the file atomically, so a reader sees the
//!   old file or the new one.
//! - **Writes** go through [`update`](SharedStore::update): lock, read, change, validate, write.
//!   Two processes updating at once both land. A change that fails validation writes nothing.
//! - **A newer schema is never overwritten.** An older build that meets one refuses to write.
//! - **Unknown top-level fields survive** a rewrite by an older build of the same schema version,
//!   so a newer launcher's additions are not silently dropped. (Nested unknown fields are not
//!   preserved; only the launcher should rewrite the file in practice.)
//! - A missing file reads as an empty store, so an app works with no launcher installed.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use craft_ai_core::store::{MAX_STORE_BYTES, StoreError};
use craft_ai_core::{AiError, ErrorKind, StoreFile};
use serde_json::{Map, Value};

use crate::{fsutil, paths};

/// A parsed store plus the top-level fields this build does not know.
type Loaded = (StoreFile, Map<String, Value>);

/// Why a shared store operation failed.
#[derive(Debug)]
pub enum SharedError {
    /// The OS reports no config directory and `CRAFT_AI_DIR` is unset.
    NoLocation,
    /// The file could not be read, written or locked.
    Io(io::Error),
    /// The file or the change was refused by the data model (invalid, too large, too new).
    Store(StoreError),
}

impl fmt::Display for SharedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLocation => f.write_str("no place to keep the connections file on this system"),
            Self::Io(e) => write!(f, "connections file: {e}"),
            Self::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SharedError {}

impl From<io::Error> for SharedError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<StoreError> for SharedError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl From<SharedError> for AiError {
    fn from(e: SharedError) -> Self {
        let kind = match &e {
            SharedError::Store(StoreError::TooNew { .. }) => ErrorKind::Unsupported,
            _ => ErrorKind::Internal,
        };
        AiError::new(kind, e.to_string())
    }
}

/// The connections file at a path.
#[derive(Clone, Debug)]
pub struct SharedStore {
    path: PathBuf,
}

impl SharedStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The store at the standard per-OS location ([`paths::store_path`]).
    pub fn at_default_location() -> Result<Self, SharedError> {
        paths::store_path()
            .map(Self::new)
            .ok_or(SharedError::NoLocation)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the store. A missing file is an empty store.
    pub fn load(&self) -> Result<StoreFile, SharedError> {
        Ok(self
            .read_raw()?
            .map_or_else(StoreFile::default, |(store, _)| store))
    }

    /// Reads the store, returning `None` when the file does not exist (no launcher has written
    /// one yet), for callers that want to tell "empty" from "absent".
    pub fn load_existing(&self) -> Result<Option<StoreFile>, SharedError> {
        Ok(self.read_raw()?.map(|(store, _)| store))
    }

    /// Reads, changes and writes the store under the lock, returning what `f` returns.
    ///
    /// Nothing is written if reading fails, if the file is a newer schema, or if the changed
    /// store is invalid or too large.
    pub fn update<T>(&self, f: impl FnOnce(&mut StoreFile) -> T) -> Result<T, SharedError> {
        self.locked(|| {
            let (mut store, extras) = self
                .read_raw()?
                .unwrap_or_else(|| (StoreFile::default(), Map::new()));
            let out = f(&mut store);
            self.write(&store, extras)?;
            Ok(out)
        })
    }

    /// Replaces the file with `store`. A corrupt existing file is kept beside it as
    /// `<name>.corrupt`; a newer-schema file is refused, as in [`update`](Self::update).
    pub fn replace(&self, store: &StoreFile) -> Result<(), SharedError> {
        self.locked(|| {
            let extras = match self.read_raw() {
                Ok(found) => found.map_or_else(Map::new, |(_, extras)| extras),
                Err(SharedError::Store(StoreError::TooNew { found, supported })) => {
                    return Err(StoreError::TooNew { found, supported }.into());
                }
                Err(_) => {
                    let mut backup = self.path.clone().into_os_string();
                    backup.push(".corrupt");
                    let _ = std::fs::copy(&self.path, PathBuf::from(backup));
                    Map::new()
                }
            };
            self.write(store, extras)
        })
    }

    fn locked<T>(&self, f: impl FnOnce() -> Result<T, SharedError>) -> Result<T, SharedError> {
        fsutil::with_lock(&self.path, || Ok(f()))?
    }

    /// Parses the file and also returns its top-level fields this build does not know.
    fn read_raw(&self) -> Result<Option<Loaded>, SharedError> {
        let Some(bytes) = fsutil::read_capped(&self.path, MAX_STORE_BYTES).map_err(|e| {
            if e.kind() == io::ErrorKind::InvalidData {
                SharedError::Store(StoreError::TooLarge {
                    len: MAX_STORE_BYTES.saturating_add(1),
                    max: MAX_STORE_BYTES,
                })
            } else {
                SharedError::Io(e)
            }
        })?
        else {
            return Ok(None);
        };
        let text = String::from_utf8(bytes)
            .map_err(|_| StoreError::Syntax("the file is not valid text".to_owned()))?;
        let store = StoreFile::parse(&text)?;
        Ok(Some((store, unknown_fields(&text))))
    }

    fn write(&self, store: &StoreFile, extras: Map<String, Value>) -> Result<(), SharedError> {
        store.validate()?;
        let mut value =
            serde_json::to_value(store).map_err(|e| StoreError::Syntax(e.to_string()))?;
        if let Value::Object(map) = &mut value {
            for (k, v) in extras {
                map.entry(k).or_insert(v);
            }
        }
        let text =
            serde_json::to_string_pretty(&value).map_err(|e| StoreError::Syntax(e.to_string()))?;
        if text.len() > MAX_STORE_BYTES {
            return Err(StoreError::TooLarge {
                len: text.len(),
                max: MAX_STORE_BYTES,
            }
            .into());
        }
        fsutil::atomic_write(&self.path, text.as_bytes())?;
        Ok(())
    }
}

/// Top-level keys of the document that `StoreFile` has no field for.
fn unknown_fields(text: &str) -> Map<String, Value> {
    const KNOWN: [&str; 6] = [
        "schema_version",
        "connections",
        "defaults",
        "apps",
        "custom_tools",
        "model_folders",
    ];
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => map
            .into_iter()
            .filter(|(k, _)| !KNOWN.contains(&k.as_str()))
            .collect(),
        _ => Map::new(),
    }
}

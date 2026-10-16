//! [`ConsentFile`]: where an app keeps its consent ledger on disk.
//!
//! Consent belongs to the app (it says exactly what that app sends), so each app has its own file,
//! `consent/<app>.json` under the AI directory. The ledger type and its rules live in
//! `craft_ai_core::ConsentLedger`; this adds the file handling.
//!
//! **Fails closed.** A missing file is an empty ledger (nothing agreed yet). A damaged one is an
//! error from [`load`](ConsentFile::load), and [`load_or_empty`](ConsentFile::load_or_empty) treats
//! it as empty, so damage can only make the app ask again, never skip asking.

use std::path::{Path, PathBuf};

use craft_ai_core::{AiError, AppId, ConsentLedger, ErrorKind};

use crate::{fsutil, paths};

/// Largest consent file accepted, in bytes.
const MAX_BYTES: usize = 256 * 1024;

/// One app's consent ledger on disk.
#[derive(Clone, Debug)]
pub struct ConsentFile {
    path: PathBuf,
}

impl ConsentFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The standard file for `app`, or `None` if there is no config directory.
    pub fn at_default_location(app: &AppId) -> Option<Self> {
        paths::ai_dir().map(|d| Self::new(d.join("consent").join(file_name(app))))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the ledger. A missing file is an empty ledger; a damaged one is an error.
    pub fn load(&self) -> Result<ConsentLedger, AiError> {
        let bytes = fsutil::read_capped(&self.path, MAX_BYTES)
            .map_err(|_| problem("the consent file could not be read"))?;
        match bytes {
            None => Ok(ConsentLedger::default()),
            Some(b) => {
                serde_json::from_slice(&b).map_err(|_| problem("the consent file is damaged"))
            }
        }
    }

    /// Reads the ledger, treating any problem as "nothing agreed yet".
    pub fn load_or_empty(&self) -> ConsentLedger {
        self.load().unwrap_or_default()
    }

    pub fn save(&self, ledger: &ConsentLedger) -> Result<(), AiError> {
        let json = serde_json::to_vec_pretty(ledger)
            .map_err(|_| problem("could not encode the consent file"))?;
        fsutil::with_lock(&self.path, || fsutil::atomic_write(&self.path, &json))
            .map_err(|_| problem("the consent file could not be written"))
    }
}

fn problem(what: &str) -> AiError {
    AiError::new(ErrorKind::Internal, what)
}

/// A file name that is safe whatever the app id holds (ids may contain `/`, `.` and `:`), and
/// unique per id: the readable part is cleaned, and a hash of the exact id is appended.
fn file_name(app: &AppId) -> String {
    let clean: String = app
        .as_str()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    let mut hash: u32 = 0x811c_9dc5;
    for b in app.as_str().bytes() {
        hash = (hash ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    format!("{clean}-{hash:08x}.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_ai_core::consent::ConsentRecord;
    use craft_ai_core::{CapabilityId, ConnectionId};

    fn app(s: &str) -> AppId {
        AppId::new(s).unwrap()
    }

    fn ledger_with_record() -> ConsentLedger {
        let mut l = ConsentLedger::default();
        l.records.push(ConsentRecord {
            app: app("photocraft"),
            connection: ConnectionId::new("c1").unwrap(),
            destination: "https://api.example.com".into(),
            capability: Some(CapabilityId::parse("image.inpaint").unwrap()),
            kinds: vec![],
            expires_at: Some(1_000),
        });
        l
    }

    #[test]
    fn missing_is_empty_and_save_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let f = ConsentFile::new(dir.path().join("consent").join("a.json"));
        assert_eq!(f.load().unwrap(), ConsentLedger::default());
        f.save(&ledger_with_record()).unwrap();
        assert_eq!(f.load().unwrap(), ledger_with_record());
    }

    #[test]
    fn damage_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let f = ConsentFile::new(dir.path().join("a.json"));
        std::fs::write(f.path(), "{ broken").unwrap();
        assert!(f.load().is_err());
        assert!(f.load_or_empty().records.is_empty());
        std::fs::write(f.path(), vec![b' '; MAX_BYTES + 1]).unwrap();
        assert!(f.load().is_err());
    }

    #[test]
    fn file_names_are_safe_and_distinct() {
        let a = file_name(&app("../../etc/passwd"));
        assert!(!a.contains('/') && !a.contains('\\') && !a.contains(".."));
        assert!(a.ends_with(".json"));
        // Ids that clean to the same text still get different names.
        assert_ne!(file_name(&app("a/b")), file_name(&app("a.b")));
        assert_eq!(file_name(&app("a/b")), file_name(&app("a/b")));
        let long = file_name(&app(&"x".repeat(120)));
        assert!(long.len() < 60);
    }

    #[test]
    fn default_location_is_per_app() {
        // Only checks the shape; the directory itself depends on the machine.
        if let Some(f) = ConsentFile::at_default_location(&app("photocraft")) {
            assert!(f.path().to_string_lossy().contains("consent"));
        }
    }
}

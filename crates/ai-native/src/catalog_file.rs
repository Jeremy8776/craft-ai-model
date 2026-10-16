//! [`CatalogFile`]: the curated model picks, updated without an app release.
//!
//! The picks a person sees as "Recommended" or "Best result" are a judgement kept as data. A copy
//! is built into every app so it works offline; this file, in the shared AI folder, sits on top of
//! it, so a newer list can be dropped in (by hand, or by whatever keeps it up to date) and every
//! app uses it at once. Nothing is fetched here.
//!
//! **Never worse than the built-in list.** A missing file is just the built-in catalog. A damaged,
//! unreadable or oversized one is also the built-in catalog, with a plain sentence saying why, so
//! a bad edit can only lose the update, never the picks.

use std::path::{Path, PathBuf};

use craft_ai_core::Catalog;
use craft_ai_core::picks::MAX_CATALOG_BYTES;

use crate::{fsutil, paths};

/// The catalog an app should use, and why it is not the file's, if it is not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogLoad {
    pub catalog: Catalog,
    /// Set when a file exists but could not be used. Fit to show a person.
    pub problem: Option<String>,
}

/// The shared curated picks file.
#[derive(Clone, Debug)]
pub struct CatalogFile {
    path: PathBuf,
}

impl CatalogFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The standard file, or `None` if there is no config directory.
    pub fn at_default_location() -> Option<Self> {
        paths::catalog_path().map(Self::new)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file's picks over the built-in ones.
    pub fn load(&self) -> CatalogLoad {
        self.load_over(Catalog::builtin())
    }

    /// The file's picks over `built_in`, which is what an app ships with.
    pub fn load_over(&self, built_in: Catalog) -> CatalogLoad {
        let fail = |why: &str| CatalogLoad {
            catalog: built_in.clone(),
            problem: Some(format!(
                "The updated list of recommended models could not be used ({why}), so the one that came with the app is in use."
            )),
        };
        let bytes = match fsutil::read_capped(&self.path, MAX_CATALOG_BYTES) {
            Ok(None) => {
                return CatalogLoad {
                    catalog: built_in.clone(),
                    problem: None,
                };
            }
            Ok(Some(b)) => b,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => return fail("it is too big"),
            Err(_) => return fail("it could not be read"),
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return fail("it is damaged");
        };
        match Catalog::from_json(&text) {
            Ok(file) => CatalogLoad {
                catalog: file.over(built_in),
                problem: None,
            },
            Err(_) => fail("it is damaged"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_with(text: &[u8]) -> (tempfile::TempDir, CatalogFile) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("catalog.json");
        std::fs::write(&p, text).unwrap();
        (dir, CatalogFile::new(p))
    }

    fn shipped() -> Catalog {
        Catalog::from_json(
            r#"{"version":1,"entries":[{"capability":"text.complete","pick":"best_result","provider":"openrouter","model":"shipped"}]}"#,
        )
        .unwrap()
    }

    const ONE: &str = r#"{"version":7,"entries":[{"capability":"text.complete","pick":"recommended","provider":"openrouter","model":"m"}]}"#;

    #[test]
    fn no_file_is_the_built_in_catalog_without_a_complaint() {
        let dir = tempfile::tempdir().unwrap();
        let shipped = shipped();
        let got = CatalogFile::new(dir.path().join("catalog.json")).load_over(shipped.clone());
        assert_eq!(got.catalog, shipped);
        assert!(got.problem.is_none());
    }

    #[test]
    fn a_good_file_is_used_and_counts_as_newer() {
        let (_d, f) = file_with(ONE.as_bytes());
        let got = f.load_over(shipped());
        assert!(got.problem.is_none());
        assert_eq!(got.catalog.version, 7);
        let models: Vec<_> = got
            .catalog
            .entries
            .iter()
            .map(|e| e.model.as_str())
            .collect();
        assert_eq!(
            models,
            ["m", "shipped"],
            "the file first, built-in underneath"
        );
    }

    #[test]
    fn a_damaged_or_oversized_file_falls_back_with_a_plain_reason() {
        for bytes in [
            b"{ not json".to_vec(),
            vec![0xff, 0xfe, 0x00],
            vec![b' '; MAX_CATALOG_BYTES + 1],
        ] {
            let (_d, f) = file_with(&bytes);
            let got = f.load_over(shipped());
            assert_eq!(got.catalog, shipped());
            let why = got.problem.expect("a reason is given");
            for jargon in ["json", "utf", "parse", "serde", "io error"] {
                assert!(!why.to_lowercase().contains(jargon), "{why}");
            }
        }
    }
}

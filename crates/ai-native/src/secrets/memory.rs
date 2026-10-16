//! In-memory secret store. Useful in tests, including the test suites of the apps.

use std::collections::BTreeMap;
use std::sync::Mutex;

use craft_ai_core::{AiError, SecretRef, SecretString};

use super::SecretStore;

/// A map of secrets that lives and dies with the process.
#[derive(Default)]
pub struct MemoryStore {
    map: Mutex<BTreeMap<String, String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn with<T>(&self, f: impl FnOnce(&mut BTreeMap<String, String>) -> T) -> T {
        let mut guard = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard)
    }
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.with(|m| m.len());
        write!(f, "MemoryStore({n} secrets)")
    }
}

impl SecretStore for MemoryStore {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn get(&self, key: &SecretRef) -> Result<Option<SecretString>, AiError> {
        Ok(self.with(|m| m.get(key.as_str()).map(SecretString::new)))
    }

    fn set(&self, key: &SecretRef, value: &SecretString) -> Result<(), AiError> {
        self.with(|m| m.insert(key.as_str().to_owned(), value.expose().to_owned()));
        Ok(())
    }

    fn delete(&self, key: &SecretRef) -> Result<(), AiError> {
        self.with(|m| m.remove(key.as_str()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn key(s: &str) -> SecretRef {
        SecretRef::new(s).unwrap()
    }

    #[test]
    fn set_get_replace_delete() {
        let s = MemoryStore::new();
        assert!(s.get(&key("a")).unwrap().is_none());
        s.set(&key("a"), &SecretString::new("one")).unwrap();
        s.set(&key("a"), &SecretString::new("two")).unwrap();
        assert_eq!(s.get(&key("a")).unwrap().unwrap().expose(), "two");
        s.delete(&key("a")).unwrap();
        s.delete(&key("a")).unwrap();
        assert!(s.get(&key("a")).unwrap().is_none());
    }

    #[test]
    fn debug_shows_count_only() {
        let s = MemoryStore::new();
        s.set(&key("a"), &SecretString::new("hunter2-value"))
            .unwrap();
        let shown = format!("{s:?}");
        assert!(!shown.contains("hunter2"));
        assert!(!shown.contains("\"a\""));
    }
}

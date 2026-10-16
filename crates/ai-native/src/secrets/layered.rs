//! An ordered chain of secret stores, such as environment, then keychain, then encrypted file.

use craft_ai_core::{AiError, ErrorKind, SecretRef, SecretString};

use super::SecretStore;

/// Tries each store in order.
///
/// - `get`: the first store holding the key wins. A store that is unavailable (an
///   [`ErrorKind::Unsupported`] error, such as no keychain on a headless box) is skipped. Any other
///   error is remembered and returned only if no store held the key, so one broken layer cannot
///   hide a good answer from another.
/// - `set`: written to the first store that accepts it. Read-only and unavailable stores are
///   skipped.
/// - `delete`: removed from **every** store that can, so an old copy in a lower layer cannot come
///   back after the user deletes a key.
pub struct LayeredStore {
    layers: Vec<Box<dyn SecretStore>>,
}

impl LayeredStore {
    pub fn new(layers: Vec<Box<dyn SecretStore>>) -> Self {
        Self { layers }
    }

    /// Names of the layers in order, for diagnostics.
    pub fn layer_names(&self) -> Vec<&'static str> {
        self.layers.iter().map(|l| l.name()).collect()
    }
}

impl std::fmt::Debug for LayeredStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LayeredStore({:?})", self.layer_names())
    }
}

impl SecretStore for LayeredStore {
    fn name(&self) -> &'static str {
        "layered"
    }

    fn get(&self, key: &SecretRef) -> Result<Option<SecretString>, AiError> {
        let mut first_error: Option<AiError> = None;
        for layer in &self.layers {
            match layer.get(key) {
                Ok(Some(secret)) => return Ok(Some(secret)),
                Ok(None) => {}
                Err(e) if e.kind == ErrorKind::Unsupported => {}
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        first_error.map_or(Ok(None), Err)
    }

    fn set(&self, key: &SecretRef, value: &SecretString) -> Result<(), AiError> {
        let mut first_error: Option<AiError> = None;
        for layer in &self.layers {
            match layer.set(key, value) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind == ErrorKind::Unsupported => {}
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        Err(first_error.unwrap_or_else(|| {
            AiError::unsupported("no secret store is available to save this key")
        }))
    }

    fn delete(&self, key: &SecretRef) -> Result<(), AiError> {
        let mut first_error: Option<AiError> = None;
        for layer in &self.layers {
            match layer.delete(key) {
                Ok(()) => {}
                Err(e) if e.kind == ErrorKind::Unsupported => {}
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{EnvStore, MemoryStore};

    fn key(s: &str) -> SecretRef {
        SecretRef::new(s).unwrap()
    }

    /// A store that always fails with a chosen kind.
    struct Failing(ErrorKind);

    impl SecretStore for Failing {
        fn name(&self) -> &'static str {
            "failing"
        }
        fn get(&self, _: &SecretRef) -> Result<Option<SecretString>, AiError> {
            Err(AiError::new(self.0, "boom"))
        }
        fn set(&self, _: &SecretRef, _: &SecretString) -> Result<(), AiError> {
            Err(AiError::new(self.0, "boom"))
        }
        fn delete(&self, _: &SecretRef) -> Result<(), AiError> {
            Err(AiError::new(self.0, "boom"))
        }
    }

    #[test]
    fn first_layer_with_the_key_wins() {
        let top = MemoryStore::new();
        let bottom = MemoryStore::new();
        top.set(&key("a"), &SecretString::new("top")).unwrap();
        bottom.set(&key("a"), &SecretString::new("bottom")).unwrap();
        bottom
            .set(&key("b"), &SecretString::new("only-bottom"))
            .unwrap();
        let s = LayeredStore::new(vec![Box::new(top), Box::new(bottom)]);
        assert_eq!(s.get(&key("a")).unwrap().unwrap().expose(), "top");
        assert_eq!(s.get(&key("b")).unwrap().unwrap().expose(), "only-bottom");
        assert!(s.get(&key("c")).unwrap().is_none());
    }

    #[test]
    fn unavailable_layer_is_skipped_but_a_real_fault_does_not_hide_a_hit() {
        let mem = MemoryStore::new();
        mem.set(&key("a"), &SecretString::new("v")).unwrap();
        let s = LayeredStore::new(vec![
            Box::new(Failing(ErrorKind::Unsupported)),
            Box::new(Failing(ErrorKind::Internal)),
            Box::new(mem),
        ]);
        assert_eq!(s.get(&key("a")).unwrap().unwrap().expose(), "v");
        // A miss with a real fault below reports the fault rather than pretending "not found".
        assert_eq!(s.get(&key("zzz")).unwrap_err().kind, ErrorKind::Internal);
    }

    #[test]
    fn set_goes_to_first_writable_layer() {
        let s = LayeredStore::new(vec![
            Box::new(EnvStore::with_lookup(|_| None)),
            Box::new(Failing(ErrorKind::Unsupported)),
            Box::new(MemoryStore::new()),
        ]);
        s.set(&key("a"), &SecretString::new("v")).unwrap();
        assert_eq!(s.get(&key("a")).unwrap().unwrap().expose(), "v");
    }

    #[test]
    fn set_with_no_writable_layer_says_so() {
        let s = LayeredStore::new(vec![Box::new(EnvStore::with_lookup(|_| None))]);
        let e = s.set(&key("a"), &SecretString::new("v")).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unsupported);
        let empty = LayeredStore::new(vec![]);
        assert!(empty.set(&key("a"), &SecretString::new("v")).is_err());
    }

    #[test]
    fn delete_clears_every_layer() {
        let a = std::sync::Arc::new(MemoryStore::new());
        let b = std::sync::Arc::new(MemoryStore::new());
        a.set(&key("k"), &SecretString::new("1")).unwrap();
        b.set(&key("k"), &SecretString::new("2")).unwrap();
        struct Shared(std::sync::Arc<MemoryStore>);
        impl SecretStore for Shared {
            fn name(&self) -> &'static str {
                "shared"
            }
            fn get(&self, k: &SecretRef) -> Result<Option<SecretString>, AiError> {
                self.0.get(k)
            }
            fn set(&self, k: &SecretRef, v: &SecretString) -> Result<(), AiError> {
                self.0.set(k, v)
            }
            fn delete(&self, k: &SecretRef) -> Result<(), AiError> {
                self.0.delete(k)
            }
        }
        let s = LayeredStore::new(vec![
            Box::new(Shared(a.clone())),
            Box::new(Shared(b.clone())),
        ]);
        s.delete(&key("k")).unwrap();
        assert!(a.get(&key("k")).unwrap().is_none());
        assert!(b.get(&key("k")).unwrap().is_none());
    }
}

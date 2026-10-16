//! Environment secrets for headless runs (CI, servers, containers): read-only.
//!
//! The variable for a reference is `CRAFT_AI_SECRET_` plus the reference upper-cased with every
//! character that is not a letter or digit turned into `_`. So `conn_01/key` is read from
//! `CRAFT_AI_SECRET_CONN_01_KEY`. Two references that differ only in punctuation share a
//! variable; that is acceptable for a fallback and documented here.
//!
//! The launcher never starts an app with secrets in its environment. This store exists
//! for runs where no launcher or keychain is present.

use craft_ai_core::{AiError, SecretRef, SecretString};

use super::{SecretStore, read_only};

/// Prefix of every variable this store reads.
pub const PREFIX: &str = "CRAFT_AI_SECRET_";

type Lookup = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Reads secrets from environment variables.
pub struct EnvStore {
    lookup: Lookup,
}

impl EnvStore {
    /// Reads the real process environment.
    pub fn new() -> Self {
        Self::with_lookup(|name| std::env::var(name).ok())
    }

    /// Reads through `lookup`, so tests need not touch the process environment.
    pub fn with_lookup(lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        Self {
            lookup: Box::new(lookup),
        }
    }

    /// The variable name for a reference.
    pub fn var_name(key: &SecretRef) -> String {
        let tail: String = key
            .as_str()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        format!("{PREFIX}{tail}")
    }
}

impl Default for EnvStore {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for EnvStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EnvStore")
    }
}

impl SecretStore for EnvStore {
    fn name(&self) -> &'static str {
        "environment"
    }

    fn get(&self, key: &SecretRef) -> Result<Option<SecretString>, AiError> {
        Ok((self.lookup)(&Self::var_name(key))
            .filter(|v| !v.is_empty())
            .map(SecretString::new))
    }

    fn set(&self, _key: &SecretRef, _value: &SecretString) -> Result<(), AiError> {
        Err(read_only(self.name()))
    }

    fn delete(&self, _key: &SecretRef) -> Result<(), AiError> {
        Err(read_only(self.name()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_ai_core::ErrorKind;

    fn key(s: &str) -> SecretRef {
        SecretRef::new(s).unwrap()
    }

    #[test]
    fn variable_name_is_upper_case_with_punctuation_flattened() {
        assert_eq!(
            EnvStore::var_name(&key("conn_01/key.v2")),
            "CRAFT_AI_SECRET_CONN_01_KEY_V2"
        );
    }

    #[test]
    fn reads_through_the_lookup_and_ignores_empty() {
        let s = EnvStore::with_lookup(|n| match n {
            "CRAFT_AI_SECRET_A" => Some("value".to_owned()),
            "CRAFT_AI_SECRET_B" => Some(String::new()),
            _ => None,
        });
        assert_eq!(s.get(&key("a")).unwrap().unwrap().expose(), "value");
        assert!(s.get(&key("b")).unwrap().is_none());
        assert!(s.get(&key("c")).unwrap().is_none());
    }

    #[test]
    fn is_read_only() {
        let s = EnvStore::with_lookup(|_| None);
        let e = s.set(&key("a"), &SecretString::new("x")).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unsupported);
        assert_eq!(
            s.delete(&key("a")).unwrap_err().kind,
            ErrorKind::Unsupported
        );
    }
}

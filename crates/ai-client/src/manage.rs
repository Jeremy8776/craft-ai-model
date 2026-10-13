//! Adding, editing, removing and testing connections: what the launcher's connections screen (and
//! the small fallback form inside an app) calls.
//!
//! It is the one writer of the shared store and of the keys that go with it, so the two cannot
//! drift apart. The rules a person types against live in
//! [`ConnectionDraft`](craft_ai_model::ConnectionDraft); this adds the parts that touch disk and the
//! keychain, in an order that cannot lose a key:
//!
//! - **Save** takes the file lock, writes the new key, then the connection, and puts the old key
//!   back if the file cannot be written. Either both are saved or neither changed, and two saves
//!   at once can never share or delete each other's key.
//! - **Delete** removes the connection and every default or app choice that pointed at it in one
//!   locked write, then its key (unless another connection still uses that locator).
//! - **Test** asks the server for its models with short timeouts, without saving anything.
//!
//! Every call blocks, so a screen runs it on a worker thread. Nothing here puts a key in an error
//! message, a log or a file other than the secret store.

use std::sync::Arc;
use std::time::{Duration, Instant};

use craft_ai_model::{
    AiError, Connection, ConnectionDraft, ConnectionId, DraftError, ErrorKind, KeyChange,
    ModelInfo, SecretRef, StoreFile,
};
use craft_ai_native::secrets::{EnvStore, KeychainStore, LayeredStore, SecretStore, secret_for};
use craft_ai_native::shared::SharedStore;

use crate::factory::ProviderFactory;

mod probe;
pub use probe::{
    Confirmed, MAX_DECLARED, ModelChoice, ProbeReport, ProbedModel, Unconfirmed, declare_models,
};

/// Whether a connection's key is where it should be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyState {
    /// The connection needs no key.
    NotNeeded,
    Saved,
    /// It needs a key and none is saved.
    Missing,
    /// The store could not be read (a locked keychain, a damaged file).
    Unreadable,
}

/// One saved connection as a screen shows it.
#[derive(Clone, Debug)]
pub struct ConnectionView {
    pub connection: Connection,
    pub key: KeyState,
    /// Default or app choices that use it, so a delete can warn.
    pub bindings: usize,
}

/// Why a save did not happen.
#[derive(Debug)]
pub enum SaveError {
    /// The form has a problem the person can fix.
    Invalid(DraftError),
    /// The disk or keychain refused. The message is safe to show.
    Failed(AiError),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(e) => e.fmt(f),
            Self::Failed(e) => f.write_str(&e.message),
        }
    }
}

impl std::error::Error for SaveError {}

/// A completed save.
#[derive(Clone, Debug)]
pub struct Saved {
    pub id: ConnectionId,
    /// Something minor went wrong after the save (an old key could not be removed).
    pub warning: Option<String>,
}

/// A completed delete.
#[derive(Clone, Debug)]
pub struct Removed {
    /// Default and per-app choices that pointed at it and were removed with it.
    pub bindings_removed: usize,
    /// `false` if the saved key could not be removed; the connection is gone either way.
    pub key_removed: bool,
}

/// What a test found.
#[derive(Clone, Debug)]
pub struct TestReport {
    /// The models the server offers. Empty for a custom connection, whose models are only what a
    /// person declares (see `probe`).
    pub models: Vec<ModelInfo>,
    pub elapsed: Duration,
    /// For a custom connection: what the test confirmed and could not confirm.
    pub probe: Option<ProbeReport>,
}

/// Reads and writes the user's connections.
#[derive(Clone)]
pub struct ConnectionManager {
    shared: SharedStore,
    secrets: Arc<dyn SecretStore>,
    probe: ProviderFactory,
}

impl ConnectionManager {
    pub fn new(shared: SharedStore, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            shared,
            secrets,
            probe: ProviderFactory::probing(),
        }
    }

    /// The usual setup: the standard store location, keys in the environment (read only) then the
    /// OS keychain.
    pub fn standard() -> Result<Self, AiError> {
        let shared = SharedStore::at_default_location()?;
        let secrets = LayeredStore::new(vec![
            Box::new(EnvStore::new()),
            Box::new(KeychainStore::new()),
        ]);
        Ok(Self::new(shared, Arc::new(secrets)))
    }

    /// Replaces the factory used by the Test button, to add adapters.
    pub fn with_probe_factory(mut self, factory: ProviderFactory) -> Self {
        self.probe = factory;
        self
    }

    pub fn store_path(&self) -> &std::path::Path {
        self.shared.path()
    }

    fn load(&self) -> Result<StoreFile, AiError> {
        Ok(self.shared.load()?)
    }

    /// Every connection with the state of its key.
    pub fn views(&self) -> Result<Vec<ConnectionView>, AiError> {
        let store = self.load()?;
        Ok(store
            .connections
            .iter()
            .map(|c| ConnectionView {
                key: self.key_state(c),
                bindings: count_bindings(&store, &c.id),
                connection: c.clone(),
            })
            .collect())
    }

    fn key_state(&self, c: &Connection) -> KeyState {
        let Some(key) = c.auth.secret_ref() else {
            return KeyState::NotNeeded;
        };
        match self.secrets.get(key) {
            Ok(Some(s)) if !s.is_empty() => KeyState::Saved,
            Ok(_) => KeyState::Missing,
            Err(_) => KeyState::Unreadable,
        }
    }

    /// Adds a connection or saves changes to one.
    pub fn save(&self, draft: &ConnectionDraft) -> Result<Saved, SaveError> {
        let current = self.load().map_err(SaveError::Failed)?;
        let prepared = draft
            .prepare(&current.connections)
            .map_err(SaveError::Invalid)?;
        let key_ref = prepared.connection.auth.secret_ref().cloned();

        // 1. Under the file lock, the connection is re-checked against the file as it is now, then
        // the key is written, then the connection. Holding the lock across the key write means two
        // saves can never pick the same key slot and overwrite (or, on undo, delete) each other's key.
        let wanted = key_ref.clone();
        let secrets = &self.secrets;
        let mut undo = None;
        let result = self
            .shared
            .update(|file| -> Result<ConnectionId, SaveError> {
                let again = draft
                    .prepare(&file.connections)
                    .map_err(SaveError::Invalid)?;
                if again.connection.auth.secret_ref() != wanted.as_ref() {
                    return Err(SaveError::Invalid(DraftError {
                        field: craft_ai_model::Field::Label,
                        message: "Another change was saved at the same moment. Try again."
                            .to_owned(),
                    }));
                }
                if let (KeyChange::Set(key), Some(r)) = (&again.key, &wanted) {
                    let previous = secrets.get(r).ok().flatten();
                    secrets
                        .set(r, key)
                        .map_err(|_| SaveError::Failed(key_not_saved(r)))?;
                    undo = Some((r.clone(), previous));
                }
                let id = again.connection.id.clone();
                match file.connections.iter_mut().find(|c| c.id == id) {
                    Some(slot) => *slot = again.connection,
                    None => file.connections.push(again.connection),
                }
                Ok(id)
            });
        let outcome = match result {
            Ok(done) => done,
            Err(e) => Err(SaveError::Failed(e.into())),
        };

        // 2. If the file could not be written after the key was, put the old key back. This is the
        // only failure left after the key write, and it is rare (a full or read-only disk).
        let id = match outcome {
            Ok(id) => id,
            Err(e) => {
                if let Some((r, previous)) = undo {
                    let _ = match previous {
                        Some(old) => self.secrets.set(&r, &old),
                        None => self.secrets.delete(&r),
                    };
                }
                return Err(e);
            }
        };

        // 3. A key the connection no longer uses.
        let mut warning = None;
        if matches!(prepared.key, KeyChange::Clear)
            && let Some(old) = current
                .connections
                .iter()
                .find(|c| c.id == id)
                .and_then(|c| c.auth.secret_ref())
            && self.secrets.delete(old).is_err()
        {
            warning = Some("The old key could not be removed from the keychain.".to_owned());
        }
        Ok(Saved { id, warning })
    }

    /// Turns a connection on or off without losing it.
    pub fn set_enabled(&self, id: &ConnectionId, enabled: bool) -> Result<(), AiError> {
        let found = self.shared.update(|file| {
            file.connections
                .iter_mut()
                .find(|c| &c.id == id)
                .map(|c| c.enabled = enabled)
                .is_some()
        })?;
        if found { Ok(()) } else { Err(not_found(id)) }
    }

    /// Removes a connection, the choices that used it, and its key.
    pub fn delete(&self, id: &ConnectionId) -> Result<Removed, AiError> {
        let (gone, bindings_removed, key_still_used) = self
            .shared
            .update(|file| {
                let pos = file.connections.iter().position(|c| &c.id == id)?;
                let gone = file.connections.remove(pos);
                let before = count_bindings(file, id);
                file.defaults.retain(|b| &b.connection != id);
                for app in file.apps.values_mut() {
                    app.overrides.retain(|b| &b.connection != id);
                }
                let key_still_used = gone.auth.secret_ref().is_some_and(|r| {
                    file.connections
                        .iter()
                        .any(|c| c.auth.secret_ref() == Some(r))
                });
                Some((gone, before, key_still_used))
            })?
            .ok_or_else(|| not_found(id))?;
        let key_removed = match gone.auth.secret_ref() {
            Some(r) if !key_still_used => self.secrets.delete(r).is_ok(),
            _ => true,
        };
        Ok(Removed {
            bindings_removed,
            key_removed,
        })
    }

    /// Asks the server in `draft` for its models without saving anything, so a key can be tried
    /// before it is kept.
    pub fn test_draft(&self, draft: &ConnectionDraft) -> Result<TestReport, AiError> {
        let current = self.load()?;
        let prepared = draft
            .prepare(&current.connections)
            .map_err(|e| AiError::invalid(e.message))?;
        let secret = match (&prepared.key, prepared.connection.auth.secret_ref()) {
            (KeyChange::Set(key), _) => Some(key.clone()),
            (KeyChange::Keep, Some(r)) => self.secrets.get(r)?.filter(|s| !s.is_empty()),
            _ => None,
        };
        self.ask(prepared.connection, secret)
    }

    /// Tests a saved connection. With `remember`, the models found replace the saved list, so apps
    /// offer what the server really has.
    pub fn test_saved(&self, id: &ConnectionId, remember: bool) -> Result<TestReport, AiError> {
        let current = self.load()?;
        let connection = current
            .connections
            .iter()
            .find(|c| &c.id == id)
            .cloned()
            .ok_or_else(|| not_found(id))?;
        let secret = secret_for(self.secrets.as_ref(), &connection)?;
        let report = self.ask(connection, secret)?;
        // Only a list the server reported itself is remembered; a custom connection's models
        // are the ones a person declared, and a test never replaces them with guesses.
        if remember && report.probe.is_none() {
            let models = report.models.clone();
            self.shared.update(|file| {
                if let Some(c) = file.connections.iter_mut().find(|c| &c.id == id) {
                    c.models = models;
                }
            })?;
        }
        Ok(report)
    }

    fn ask(
        &self,
        mut connection: Connection,
        secret: Option<craft_ai_model::SecretString>,
    ) -> Result<TestReport, AiError> {
        if connection.provider == probe::CUSTOM_PROVIDER {
            let start = Instant::now();
            let probe = probe::run(&connection, secret)?;
            return Ok(TestReport {
                models: Vec::new(),
                elapsed: start.elapsed(),
                probe: Some(probe),
            });
        }
        // Saved models would be answered from the list, not the server.
        connection.models.clear();
        let provider = self.probe.build(&connection, secret)?;
        let start = Instant::now();
        let models = provider.models()?;
        Ok(TestReport {
            models,
            elapsed: start.elapsed(),
            probe: None,
        })
    }
}

impl std::fmt::Debug for ConnectionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ConnectionManager({})", self.shared.path().display())
    }
}

fn count_bindings(store: &StoreFile, id: &ConnectionId) -> usize {
    let defaults = store
        .defaults
        .iter()
        .filter(|b| &b.connection == id)
        .count();
    let overrides: usize = store
        .apps
        .values()
        .map(|a| a.overrides.iter().filter(|b| &b.connection == id).count())
        .sum();
    defaults + overrides
}

fn not_found(id: &ConnectionId) -> AiError {
    AiError::new(
        ErrorKind::InvalidRequest,
        format!("there is no connection \"{id}\" (it may have been removed elsewhere)"),
    )
}

/// What to tell someone whose keychain refused a key. The cause is not shown (it can name system
/// internals), but the way out is.
fn key_not_saved(key: &SecretRef) -> AiError {
    let var = craft_ai_native::secrets::EnvStore::var_name(key);
    AiError::new(
        ErrorKind::Internal,
        format!(
            "The key could not be saved in this computer's keychain. On a computer without one, \
             set the environment variable {var} to the key instead."
        ),
    )
}

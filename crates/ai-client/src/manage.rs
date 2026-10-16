//! Adding, editing, removing and testing connections: what the launcher's connections screen (and
//! the small fallback form inside an app) calls.
//!
//! It is the one writer of the shared store and of the keys that go with it, so the two cannot
//! drift apart. The rules a person types against live in
//! [`ConnectionDraft`](craft_ai_core::ConnectionDraft); this adds the parts that touch disk and the
//! keychain, in an order that cannot lose a key:
//!
//! - **Save** writes a new key into a slot of its own with no lock held, then points the
//!   connection at it under the file lock, then deletes the slot it replaced. No key is ever
//!   overwritten, so a failed save only deletes the slot it just wrote, two saves at once can
//!   never share or delete each other's key, and the lock is never held across the keychain.
//! - **Delete** removes the connection and every default or app choice that pointed at it in one
//!   locked write, then its key (unless another connection still uses that locator).
//! - **Test** asks the server for its models with short timeouts, without saving anything.
//!
//! Every call blocks, so a screen runs it on a worker thread. Nothing here puts a key in an error
//! message, a log or a file other than the secret store.

use std::sync::Arc;
use std::time::{Duration, Instant};

use craft_ai_core::{
    AiError, Auth, Connection, ConnectionDraft, ConnectionId, DraftError, ErrorKind, KeyChange,
    ModelInfo, SecretRef, SecretString, StoreFile,
};
use craft_ai_native::secrets::{EnvStore, KeychainStore, LayeredStore, SecretStore, secret_for};
use craft_ai_native::shared::SharedStore;

use crate::factory::ProviderFactory;

mod probe;
mod slot;
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
    ///
    /// A new key goes into a slot of its own, with the file lock not held (a keychain can wait on
    /// a prompt or a daemon, and every other app waits on that lock). Under the lock the file
    /// only ever swaps to a slot that already holds its key, so a connection never points at a
    /// missing key. A key is never overwritten: if the file cannot be written, the slot just
    /// written is deleted and everything else is as it was; once it is written, the slot the
    /// connection used to point at is deleted. A crash between the two writes leaves one unused
    /// slot and nothing else.
    pub fn save(&self, draft: &ConnectionDraft) -> Result<Saved, SaveError> {
        let current = self.load().map_err(SaveError::Failed)?;
        let prepared = draft
            .prepare(&current.connections)
            .map_err(SaveError::Invalid)?;

        // 1. A typed key, in a slot nobody else can be using.
        let fresh = match &prepared.key {
            KeyChange::Set(key) => Some(self.store_new_key(&prepared.connection.id, key)?),
            _ => None,
        };

        // 2. The file, re-checked as it is now. Only file work happens under the lock.
        let result = self.shared.update(
            |file| -> Result<(ConnectionId, Option<SecretRef>), SaveError> {
                let mut again = draft
                    .prepare(&file.connections)
                    .map_err(SaveError::Invalid)?;
                if let (Some(slot), Auth::ApiKey { secret }) = (&fresh, &mut again.connection.auth)
                {
                    *secret = slot.clone();
                }
                let id = again.connection.id.clone();
                let new_ref = again.connection.auth.secret_ref().cloned();
                let old_ref = match file.connections.iter_mut().find(|c| c.id == id) {
                    Some(slot) => {
                        let old = slot.auth.secret_ref().cloned();
                        *slot = again.connection;
                        old
                    }
                    None => {
                        file.connections.push(again.connection);
                        None
                    }
                };
                let retired = old_ref.filter(|r| {
                    Some(r) != new_ref.as_ref()
                        && !file
                            .connections
                            .iter()
                            .any(|c| c.auth.secret_ref() == Some(r))
                });
                Ok((id, retired))
            },
        );
        let outcome = match result {
            Ok(done) => done,
            Err(e) => Err(SaveError::Failed(e.into())),
        };

        // 3. The file did not change, so the slot just written is unused: drop it. It is ours
        // alone, so nothing else can be affected.
        let (id, retired) = match outcome {
            Ok(done) => done,
            Err(e) => {
                if let Some(slot) = &fresh {
                    let _ = self.secrets.delete(slot);
                }
                return Err(e);
            }
        };

        // 4. The file now points at the new key. A key the connection no longer uses can go.
        let warning = retired
            .filter(|old| self.secrets.delete(old).is_err())
            .map(|_| "The old key could not be removed from the keychain.".to_owned());
        Ok(Saved { id, warning })
    }

    /// Writes `key` to a slot no other save can be using and returns where it went.
    fn store_new_key(&self, id: &ConnectionId, key: &SecretString) -> Result<SecretRef, SaveError> {
        let mut slot = slot::fresh(id).map_err(SaveError::Failed)?;
        for _ in 0..4 {
            if !matches!(self.secrets.get(&slot), Ok(Some(_))) {
                break;
            }
            slot = slot::fresh(id).map_err(SaveError::Failed)?;
        }
        if self.secrets.set(&slot, key).is_err() {
            let _ = self.secrets.delete(&slot);
            return Err(SaveError::Failed(key_not_saved(&slot)));
        }
        Ok(slot)
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
        secret: Option<craft_ai_core::SecretString>,
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

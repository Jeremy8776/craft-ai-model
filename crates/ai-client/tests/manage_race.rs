//! Saving connections while something else is also saving, or while the disk refuses the write.
//!
//! The invariants: a save that reports success has its own key stored under its own locator; a
//! save that fails leaves every key that a saved connection uses exactly as it was; no key slot is
//! ever written by two saves; and the shared file's lock is never held while the keychain is
//! asked for anything (a keychain can block on a prompt or a daemon, and every other app waits on
//! that lock). These run against real files and an in-memory secret store.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use craft_ai_client::{ConnectionManager, SaveError};
use craft_ai_core::{AiError, ConnectionDraft, PRESETS, SecretRef, SecretString};
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;

fn openai(key: &str) -> ConnectionDraft {
    let mut d = ConnectionDraft::from_preset(PRESETS.iter().find(|p| p.key == "openai").unwrap());
    d.key = key.to_owned();
    d
}

type Hookfn = Box<dyn FnOnce() + Send>;

/// A secret store that records what it holds and can run a closure at the start of the first
/// write (to look at the world while the keychain is busy) or right after it (to stage a disk
/// fault).
struct Hook {
    inner: MemoryStore,
    live: Mutex<BTreeSet<String>>,
    sets: Mutex<Vec<String>>,
    before_set: Mutex<Option<Hookfn>>,
    after_set: Mutex<Option<Hookfn>>,
    fail_delete: AtomicBool,
}

impl Hook {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MemoryStore::new(),
            live: Mutex::new(BTreeSet::new()),
            sets: Mutex::new(Vec::new()),
            before_set: Mutex::new(None),
            after_set: Mutex::new(None),
            fail_delete: AtomicBool::new(false),
        })
    }

    fn value(&self, r: &SecretRef) -> Option<String> {
        self.inner.get(r).unwrap().map(|s| s.expose().to_owned())
    }

    fn live(&self) -> BTreeSet<String> {
        self.live.lock().unwrap().clone()
    }
}

impl SecretStore for Hook {
    fn name(&self) -> &'static str {
        "hook"
    }
    fn get(&self, key: &SecretRef) -> Result<Option<SecretString>, AiError> {
        self.inner.get(key)
    }
    fn set(&self, key: &SecretRef, value: &SecretString) -> Result<(), AiError> {
        let before = self.before_set.lock().unwrap().take();
        if let Some(f) = before {
            f();
        }
        self.inner.set(key, value)?;
        self.live.lock().unwrap().insert(key.as_str().to_owned());
        self.sets.lock().unwrap().push(key.as_str().to_owned());
        let after = self.after_set.lock().unwrap().take();
        if let Some(f) = after {
            f();
        }
        Ok(())
    }
    fn delete(&self, key: &SecretRef) -> Result<(), AiError> {
        if self.fail_delete.load(Ordering::SeqCst) {
            return Err(AiError::invalid("the keychain refused"));
        }
        self.live.lock().unwrap().remove(key.as_str());
        self.inner.delete(key)
    }
}

fn rig() -> (tempfile::TempDir, Arc<Hook>, ConnectionManager) {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let hook = Hook::new();
    let manager = ConnectionManager::new(shared, hook.clone());
    (dir, hook, manager)
}

fn key_ref(manager: &ConnectionManager, n: usize) -> SecretRef {
    let views = manager.views().unwrap();
    views[n].connection.auth.secret_ref().unwrap().clone()
}

/// Several people saving the same kind of connection at the same moment. Every save that says it
/// worked must find its own key under its own connection afterwards, and the keychain must hold
/// nothing but those keys. Run many rounds: the fault this guards against (a loser deleting the
/// winner's key) showed up in about one run in six.
#[test]
fn saves_at_the_same_moment_never_lose_swap_or_strand_a_key() {
    for round in 0..40 {
        let (_dir, hook, manager) = rig();
        let handles: Vec<_> = (0..6)
            .map(|n| {
                let m = manager.clone();
                std::thread::spawn(move || (n, m.save(&openai(&format!("key-{n}")))))
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let views = manager.views().unwrap();
        let won: Vec<_> = results.iter().filter(|(_, r)| r.is_ok()).collect();
        assert!(!won.is_empty(), "round {round}: someone must win");
        assert_eq!(
            views.len(),
            won.len(),
            "round {round}: every success is on disk"
        );
        for (n, saved) in won {
            let id = &saved.as_ref().unwrap().id;
            let view = views.iter().find(|v| &v.connection.id == id).unwrap();
            let r = view.connection.auth.secret_ref().unwrap();
            assert_eq!(
                hook.value(r).expect("a saved connection has its key"),
                format!("key-{n}"),
                "round {round}: connection {id} holds someone else's key"
            );
        }
        let refs: BTreeSet<String> = views
            .iter()
            .map(|v| v.connection.auth.secret_ref().unwrap().as_str().to_owned())
            .collect();
        assert_eq!(refs.len(), views.len(), "round {round}: no shared locator");
        assert_eq!(
            hook.live(),
            refs,
            "round {round}: the keychain holds exactly the saved connections' keys"
        );
        for e in results.into_iter().filter_map(|(_, r)| r.err()) {
            assert!(!e.to_string().is_empty());
        }
    }
}

/// While the keychain is being written, another process must be able to take the file lock.
#[test]
fn the_file_lock_is_free_while_the_keychain_is_written() {
    let (dir, hook, manager) = rig();
    let path = dir.path().join("connections.json");
    let freed = Arc::new(AtomicBool::new(false));
    let seen = freed.clone();
    *hook.before_set.lock().unwrap() = Some(Box::new(move || {
        let (tx, rx) = mpsc::channel();
        let other = SharedStore::new(path);
        std::thread::spawn(move || {
            let _ = tx.send(other.update(|_| ()).is_ok());
        });
        seen.store(
            rx.recv_timeout(Duration::from_secs(2)) == Ok(true),
            Ordering::SeqCst,
        );
    }));
    manager.save(&openai("a-key")).unwrap();
    assert!(
        freed.load(Ordering::SeqCst),
        "the file lock was held across the keychain write"
    );
}

/// Replacing a key writes a new slot and only then retires the old one, so the key a connection
/// points at is never half-replaced.
#[test]
fn replacing_a_key_never_overwrites_the_old_slot_and_retires_it_after() {
    let (_dir, hook, manager) = rig();
    manager.save(&openai("original-key")).unwrap();
    let old = key_ref(&manager, 0);

    let mut edit = ConnectionDraft::from_connection(&manager.views().unwrap()[0].connection);
    edit.key = "replacement-key".into();
    let during = Arc::new(Mutex::new(None));
    let (h, o, d) = (hook.clone(), old.clone(), during.clone());
    *hook.before_set.lock().unwrap() = Some(Box::new(move || {
        *d.lock().unwrap() = Some(h.value(&o));
    }));
    manager.save(&edit).unwrap();

    assert_eq!(
        during.lock().unwrap().clone().flatten().as_deref(),
        Some("original-key"),
        "the old key was still in place when the new one was written"
    );
    let new = key_ref(&manager, 0);
    assert_ne!(new, old, "the new key has its own slot");
    assert_eq!(hook.value(&new).as_deref(), Some("replacement-key"));
    assert_eq!(hook.value(&old), None, "the retired slot is gone");
    assert_eq!(hook.live().len(), 1);
}

/// A save that does not change the key does not touch the keychain.
#[test]
fn a_save_that_keeps_the_key_does_not_write_to_the_keychain() {
    let (_dir, hook, manager) = rig();
    manager.save(&openai("original-key")).unwrap();
    let writes = hook.sets.lock().unwrap().len();
    let mut edit = ConnectionDraft::from_connection(&manager.views().unwrap()[0].connection);
    edit.label = "Renamed".into();
    manager.save(&edit).unwrap();
    assert_eq!(hook.sets.lock().unwrap().len(), writes);
    assert_eq!(
        hook.value(&key_ref(&manager, 0)).as_deref(),
        Some("original-key")
    );
}

/// Makes the next write of the connections file fail: the writer's temporary file name is taken
/// by a folder.
fn block_next_write(dir: &std::path::Path) {
    let tmp = dir.join(format!(".connections.json.tmp-{}", std::process::id()));
    std::fs::create_dir(tmp).unwrap();
}

fn unblock(dir: &std::path::Path) {
    let tmp = dir.join(format!(".connections.json.tmp-{}", std::process::id()));
    let _ = std::fs::remove_dir(tmp);
}

#[test]
fn a_write_that_fails_after_the_key_leaves_the_old_key_untouched_and_drops_the_new_slot() {
    let (dir, hook, manager) = rig();
    manager.save(&openai("original-key")).unwrap();
    let old = key_ref(&manager, 0);

    let mut edit = ConnectionDraft::from_connection(&manager.views().unwrap()[0].connection);
    edit.key = "replacement-key".into();
    let path = dir.path().to_owned();
    *hook.after_set.lock().unwrap() = Some(Box::new(move || block_next_write(&path)));
    let err = manager.save(&edit).unwrap_err();
    assert!(matches!(err, SaveError::Failed(_)), "{err}");
    assert_eq!(hook.value(&old).as_deref(), Some("original-key"));
    assert_eq!(hook.live(), BTreeSet::from([old.as_str().to_owned()]));
    unblock(dir.path());
    assert_eq!(
        key_ref(&manager, 0),
        old,
        "the connection still points at it"
    );
}

#[test]
fn a_new_connection_whose_write_fails_leaves_no_orphan_key() {
    let (dir, hook, manager) = rig();
    let path = dir.path().to_owned();
    *hook.after_set.lock().unwrap() = Some(Box::new(move || block_next_write(&path)));
    let err = manager.save(&openai("fresh-key")).unwrap_err();
    assert!(matches!(err, SaveError::Failed(_)), "{err}");
    assert!(hook.live().is_empty(), "no orphan key");
    unblock(dir.path());
    assert!(manager.views().unwrap().is_empty(), "nothing was saved");
}

/// The file now points at the new key; failing to remove the retired one is a warning, not an
/// error, and loses nothing.
#[test]
fn a_retired_key_that_cannot_be_removed_is_a_warning() {
    let (_dir, hook, manager) = rig();
    manager.save(&openai("original-key")).unwrap();
    let old = key_ref(&manager, 0);
    let mut edit = ConnectionDraft::from_connection(&manager.views().unwrap()[0].connection);
    edit.key = "replacement-key".into();
    hook.fail_delete.store(true, Ordering::SeqCst);
    let saved = manager.save(&edit).unwrap();
    assert!(saved.warning.is_some());
    let new = key_ref(&manager, 0);
    assert_ne!(new, old);
    assert_eq!(hook.value(&new).as_deref(), Some("replacement-key"));
}

/// Two connections from an older file can share one locator. Replacing the key of one must not
/// retire the slot the other still reads.
#[test]
fn replacing_a_key_keeps_a_slot_another_connection_still_uses() {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let hook = Hook::new();
    let manager = ConnectionManager::new(shared.clone(), hook.clone());
    manager.save(&openai("shared-key")).unwrap();
    let old = key_ref(&manager, 0);
    shared
        .update(|f| {
            let mut twin = f.connections[0].clone();
            twin.id = craft_ai_core::ConnectionId::new("twin").unwrap();
            f.connections.push(twin);
        })
        .unwrap();

    let mut edit = ConnectionDraft::from_connection(&manager.views().unwrap()[0].connection);
    edit.key = "replacement-key".into();
    let saved = manager.save(&edit).unwrap();
    assert!(saved.warning.is_none());
    assert_eq!(hook.value(&old).as_deref(), Some("shared-key"));
    assert_ne!(key_ref(&manager, 0), old);
    assert_eq!(key_ref(&manager, 1), old, "the twin still points at it");
}

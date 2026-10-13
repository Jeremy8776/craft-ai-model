//! Saving connections while something else is also saving, or while the disk refuses the write.
//!
//! The invariant: a save that reports success has its own key stored under its own locator, a
//! save that fails leaves the key store as it was, and no save ever deletes or overwrites a key
//! that a saved connection uses. These run against real files and an in-memory secret store.

use std::sync::{Arc, Mutex};

use craft_ai_client::{ConnectionManager, SaveError};
use craft_ai_model::{AiError, ConnectionDraft, PRESETS, SecretRef, SecretString};
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;

fn openai(key: &str) -> ConnectionDraft {
    let mut d = ConnectionDraft::from_preset(PRESETS.iter().find(|p| p.key == "openai").unwrap());
    d.key = key.to_owned();
    d
}

fn secret_ref(s: &str) -> SecretRef {
    SecretRef::new(s).unwrap()
}

/// Several people saving the same kind of connection at the same moment. Every save that says it
/// worked must find its own key under its own connection afterwards. Run many rounds: the fault
/// this guards against (a loser deleting the winner's key) showed up in about one run in six.
#[test]
fn saves_at_the_same_moment_never_lose_or_swap_a_key() {
    for round in 0..40 {
        let dir = tempfile::tempdir().unwrap();
        let shared = SharedStore::new(dir.path().join("connections.json"));
        let secrets = Arc::new(MemoryStore::new());
        let manager = ConnectionManager::new(shared, secrets.clone());
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
            let key = secrets
                .get(r)
                .unwrap()
                .expect("a saved connection has its key");
            assert_eq!(
                key.expose(),
                format!("key-{n}"),
                "round {round}: connection {id} holds someone else's key"
            );
        }
        let mut refs: Vec<_> = views
            .iter()
            .map(|v| v.connection.auth.secret_ref().unwrap().clone())
            .collect();
        refs.sort();
        refs.dedup();
        assert_eq!(refs.len(), views.len(), "round {round}: no shared locator");
        for e in results.into_iter().filter_map(|(_, r)| r.err()) {
            assert!(!e.to_string().is_empty());
        }
    }
}

/// A secret store that runs a closure right after the first write, to stage a disk fault.
struct Hook {
    inner: MemoryStore,
    after_set: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl SecretStore for Hook {
    fn name(&self) -> &'static str {
        "hook"
    }
    fn get(&self, key: &SecretRef) -> Result<Option<SecretString>, AiError> {
        self.inner.get(key)
    }
    fn set(&self, key: &SecretRef, value: &SecretString) -> Result<(), AiError> {
        self.inner.set(key, value)?;
        if let Some(f) = self.after_set.lock().unwrap().take() {
            f();
        }
        Ok(())
    }
    fn delete(&self, key: &SecretRef) -> Result<(), AiError> {
        self.inner.delete(key)
    }
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

fn rig_with_hook() -> (tempfile::TempDir, Arc<Hook>, ConnectionManager) {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let hook = Arc::new(Hook {
        inner: MemoryStore::new(),
        after_set: Mutex::new(None),
    });
    let manager = ConnectionManager::new(shared, hook.clone());
    (dir, hook, manager)
}

#[test]
fn a_write_that_fails_after_the_key_puts_the_old_key_back() {
    let (dir, hook, manager) = rig_with_hook();
    manager.save(&openai("original-key")).unwrap();
    let view = manager.views().unwrap().remove(0);

    let mut edit = ConnectionDraft::from_connection(&view.connection);
    edit.key = "replacement-key".into();
    let path = dir.path().to_owned();
    *hook.after_set.lock().unwrap() = Some(Box::new(move || block_next_write(&path)));
    let err = manager.save(&edit).unwrap_err();
    assert!(matches!(err, SaveError::Failed(_)), "{err}");
    let kept = hook.inner.get(&secret_ref("openai.key")).unwrap().unwrap();
    assert_eq!(kept.expose(), "original-key", "the old key was restored");
    unblock(dir.path());
}

#[test]
fn a_new_connection_whose_write_fails_leaves_no_orphan_key() {
    let (dir, hook, manager) = rig_with_hook();
    let path = dir.path().to_owned();
    *hook.after_set.lock().unwrap() = Some(Box::new(move || block_next_write(&path)));
    let err = manager.save(&openai("fresh-key")).unwrap_err();
    assert!(matches!(err, SaveError::Failed(_)), "{err}");
    assert!(
        hook.inner.get(&secret_ref("openai.key")).unwrap().is_none(),
        "no orphan key"
    );
    unblock(dir.path());
    assert!(manager.views().unwrap().is_empty(), "nothing was saved");
}

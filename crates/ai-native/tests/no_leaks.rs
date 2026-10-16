//! The end-to-end promise: a key saved through the launcher path never lands in the shared store
//! file, never in a Debug dump or an error, and is found again by an app that only has the
//! connection.

use craft_ai_core::{AiError, Connection, ErrorKind, SecretRef, SecretString};
use craft_ai_native::secrets::{
    EncryptedFileStore, EnvStore, KdfParams, LayeredStore, MemoryStore, SecretStore, secret_for,
};
use craft_ai_native::shared::SharedStore;

const KEY_TEXT: &str = "sk-live-needle-0123456789";

fn api_key_connection() -> Connection {
    serde_json::from_value(serde_json::json!({
        "id": "openai-main",
        "label": "OpenAI",
        "provider": "openai",
        "endpoint": "https://api.openai.com/v1",
        "locality": "remote",
        "auth": {"kind": "api_key", "secret": "openai-main-key"},
        "billing": "metered",
        "models": []
    }))
    .unwrap()
}

fn local_connection() -> Connection {
    serde_json::from_value(serde_json::json!({
        "id": "ollama",
        "label": "Ollama",
        "provider": "openai",
        "endpoint": "http://127.0.0.1:11434/v1",
        "locality": "local",
        "auth": {"kind": "none"},
        "billing": "free",
        "models": []
    }))
    .unwrap()
}

fn fast_file_store(dir: &std::path::Path) -> EncryptedFileStore {
    EncryptedFileStore::with_params(
        dir.join("secrets.enc.json"),
        SecretString::new("passphrase-needle"),
        KdfParams::new(64, 1, 1).unwrap(),
    )
}

#[test]
fn launcher_saves_and_app_reads_without_the_key_touching_the_shared_file() {
    let dir = tempfile::tempdir().unwrap();
    let conn = api_key_connection();
    let secret_ref = conn.auth.secret_ref().unwrap().clone();

    // Launcher side: save the key, then record the connection.
    let secrets = LayeredStore::new(vec![
        Box::new(EnvStore::with_lookup(|_| None)),
        Box::new(fast_file_store(dir.path())),
    ]);
    secrets
        .set(&secret_ref, &SecretString::new(KEY_TEXT))
        .unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    shared
        .update(|st| st.connections.push(conn.clone()))
        .unwrap();

    // App side: only the shared file and the secret store.
    let loaded = shared.load().unwrap();
    let found = &loaded.connections[0];
    let key = secret_for(&secrets, found).unwrap().unwrap();
    assert_eq!(key.expose(), KEY_TEXT);

    // Nothing on disk except the encrypted file may contain the key or the passphrase.
    for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
        let bytes = std::fs::read(entry.path()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("needle"), "{:?} leaks", entry.path());
    }
}

#[test]
fn connection_without_credentials_needs_no_secret() {
    let store = MemoryStore::new();
    assert!(secret_for(&store, &local_connection()).unwrap().is_none());
}

#[test]
fn missing_key_is_an_auth_error_that_names_the_connection_not_the_key() {
    let store = MemoryStore::new();
    let err = secret_for(&store, &api_key_connection()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Auth);
    assert!(err.message.contains("OpenAI"));
    assert!(err.message.contains("Launcher"));

    // An empty stored value counts as missing.
    let secret_ref = SecretRef::new("openai-main-key").unwrap();
    store.set(&secret_ref, &SecretString::new("")).unwrap();
    assert!(secret_for(&store, &api_key_connection()).is_err());
}

#[test]
fn debug_and_error_text_never_show_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let file = fast_file_store(dir.path());
    let secret_ref = SecretRef::new("openai-main-key").unwrap();
    file.set(&secret_ref, &SecretString::new(KEY_TEXT)).unwrap();

    let layered = LayeredStore::new(vec![Box::new(EnvStore::new()), Box::new(file)]);
    let key = layered.get(&secret_ref).unwrap().unwrap();
    let mut texts = vec![format!("{key:?}"), format!("{key}"), format!("{layered:?}")];

    // A wrong passphrase gives an error that must not echo anything sensitive.
    let wrong = EncryptedFileStore::with_params(
        dir.path().join("secrets.enc.json"),
        SecretString::new("wrong-needle"),
        KdfParams::new(64, 1, 1).unwrap(),
    );
    let err: AiError = wrong.get(&secret_ref).unwrap_err();
    texts.push(format!("{err}"));
    texts.push(format!("{err:?}"));
    texts.push(format!("{wrong:?}"));
    for t in texts {
        assert!(!t.contains("needle"), "{t}");
    }
}

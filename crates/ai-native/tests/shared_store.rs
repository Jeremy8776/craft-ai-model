//! Behaviour of the shared store file: persistence, concurrent writers, version refusal and
//! hostile input. Uses real files in a temp directory.

use craft_ai_core::store::{MAX_STORE_BYTES, StoreError};
use craft_ai_core::{Connection, StoreFile};
use craft_ai_native::shared::{SharedError, SharedStore};

fn connection(id: &str) -> Connection {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "label": format!("Connection {id}"),
        "provider": "openai",
        "endpoint": "https://api.example.com/v1",
        "locality": "remote",
        "auth": {"kind": "api_key", "secret": format!("{id}-key")},
        "billing": "metered",
        "models": []
    }))
    .unwrap()
}

fn store_at(dir: &tempfile::TempDir) -> SharedStore {
    SharedStore::new(dir.path().join("sub").join("connections.json"))
}

#[test]
fn missing_file_is_an_empty_store_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let s = store_at(&dir);
    assert_eq!(s.load().unwrap(), StoreFile::default());
    assert!(s.load_existing().unwrap().is_none());
    assert!(!s.path().exists());
}

#[test]
fn update_persists_and_a_second_handle_sees_it() {
    let dir = tempfile::tempdir().unwrap();
    let s = store_at(&dir);
    let n = s
        .update(|st| {
            st.connections.push(connection("a"));
            st.connections.len()
        })
        .unwrap();
    assert_eq!(n, 1);
    let other = SharedStore::new(s.path());
    assert_eq!(other.load().unwrap().connections.len(), 1);
}

#[test]
fn concurrent_writers_all_land() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("connections.json");
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let path = path.clone();
            std::thread::spawn(move || {
                SharedStore::new(path)
                    .update(|st| st.connections.push(connection(&format!("c{i}"))))
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap().unwrap();
    }
    assert_eq!(SharedStore::new(&path).load().unwrap().connections.len(), 8);
}

#[test]
fn newer_schema_is_refused_for_reads_and_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let s = store_at(&dir);
    std::fs::create_dir_all(s.path().parent().unwrap()).unwrap();
    let newer = r#"{"schema_version": 99, "something_new": [1,2,3]}"#;
    std::fs::write(s.path(), newer).unwrap();
    assert!(matches!(
        s.load(),
        Err(SharedError::Store(StoreError::TooNew { found: 99, .. }))
    ));
    assert!(s.update(|st| st.connections.push(connection("a"))).is_err());
    assert!(s.replace(&StoreFile::default()).is_err());
    assert_eq!(std::fs::read_to_string(s.path()).unwrap(), newer);
}

#[test]
fn unknown_top_level_fields_survive_a_rewrite() {
    let dir = tempfile::tempdir().unwrap();
    let s = store_at(&dir);
    std::fs::create_dir_all(s.path().parent().unwrap()).unwrap();
    std::fs::write(
        s.path(),
        r#"{"schema_version": 1, "launcher_note": {"keep": true}}"#,
    )
    .unwrap();
    s.update(|st| st.connections.push(connection("a"))).unwrap();
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(s.path()).unwrap()).unwrap();
    assert_eq!(raw["launcher_note"]["keep"], true);
    assert_eq!(raw["connections"].as_array().unwrap().len(), 1);
}

#[test]
fn an_invalid_change_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let s = store_at(&dir);
    s.update(|st| st.connections.push(connection("a"))).unwrap();
    let before = std::fs::read_to_string(s.path()).unwrap();
    let result = s.update(|st| st.connections.push(connection("a")));
    assert!(matches!(
        result,
        Err(SharedError::Store(StoreError::DuplicateConnection(_)))
    ));
    assert_eq!(std::fs::read_to_string(s.path()).unwrap(), before);
}

#[test]
fn corrupt_file_blocks_update_but_replace_recovers_and_keeps_a_copy() {
    let dir = tempfile::tempdir().unwrap();
    let s = store_at(&dir);
    std::fs::create_dir_all(s.path().parent().unwrap()).unwrap();
    std::fs::write(s.path(), "{ truncated").unwrap();
    assert!(s.load().is_err());
    assert!(s.update(|_| ()).is_err());
    assert_eq!(std::fs::read_to_string(s.path()).unwrap(), "{ truncated");

    s.replace(&StoreFile::default()).unwrap();
    assert!(s.load().is_ok());
    let backup = format!("{}.corrupt", s.path().display());
    assert_eq!(std::fs::read_to_string(backup).unwrap(), "{ truncated");
}

#[test]
fn oversize_and_non_text_files_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    let s = store_at(&dir);
    std::fs::create_dir_all(s.path().parent().unwrap()).unwrap();
    std::fs::write(s.path(), vec![b' '; MAX_STORE_BYTES + 1]).unwrap();
    assert!(matches!(
        s.load(),
        Err(SharedError::Store(StoreError::TooLarge { .. }))
    ));
    std::fs::write(s.path(), [0xff, 0xfe, 0x00, 0x80]).unwrap();
    assert!(s.load().is_err());
}

#[test]
fn truncating_a_valid_file_anywhere_never_panics() {
    let dir = tempfile::tempdir().unwrap();
    let s = store_at(&dir);
    s.update(|st| st.connections.push(connection("a"))).unwrap();
    let good = std::fs::read(s.path()).unwrap();
    for cut in 0..good.len() {
        std::fs::write(s.path(), &good[..cut]).unwrap();
        // Any prefix short of the whole document is an error, never a panic or a half-store.
        assert!(s.load().is_err(), "prefix of {cut} bytes parsed");
    }
}

#[test]
fn shared_error_maps_to_ai_error_kinds() {
    use craft_ai_core::{AiError, ErrorKind};
    let too_new: AiError = SharedError::Store(StoreError::TooNew {
        found: 2,
        supported: 1,
    })
    .into();
    assert_eq!(too_new.kind, ErrorKind::Unsupported);
    let other: AiError = SharedError::NoLocation.into();
    assert_eq!(other.kind, ErrorKind::Internal);
}

#[test]
fn deleting_a_custom_tool_does_not_resurrect_it_as_an_unknown_field() {
    use craft_ai_core::{CapabilityId, InputRole, Role, RoleId};
    let dir = tempfile::tempdir().unwrap();
    let s = store_at(&dir);
    s.update(|st| {
        st.custom_tools.push(Role {
            id: RoleId::new("describe-photo").unwrap(),
            label: "Describe photo".into(),
            capability: CapabilityId::parse("image.describe").unwrap(),
            inputs: vec![InputRole::Source],
        })
    })
    .unwrap();
    assert_eq!(s.load().unwrap().custom_tools.len(), 1);
    s.update(|st| st.custom_tools.clear()).unwrap();
    assert!(s.load().unwrap().custom_tools.is_empty());
    let raw = std::fs::read_to_string(s.path()).unwrap();
    assert!(!raw.contains("describe-photo"));
}

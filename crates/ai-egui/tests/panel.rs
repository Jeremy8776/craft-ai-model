//! The add-connection panel driven like a person would: pick, type, test, save. A scripted
//! localhost server stands in for the model server; the secret store is in memory.

#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use craft_ai_client::ConnectionManager;
use craft_ai_core::ConnectionId;
use craft_ai_egui::{ConnectionPanel, PanelEvent};
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use support::{Reply, Server};

const KEY: &str = "sk-panel-key-777";

struct State {
    panel: ConnectionPanel,
    event: Option<PanelEvent>,
}

struct Rig {
    h: Harness<'static, State>,
    shared: SharedStore,
    secrets: Arc<MemoryStore>,
    _dir: tempfile::TempDir,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    let secrets = Arc::new(MemoryStore::new());
    let manager = ConnectionManager::new(shared.clone(), secrets.clone());
    let h = Harness::new_ui_state(
        |ui, s: &mut State| {
            if let Some(e) = s.panel.show(ui) {
                s.event = Some(e);
            }
        },
        State {
            panel: ConnectionPanel::new(manager),
            event: None,
        },
    );
    Rig {
        h,
        shared,
        secrets,
        _dir: dir,
    }
}

fn models_server() -> Server {
    Server::start(|_| {
        Reply::Json(
            200,
            r#"{"object":"list","data":[{"id":"gpt-4o"},{"id":"gpt-image-1"}]}"#.into(),
        )
    })
}

fn type_into(h: &mut Harness<'_, State>, role: Role, nth: usize, text: &str) {
    let node = h.get_all_by_role(role).nth(nth).expect("field");
    node.focus();
    node.type_text(text);
    h.run();
}

/// Replaces what is in a text box.
fn replace_text(h: &mut Harness<'_, State>, nth: usize, text: &str) {
    h.get_all_by_role(Role::TextInput)
        .nth(nth)
        .expect("field")
        .focus();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run();
    h.get_all_by_role(Role::TextInput)
        .nth(nth)
        .expect("field")
        .type_text(text);
    h.run();
}

fn wait_until(h: &mut Harness<'_, State>, label: &str) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        h.step();
        if h.query_by_label_contains(label).is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("never saw {label:?}");
}

#[test]
fn a_remote_connection_is_saved_with_its_key_in_the_secret_store_only() {
    let mut r = rig();
    r.h.run();
    r.h.get_by_label("Data sent to this connection leaves this computer.");
    type_into(&mut r.h, Role::PasswordInput, 0, KEY);
    r.h.get_by_label("Save").click();
    r.h.run();
    assert_eq!(
        r.h.state().event,
        Some(PanelEvent::Saved(ConnectionId::new("openai").unwrap()))
    );
    let text = std::fs::read_to_string(r.shared.path()).unwrap();
    let loc = r.shared.load().unwrap().connections[0]
        .auth
        .secret_ref()
        .unwrap()
        .clone();
    assert!(loc.as_str().starts_with("openai.key"), "{loc:?}");
    assert!(text.contains(loc.as_str()) && !text.contains(KEY));
    let stored = r.secrets.get(&loc).unwrap().unwrap();
    assert_eq!(stored.expose(), KEY);
}

#[test]
fn a_form_problem_stays_on_screen_and_saves_nothing() {
    let mut r = rig();
    r.h.run();
    r.h.get_by_label("Save").click();
    r.h.run();
    r.h.get_by_label_contains("Paste the key");
    assert!(r.h.state().event.is_none());
    assert!(r.shared.load().unwrap().connections.is_empty());
    // A key with a space in it is refused, and the message does not repeat it.
    type_into(&mut r.h, Role::PasswordInput, 0, "bad key");
    r.h.get_by_label("Save").click();
    r.h.run();
    r.h.get_by_label_contains("no spaces");
    assert!(r.h.query_by_label_contains("bad key").is_none());
}

#[test]
fn testing_lists_models_before_anything_is_saved() {
    let server = models_server();
    let mut r = rig();
    r.h.run();
    r.h.get_by_role(Role::ComboBox).click();
    r.h.run();
    r.h.get_by_label("Ollama").click();
    r.h.run();
    r.h.get_by_label("Data sent to this connection stays on this computer.");
    replace_text(&mut r.h, 1, &server.url);
    r.h.get_by_label("Test").click();
    // A running test keeps asking for repaints, so `run()` would never settle.
    r.h.step();
    wait_until(&mut r.h, "Connected. Models found: 2");
    assert_eq!(server.count(), 1);
    assert!(
        r.shared.load().unwrap().connections.is_empty(),
        "a test saves nothing"
    );
    // The models it found travel with the saved connection.
    r.h.get_by_label("Save").click();
    r.h.run();
    assert!(matches!(r.h.state().event, Some(PanelEvent::Saved(_))));
    assert_eq!(r.shared.load().unwrap().connections[0].models.len(), 2);
}

#[test]
fn a_dead_server_is_reported_and_editing_the_address_clears_the_old_result() {
    let mut r = rig();
    r.h.run();
    r.h.get_by_role(Role::ComboBox).click();
    r.h.run();
    r.h.get_by_label("Ollama").click();
    r.h.run();
    replace_text(&mut r.h, 1, &Server::dead_url());
    r.h.get_by_label("Test").click();
    // A running test keeps asking for repaints, so `run()` would never settle.
    r.h.step();
    wait_until(&mut r.h, "Test failed");
    replace_text(&mut r.h, 1, "http://127.0.0.1:9/v1");
    assert!(
        r.h.query_by_label_contains("Test failed").is_none(),
        "an old result must not describe a new address"
    );
}

#[test]
fn cancel_closes_without_saving() {
    let mut r = rig();
    r.h.run();
    r.h.get_by_label("Cancel").click();
    r.h.run();
    assert_eq!(r.h.state().event, Some(PanelEvent::Closed));
    assert!(r.shared.load().unwrap().connections.is_empty());
}

#[test]
fn models_found_at_one_address_are_not_saved_for_another() {
    let server = models_server();
    let mut r = rig();
    r.h.run();
    r.h.get_by_role(Role::ComboBox).click();
    r.h.run();
    r.h.get_by_label("Ollama").click();
    r.h.run();
    replace_text(&mut r.h, 1, &server.url);
    r.h.get_by_label("Test").click();
    r.h.step();
    wait_until(&mut r.h, "Connected. Models found: 2");
    // Point it somewhere else after the test, then save: the list belongs to the old address.
    replace_text(&mut r.h, 1, "http://127.0.0.1:9/v1");
    r.h.get_by_label("Save").click();
    r.h.run();
    assert!(matches!(r.h.state().event, Some(PanelEvent::Saved(_))));
    let saved = r.shared.load().unwrap();
    assert_eq!(
        saved.connections[0].endpoint.as_deref(),
        Some("http://127.0.0.1:9/v1")
    );
    assert!(saved.connections[0].models.is_empty());
}

/// Replaces what is in a text box while a test is running: the panel asks for a repaint every
/// frame then, so the `run()` in `replace_text` would never settle.
fn replace_text_mid_test(h: &mut Harness<'_, State>, nth: usize, text: &str) {
    h.get_all_by_role(Role::TextInput)
        .nth(nth)
        .expect("field")
        .focus();
    h.step();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.step();
    h.get_all_by_role(Role::TextInput)
        .nth(nth)
        .expect("field")
        .type_text(text);
    h.step();
}

/// Answers slowly, so the address can be edited while the answer is still on its way.
fn slow_models_server() -> Server {
    Server::start(|_| {
        std::thread::sleep(Duration::from_millis(600));
        Reply::Json(
            200,
            r#"{"object":"list","data":[{"id":"gpt-4o"},{"id":"gpt-image-1"}]}"#.into(),
        )
    })
}

#[test]
fn models_from_a_test_that_was_still_running_are_not_saved_for_the_new_address() {
    // The sibling test above edits the address *after* the answer is in. This is the other
    // order: the address is edited while the test is still running, so the answer arrives
    // describing an address the form no longer holds.
    let server = slow_models_server();
    let mut r = rig();
    r.h.run();
    r.h.get_by_role(Role::ComboBox).click();
    r.h.run();
    r.h.get_by_label("Ollama").click();
    r.h.run();
    replace_text(&mut r.h, 1, &server.url);
    r.h.get_by_label("Test").click();
    r.h.step();
    replace_text_mid_test(&mut r.h, 1, "http://127.0.0.1:9/v1");
    // Let the answer land and be handled.
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        r.h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(server.count(), 1, "the test did reach the server");
    assert!(
        r.h.query_by_label_contains("Connected. Models found")
            .is_none(),
        "an answer for an address the form no longer holds is not reported as this one's"
    );
    r.h.get_by_label("Save").click();
    r.h.run();
    assert!(matches!(r.h.state().event, Some(PanelEvent::Saved(_))));
    let saved = r.shared.load().unwrap();
    assert_eq!(
        saved.connections[0].endpoint.as_deref(),
        Some("http://127.0.0.1:9/v1")
    );
    assert!(
        saved.connections[0].models.is_empty(),
        "those models were found at the address that was replaced"
    );
}

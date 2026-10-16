//! Choosing a model for a tool and proving it does the job, against fake providers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use craft_ai_client::{AiClient, ProviderFactory, RoleStatus, SampleReport, TestStart, ToolDesk};
use craft_ai_core::job::{Event, Output};
use craft_ai_core::{
    AiError, Connection, ConnectionId, ErrorKind, JobId, JobSnapshot, JobState, ModelId, ModelInfo,
    Payload, Provider, Request, Role, SecretRef, SecretString, StoreFile,
};
use craft_ai_native::ConsentFile;
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use serde_json::json;

/// Answers every job at once with whatever its connection's id says.
struct Fake {
    how: String,
    jobs: Mutex<usize>,
}

impl Provider for Fake {
    fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(Vec::new())
    }
    fn submit(&self, _: &ModelId, _: &Request) -> Result<JobId, AiError> {
        let mut n = self.jobs.lock().unwrap();
        *n += 1;
        JobId::new(format!("job-{n}")).map_err(|e| AiError::invalid(e.to_string()))
    }
    fn poll(&self, _: &JobId) -> Result<JobSnapshot, AiError> {
        let output = |payload| {
            Event::Output(Output {
                payload,
                label: None,
            })
        };
        let mut snap = JobSnapshot::new(JobState::Succeeded);
        match self.how.as_str() {
            "good" => snap
                .events
                .push(output(Payload::inline("image/png", vec![1, 2, 3])?)),
            "texty" => snap.events.push(output(Payload::text("I cannot draw"))),
            "empty" => {}
            _ => {
                snap = JobSnapshot::new(JobState::Failed {
                    error: AiError::new(ErrorKind::Provider, "the service said no"),
                });
            }
        }
        Ok(snap)
    }
    fn cancel(&self, _: &JobId) -> Result<(), AiError> {
        Ok(())
    }
}

fn connection(id: &str, remote: bool, enabled: bool, models: &[&str]) -> Connection {
    let models: Vec<_> = models
        .iter()
        .filter_map(|m| craft_ai_openai::catalog::classify(m))
        .collect();
    serde_json::from_value(json!({
        "id": id, "label": format!("Connection {id}"), "provider": "fake-image",
        "endpoint": if remote { "https://api.example.com/v1" } else { "http://127.0.0.1:9/v1" },
        "locality": if remote { "remote" } else { "local" },
        "auth": if remote { json!({"kind":"api_key","secret": format!("{id}-key")}) } else { json!({"kind":"none"}) },
        "billing": if remote { "metered" } else { "free" },
        "enabled": enabled,
        "models": models,
    }))
    .unwrap()
}

struct World {
    _dir: tempfile::TempDir,
    shared: SharedStore,
    desk: ToolDesk,
    consent: ConsentFile,
    /// Providers built so far, per world, so parallel tests cannot disturb each other's count.
    builds: Arc<AtomicUsize>,
}

fn world(connections: Vec<Connection>) -> World {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    shared
        .replace(&StoreFile {
            connections: connections.clone(),
            ..StoreFile::default()
        })
        .unwrap();
    let secrets = Arc::new(MemoryStore::new());
    for c in &connections {
        if let Some(r) = c.auth.secret_ref() {
            secrets
                .set(
                    &SecretRef::new(r.as_str()).unwrap(),
                    &SecretString::new("k"),
                )
                .unwrap();
        }
    }
    let builds = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&builds);
    let mut factory = ProviderFactory::standard();
    factory.register("fake-image", move |c, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        // `good-remote` and `good-local` both behave as `good`.
        let how = c.id.as_str().split('-').next().unwrap_or("").to_owned();
        Ok(Arc::new(Fake {
            how,
            jobs: Mutex::new(0),
        }))
    });
    let consent = ConsentFile::new(dir.path().join("consent.json"));
    let client =
        AiClient::new(ToolDesk::app().unwrap(), shared.clone(), secrets).with_factory(factory);
    let desk = ToolDesk::new(
        client,
        shared.clone(),
        Some(ConsentFile::new(dir.path().join("consent.json"))),
    );
    World {
        _dir: dir,
        shared,
        desk,
        consent,
        builds,
    }
}

fn tool(id: &str) -> Role {
    craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == id)
        .unwrap()
        .role()
        .unwrap()
}

fn sample(id: &str) -> Request {
    let f = craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == id)
        .unwrap();
    craft_ai_features::sample_request(&f).unwrap()
}

fn cid(s: &str) -> ConnectionId {
    ConnectionId::new(s).unwrap()
}

fn image_model(w: &World) -> ModelId {
    w.desk.view(&tool("generate-image")).unwrap().choices[0]
        .model
        .clone()
}

#[test]
fn only_models_that_can_do_the_job_on_connections_that_are_on_are_offered() {
    let w = world(vec![
        connection("good-local", false, true, &["gpt-image-1", "gpt-4o"]),
        connection("good-off", false, false, &["gpt-image-1"]),
        connection("textonly-local", false, true, &["gpt-4o"]),
    ]);
    let view = w.desk.view(&tool("generate-image")).unwrap();
    let offered: Vec<_> = view
        .choices
        .iter()
        .map(|c| (c.connection.as_str(), c.model.as_str()))
        .collect();
    assert_eq!(offered, [("good-local", "gpt-image-1")]);
    assert!(view.bound.is_none());
    assert!(matches!(view.status, RoleStatus::Unavailable { .. }));
    // A tool whose inputs no model takes has no choices at all.
    assert!(
        w.desk
            .view(&tool("generative-expand"))
            .unwrap()
            .choices
            .is_empty()
    );
}

#[test]
fn choosing_binds_the_tool_and_the_status_follows() {
    let w = world(vec![connection(
        "good-local",
        false,
        true,
        &["gpt-image-1"],
    )]);
    let role = tool("generate-image");
    let model = image_model(&w);
    w.desk.choose(&role, &cid("good-local"), &model).unwrap();
    let view = w.desk.view(&role).unwrap();
    assert_eq!(view.bound, Some((cid("good-local"), model)));
    assert!(matches!(view.status, RoleStatus::Ready(_)));
    // The choice is the launcher's default, so it is in the shared store for apps to inherit.
    assert_eq!(w.shared.load().unwrap().defaults.len(), 1);
    w.desk.clear(&role.id).unwrap();
    assert!(w.desk.view(&role).unwrap().bound.is_none());
    assert!(w.shared.load().unwrap().defaults.is_empty());
}

#[test]
fn a_model_that_cannot_do_the_job_is_refused_and_nothing_is_written() {
    let w = world(vec![connection(
        "good-local",
        false,
        true,
        &["gpt-image-1", "gpt-4o"],
    )]);
    let err = w
        .desk
        .choose(
            &tool("generate-image"),
            &cid("good-local"),
            &ModelId::new("gpt-4o").unwrap(),
        )
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidRequest);
    assert!(w.shared.load().unwrap().defaults.is_empty());
    assert!(
        w.desk
            .choose(
                &tool("generate-image"),
                &cid("nope"),
                &ModelId::new("gpt-image-1").unwrap()
            )
            .is_err()
    );
}

#[test]
fn a_choice_that_stops_holding_says_why_and_stays_visible() {
    let w = world(vec![connection(
        "good-remote",
        true,
        true,
        &["gpt-image-1"],
    )]);
    let role = tool("generate-image");
    let model = image_model(&w);
    w.desk.choose(&role, &cid("good-remote"), &model).unwrap();
    // The connection is switched off afterwards.
    w.shared
        .update(|s| s.connections[0].enabled = false)
        .unwrap();
    let view = w.desk.view(&role).unwrap();
    assert!(view.bound.is_some());
    match view.status {
        RoleStatus::Unavailable { reason } => assert!(reason.contains("switched off"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

fn run(w: &World, id: &str, agreed: bool) -> Result<Option<SampleReport>, AiError> {
    Ok(match w.desk.start_test(&tool(id), sample(id), agreed)? {
        TestStart::Started(job) => Some(ToolDesk::finish(&tool(id), &job, Duration::from_secs(5))),
        TestStart::NeedsConsent { .. } => None,
    })
}

fn bound(w: &World, conn: &str) {
    let role = tool("generate-image");
    w.desk.choose(&role, &cid(conn), &image_model(w)).unwrap();
}

#[test]
fn a_local_sample_runs_at_once_and_passes_when_the_right_kind_of_thing_comes_back() {
    let w = world(vec![connection(
        "good-local",
        false,
        true,
        &["gpt-image-1"],
    )]);
    bound(&w, "good-local");
    match run(&w, "generate-image", false).unwrap().unwrap() {
        SampleReport::Passed { summary, .. } => assert!(summary.contains("image"), "{summary}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_remote_sample_waits_for_agreement_and_builds_nothing_until_then() {
    let w = world(vec![connection(
        "good-remote",
        true,
        true,
        &["gpt-image-1"],
    )]);
    bound(&w, "good-remote");
    let before = w.builds.load(Ordering::SeqCst);
    match w
        .desk
        .start_test(&tool("generate-image"), sample("generate-image"), false)
        .unwrap()
    {
        TestStart::NeedsConsent {
            disclosure,
            connection_label,
            billing,
        } => {
            assert_eq!(disclosure.destination, "https://api.example.com");
            assert_eq!(connection_label, "Connection good-remote");
            assert_eq!(billing, craft_ai_core::Billing::Metered);
        }
        TestStart::Started(_) => panic!("sent before the person agreed"),
    }
    assert_eq!(
        w.builds.load(Ordering::SeqCst),
        before,
        "no provider was even built"
    );
    assert!(!w.consent.path().exists(), "nothing recorded");
    // Agreeing runs it and records the agreement, so the next sample needs no question.
    assert!(matches!(
        run(&w, "generate-image", true).unwrap().unwrap(),
        SampleReport::Passed { .. }
    ));
    assert!(w.consent.path().exists());
    assert!(matches!(
        run(&w, "generate-image", false).unwrap().unwrap(),
        SampleReport::Passed { .. }
    ));
}

#[test]
fn a_model_that_answers_wrongly_or_not_at_all_fails_the_check() {
    for (conn, expect) in [
        ("texty-local", "other than image"),
        ("empty-local", "returned nothing"),
        ("broken-local", "the service said no"),
    ] {
        let w = world(vec![connection(conn, false, true, &["gpt-image-1"])]);
        bound(&w, conn);
        match run(&w, "generate-image", false).unwrap().unwrap() {
            SampleReport::Failed { message } => {
                assert!(message.contains(expect), "{conn}: {message}")
            }
            other => panic!("{conn}: {other:?}"),
        }
    }
}

#[test]
fn a_sample_with_no_choice_made_is_refused_before_anything_is_sent() {
    let w = world(vec![connection(
        "good-remote",
        true,
        true,
        &["gpt-image-1"],
    )]);
    let before = w.builds.load(Ordering::SeqCst);
    let err = run(&w, "generate-image", true).err().unwrap();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert_eq!(w.builds.load(Ordering::SeqCst), before);
}

#[test]
fn choosing_again_replaces_the_old_choice_rather_than_adding_to_it() {
    let w = world(vec![
        connection("good-local", false, true, &["gpt-image-1"]),
        connection("good-other", false, true, &["gpt-image-1"]),
    ]);
    let role = tool("generate-image");
    let model = image_model(&w);
    w.desk.choose(&role, &cid("good-local"), &model).unwrap();
    w.desk.choose(&role, &cid("good-other"), &model).unwrap();
    assert_eq!(
        w.shared.load().unwrap().defaults.len(),
        1,
        "one default per tool"
    );
    assert_eq!(
        w.desk.view(&role).unwrap().bound,
        Some((cid("good-other"), model))
    );
}

fn roles(ids: &[&str]) -> Vec<Role> {
    ids.iter().map(|i| tool(i)).collect()
}

#[test]
fn auto_bind_gives_an_unchosen_tool_the_model_that_is_connected() {
    let w = world(vec![connection(
        "good-local",
        false,
        true,
        &["gpt-image-1"],
    )]);
    let bound = w
        .desk
        .auto_bind(&roles(&["generate-image", "generate-video"]))
        .unwrap();
    let ids: Vec<_> = bound.iter().map(|r| r.as_str()).collect();
    assert_eq!(
        ids,
        ["generate-image"],
        "the video tool has nothing that can do it"
    );
    let view = w.desk.view(&tool("generate-image")).unwrap();
    assert_eq!(view.bound.map(|(c, _)| c), Some(cid("good-local")));
    assert!(matches!(view.status, RoleStatus::Ready(_)));
    assert!(
        w.desk
            .view(&tool("generate-video"))
            .unwrap()
            .bound
            .is_none()
    );
}

#[test]
fn auto_bind_prefers_this_computer_to_a_hosted_service() {
    let w = world(vec![
        connection("good-remote", true, true, &["gpt-image-1"]),
        connection("good-local", false, true, &["gpt-image-1"]),
    ]);
    w.desk.auto_bind(&roles(&["generate-image"])).unwrap();
    let bound = w.desk.view(&tool("generate-image")).unwrap().bound.unwrap();
    assert_eq!(
        bound.0,
        cid("good-local"),
        "nothing leaves the computer when it need not"
    );
}

#[test]
fn auto_bind_never_touches_a_choice_the_person_made() {
    let w = world(vec![
        connection("good-remote", true, true, &["gpt-image-1"]),
        connection("good-local", false, true, &["gpt-image-1"]),
    ]);
    let model = image_model(&w);
    w.desk
        .choose(&tool("generate-image"), &cid("good-remote"), &model)
        .unwrap();
    assert!(
        w.desk
            .auto_bind(&roles(&["generate-image"]))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        w.desk
            .view(&tool("generate-image"))
            .unwrap()
            .bound
            .unwrap()
            .0,
        cid("good-remote")
    );
    // Even a choice that stopped working stays, so it is there when the connection comes back.
    w.shared
        .update(|s| s.connections[0].enabled = false)
        .unwrap();
    assert!(
        w.desk
            .auto_bind(&roles(&["generate-image"]))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        w.desk
            .view(&tool("generate-image"))
            .unwrap()
            .bound
            .unwrap()
            .0,
        cid("good-remote")
    );
}

#[test]
fn auto_bind_ignores_a_connection_that_is_switched_off_and_a_second_run_changes_nothing() {
    let w = world(vec![
        connection("good-off", false, false, &["gpt-image-1"]),
        connection("good-remote", true, true, &["gpt-image-1"]),
    ]);
    let first = w.desk.auto_bind(&roles(&["generate-image"])).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(
        w.desk
            .view(&tool("generate-image"))
            .unwrap()
            .bound
            .unwrap()
            .0,
        cid("good-remote")
    );
    assert!(
        w.desk
            .auto_bind(&roles(&["generate-image"]))
            .unwrap()
            .is_empty()
    );
    assert_eq!(w.shared.load().unwrap().defaults.len(), 1);
}

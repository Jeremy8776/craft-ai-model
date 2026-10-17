//! The request history written by `AiClient`: what a run leaves in `jobs.jsonl`, what it must
//! never leave there, and that a broken log never breaks a job. Providers are fakes registered in
//! the factory; no network is used.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use craft_ai_client::{AiClient, ProviderFactory};
use craft_ai_core::history::MAX_PROMPT_CHARS;
use craft_ai_core::{
    AiError, AppId, Binding, CapabilityId, Connection, ConnectionId, ConsentLedger, CostEstimate,
    ErrorKind, InputRole, JobId, JobSnapshot, JobState, ModelId, ModelInfo, Outcome, Payload,
    Provider, Request, Role, RoleId, SecretRef, SecretString, StoreFile, Usage,
};
use craft_ai_native::HistoryFile;
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use serde_json::json;

const KEY: &str = "sk-history-test-key-777";
const PROMPT: &str = "a very private prompt about a red bicycle";

#[derive(Clone)]
struct Behaviour {
    submit: Result<(), ErrorKind>,
    end: JobState,
    usage: Option<Usage>,
    estimate: Option<CostEstimate>,
    /// Polls that report "running" before the end.
    running_polls: usize,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            submit: Ok(()),
            end: JobState::Succeeded,
            usage: None,
            estimate: None,
            running_polls: 0,
        }
    }
}

struct Fake {
    how: Behaviour,
    polls: Mutex<usize>,
}

impl Provider for Fake {
    fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(Vec::new())
    }
    fn estimate(&self, _: &ModelId, _: &Request) -> Result<Option<CostEstimate>, AiError> {
        Ok(self.how.estimate.clone())
    }
    fn submit(&self, _: &ModelId, _: &Request) -> Result<JobId, AiError> {
        match self.how.submit {
            Ok(()) => JobId::new("gen-42").map_err(|e| AiError::invalid(e.to_string())),
            Err(kind) => Err(AiError::new(kind, "the provider said no")),
        }
    }
    fn poll(&self, _: &JobId) -> Result<JobSnapshot, AiError> {
        let mut n = self.polls.lock().unwrap();
        *n += 1;
        let state = if *n <= self.how.running_polls {
            JobState::Running { percent: None }
        } else {
            self.how.end.clone()
        };
        let mut snap = JobSnapshot::new(state);
        if snap.state.is_terminal() || *n <= self.how.running_polls {
            snap.usage = self.how.usage.clone();
        }
        Ok(snap)
    }
    fn cancel(&self, _: &JobId) -> Result<(), AiError> {
        Ok(())
    }
}

fn role() -> Role {
    Role {
        id: RoleId::new("rewrite").unwrap(),
        label: "Rewrite".into(),
        capability: CapabilityId::parse("text.complete").unwrap(),
        inputs: vec![InputRole::Prompt],
    }
}

fn request() -> Request {
    Request::new(role().capability).with_input(InputRole::Prompt, Payload::text(PROMPT))
}

struct World {
    dir: tempfile::TempDir,
    client: AiClient,
    log: HistoryFile,
}

fn world(how: Behaviour, configure: impl FnOnce(AiClient) -> AiClient) -> World {
    let dir = tempfile::tempdir().unwrap();
    let model = craft_ai_openai::catalog::classify("gpt-4o-mini").unwrap();
    let connection: Connection = serde_json::from_value(json!({
        "id": "hosted", "label": "Hosted", "provider": "history-fake",
        "endpoint": "https://api.example.com", "locality": "remote",
        "auth": {"kind": "api_key", "secret": "hosted-key"}, "billing": "metered",
        "models": [model],
    }))
    .unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    shared
        .replace(&StoreFile {
            connections: vec![connection],
            defaults: vec![Binding {
                role: role().id,
                connection: ConnectionId::new("hosted").unwrap(),
                model: ModelId::new("gpt-4o-mini").unwrap(),
            }],
            ..StoreFile::default()
        })
        .unwrap();
    let secrets = MemoryStore::new();
    secrets
        .set(
            &SecretRef::new("hosted-key").unwrap(),
            &SecretString::new(KEY),
        )
        .unwrap();
    let mut factory = ProviderFactory::standard();
    factory.register("history-fake", move |_, _| {
        Ok(Arc::new(Fake {
            how: how.clone(),
            polls: Mutex::new(0),
        }))
    });
    let log = HistoryFile::new(dir.path().join("jobs.jsonl"));
    let client = AiClient::new(AppId::new("wordcraft").unwrap(), shared, Arc::new(secrets))
        .with_factory(factory)
        .with_history(log.clone());
    World {
        dir,
        client: configure(client),
        log,
    }
}

fn agreed() -> ConsentLedger {
    let mut ledger = ConsentLedger::default();
    let plan = world(Behaviour::default(), |c| c)
        .client
        .plan(&role(), request())
        .unwrap();
    ledger.grant(plan.disclosure());
    ledger
}

fn run(w: &World) -> Result<craft_ai_client::Finished, AiError> {
    let plan = w.client.plan(&role(), request())?;
    let job = w.client.start(&plan, &agreed())?;
    job.wait(Duration::from_millis(1), Duration::from_secs(5))
}

fn raw(w: &World) -> String {
    std::fs::read_to_string(w.log.path()).unwrap_or_default()
}

#[test]
fn a_finished_job_leaves_a_start_and_an_end_with_the_reported_cost() {
    let how = Behaviour {
        usage: Some(Usage {
            cost_micros: Some(31_000),
            currency: Some("USD".into()),
            ..Usage::default()
        }),
        estimate: Some(CostEstimate {
            micros: 30_000,
            currency: "USD".into(),
            billing: craft_ai_core::Billing::Metered,
        }),
        running_polls: 2,
        ..Behaviour::default()
    };
    let w = world(how, |c| c);
    run(&w).unwrap();
    let log = w.client.history().unwrap();
    assert_eq!(log.entries.len(), 1);
    assert_eq!(log.skipped, 0);
    let e = &log.entries[0];
    assert_eq!(e.start.app, "wordcraft");
    assert_eq!(e.start.role_label, "Rewrite");
    assert_eq!(e.start.capability, "text.complete");
    assert_eq!(e.start.connection, "hosted");
    assert_eq!(e.start.connection_label, "Hosted");
    assert_eq!(e.start.model, "gpt-4o-mini");
    assert_eq!(e.start.destination, "https://api.example.com");
    assert_eq!(e.start.provider_job.as_deref(), Some("gen-42"));
    assert_eq!(e.start.estimate.as_ref().map(|c| c.micros), Some(30_000));
    assert_eq!(e.start.sent.len(), 1);
    assert_eq!(e.outcome(), Some(&Outcome::Done));
    assert_eq!(e.reported_cost(), Some((31_000, "USD")));
    assert!(e.duration_ms().is_some());
    assert_eq!(raw(&w).lines().count(), 2, "one start line, one end line");
}

#[test]
fn neither_the_key_nor_the_prompt_is_in_the_file_by_default() {
    let w = world(Behaviour::default(), |c| c);
    run(&w).unwrap();
    let text = raw(&w);
    assert!(!text.contains(KEY), "{text}");
    assert!(!text.contains("bicycle"), "{text}");
    assert!(!text.contains("hosted-key"), "{text}");
    assert!(
        w.client.history().unwrap().entries[0]
            .start
            .prompt
            .is_none()
    );
}

#[test]
fn the_prompt_is_kept_only_when_asked_and_never_the_key() {
    let w = world(Behaviour::default(), |c| c.keep_prompts_in_history(true));
    run(&w).unwrap();
    let log = w.client.history().unwrap();
    assert_eq!(log.entries[0].start.prompt.as_deref(), Some(PROMPT));
    assert!(!raw(&w).contains(KEY));
}

#[test]
fn a_long_prompt_is_clipped() {
    let w = world(Behaviour::default(), |c| c.keep_prompts_in_history(true));
    let long = "x".repeat(MAX_PROMPT_CHARS * 3);
    let req = Request::new(role().capability).with_input(InputRole::Prompt, Payload::text(long));
    let plan = w.client.plan(&role(), req).unwrap();
    let job = w.client.start(&plan, &agreed()).unwrap();
    job.wait(Duration::from_millis(1), Duration::from_secs(5))
        .unwrap();
    let kept = w.client.history().unwrap().entries[0]
        .start
        .prompt
        .clone()
        .unwrap();
    assert_eq!(kept.chars().count(), MAX_PROMPT_CHARS);
}

#[test]
fn a_failed_job_records_only_the_kind_of_error() {
    let how = Behaviour {
        end: JobState::Failed {
            error: AiError::new(ErrorKind::Quota, "out of credit for account 12345"),
        },
        ..Behaviour::default()
    };
    let w = world(how, |c| c);
    run(&w).unwrap();
    let log = w.client.history().unwrap();
    assert_eq!(
        log.entries[0].outcome(),
        Some(&Outcome::Failed {
            kind: ErrorKind::Quota
        })
    );
    assert!(!raw(&w).contains("12345"));
}

#[test]
fn a_cancelled_job_is_recorded_as_cancelled() {
    let how = Behaviour {
        end: JobState::Cancelled,
        ..Behaviour::default()
    };
    let w = world(how, |c| c);
    run(&w).unwrap();
    assert_eq!(
        w.client.history().unwrap().entries[0].outcome(),
        Some(&Outcome::Cancelled)
    );
}

#[test]
fn a_provider_that_refuses_the_job_is_logged_as_failed_with_no_provider_id() {
    let how = Behaviour {
        submit: Err(ErrorKind::Auth),
        ..Behaviour::default()
    };
    let w = world(how, |c| c);
    assert_eq!(run(&w).unwrap_err().kind, ErrorKind::Auth);
    let e = &w.client.history().unwrap().entries[0];
    assert_eq!(e.start.provider_job, None);
    assert_eq!(
        e.outcome(),
        Some(&Outcome::Failed {
            kind: ErrorKind::Auth
        })
    );
}

#[test]
fn a_missing_key_is_logged_as_a_failed_start() {
    let w = world(Behaviour::default(), |c| c);
    // Same adapter, but a keychain with no key in it: the key is what is missing.
    let mut factory = ProviderFactory::standard();
    factory.register("history-fake", |_, _| {
        Ok(Arc::new(Fake {
            how: Behaviour::default(),
            polls: Mutex::new(0),
        }))
    });
    let empty = AiClient::new(
        AppId::new("wordcraft").unwrap(),
        SharedStore::new(w.dir.path().join("connections.json")),
        Arc::new(MemoryStore::new()),
    )
    .with_factory(factory)
    .with_history(w.log.clone());
    let plan = empty.plan(&role(), request()).unwrap();
    assert_eq!(
        empty.start(&plan, &agreed()).unwrap_err().kind,
        ErrorKind::Auth
    );
    let e = &empty.history().unwrap().entries[0];
    assert!(matches!(e.outcome(), Some(Outcome::Failed { .. })));
}

#[test]
fn a_refusal_for_missing_consent_is_not_a_job_and_is_not_logged() {
    let w = world(Behaviour::default(), |c| c);
    let plan = w.client.plan(&role(), request()).unwrap();
    let err = w
        .client
        .start(&plan, &ConsentLedger::default())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::ConsentRequired);
    assert!(w.client.history().unwrap().entries.is_empty());
}

#[test]
fn a_job_nobody_polled_to_the_end_stays_unfinished() {
    let w = world(Behaviour::default(), |c| c);
    let plan = w.client.plan(&role(), request()).unwrap();
    let _job = w.client.start(&plan, &agreed()).unwrap();
    let e = &w.client.history().unwrap().entries[0];
    assert_eq!(e.outcome(), None);
}

#[test]
fn polling_a_finished_job_again_or_from_a_clone_writes_one_end_line() {
    let w = world(Behaviour::default(), |c| c);
    let plan = w.client.plan(&role(), request()).unwrap();
    let job = w.client.start(&plan, &agreed()).unwrap();
    let twin = job.clone();
    for _ in 0..3 {
        job.poll().unwrap();
        twin.poll().unwrap();
    }
    assert_eq!(raw(&w).lines().count(), 2);
}

#[test]
fn giving_up_waiting_records_the_run_as_failed() {
    let how = Behaviour {
        running_polls: usize::MAX,
        ..Behaviour::default()
    };
    let w = world(how, |c| c);
    let plan = w.client.plan(&role(), request()).unwrap();
    let job = w.client.start(&plan, &agreed()).unwrap();
    assert!(
        job.wait(Duration::from_millis(1), Duration::from_millis(20))
            .is_err()
    );
    let e = &w.client.history().unwrap().entries[0];
    assert_eq!(
        e.outcome(),
        Some(&Outcome::Failed {
            kind: ErrorKind::Network
        })
    );
}

#[test]
fn a_log_that_cannot_be_written_never_fails_the_job() {
    let w = world(Behaviour::default(), |c| c);
    let blocker = w.dir.path().join("not-a-dir");
    std::fs::write(&blocker, b"x").unwrap();
    let broken = AiClient::new(
        AppId::new("wordcraft").unwrap(),
        SharedStore::new(w.dir.path().join("connections.json")),
        Arc::new({
            let s = MemoryStore::new();
            s.set(
                &SecretRef::new("hosted-key").unwrap(),
                &SecretString::new(KEY),
            )
            .unwrap();
            s
        }),
    )
    .with_factory({
        let mut f = ProviderFactory::standard();
        f.register("history-fake", |_, _| {
            Ok(Arc::new(Fake {
                how: Behaviour::default(),
                polls: Mutex::new(0),
            }))
        });
        f
    })
    .with_history(HistoryFile::new(blocker.join("jobs.jsonl")));
    let plan = broken.plan(&role(), request()).unwrap();
    let job = broken.start(&plan, &agreed()).unwrap();
    let done = job
        .wait(Duration::from_millis(1), Duration::from_secs(5))
        .unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert!(
        broken
            .history()
            .map(|l| l.entries.is_empty())
            .unwrap_or(true),
        "nothing was written"
    );
}

#[test]
fn with_history_off_jobs_run_and_the_log_says_it_is_off() {
    let w = world(Behaviour::default(), |c| c.without_history());
    run(&w).unwrap();
    assert!(!w.log.path().exists());
    assert_eq!(w.client.history().unwrap_err().kind, ErrorKind::Unsupported);
}

#[test]
fn two_apps_share_one_log_and_clear_empties_it() {
    let w = world(Behaviour::default(), |c| c);
    run(&w).unwrap();
    let other = AiClient::new(
        AppId::new("photocraft").unwrap(),
        SharedStore::new(w.dir.path().join("connections.json")),
        Arc::new(MemoryStore::new()),
    )
    .with_history(w.log.clone());
    assert_eq!(other.history().unwrap().entries.len(), 1);
    other.clear_history().unwrap();
    assert!(w.client.history().unwrap().entries.is_empty());
}

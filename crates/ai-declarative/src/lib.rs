//! Runs a declarative spec (`craft_ai_core::spec`): a provider described by data.
//!
//! The spec says which calls to make; this crate makes them, within bounds, and nothing else.
//! Every call goes to the spec's `base`; a link the service returns is handed back as a handle
//! and never fetched here; the key is sent in the one header the spec names; and no reply text
//! reaches an error message.
//!
//! A request/response job makes its single call on the first poll, exactly once: a failure ends
//! the job instead of being retried, so a billable call is never sent twice. A queue job's status
//! checks are read-only and may be retried. `poll` blocks for the length of one call.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod flow;
mod http;

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use craft_ai_core::payload::Limits;
use craft_ai_core::spec::template::Values;
use craft_ai_core::spec::{Flow, Spec, SpecAuth, StateWord};
use craft_ai_core::{
    AiError, ErrorKind, Event, InputRole, JobId, JobSnapshot, JobState, ModelId, ModelInfo, Output,
    Payload, Provider, Request, SecretString,
};
use flow::Filled;
use http::Http;

/// Most jobs tracked at once. Finished ones are forgotten, oldest first, to make room.
const MAX_JOBS: usize = 128;

/// What a declarative provider is built from.
#[derive(Clone, Debug)]
pub struct DeclarativeConfig {
    pub spec: Spec,
    /// The key, when the spec's `auth` uses one.
    pub key: Option<SecretString>,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

impl DeclarativeConfig {
    pub fn new(spec: Spec, key: Option<SecretString>) -> Self {
        Self {
            spec,
            key,
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(60),
        }
    }
}

#[derive(Clone)]
struct Direct {
    prompt: String,
    model: String,
}

struct Job {
    seq: u64,
    started: Instant,
    polls: u32,
    /// The service's id, for a queue job.
    remote: Option<String>,
    /// What to send, for a request/response job.
    direct: Option<Direct>,
    attempted: bool,
    snapshot: JobSnapshot,
}

pub struct DeclarativeProvider {
    spec: Spec,
    models: Vec<ModelInfo>,
    http: Http,
    jobs: Mutex<HashMap<JobId, Job>>,
    next: AtomicU64,
}

impl std::fmt::Debug for DeclarativeProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeclarativeProvider({})", self.spec.id)
    }
}

fn stable(job: &Job) -> JobSnapshot {
    let mut snap = job.snapshot.clone();
    snap.events.clear();
    snap
}

fn finish(job: &mut Job, snap: JobSnapshot) -> JobSnapshot {
    job.snapshot = snap.clone();
    snap
}

fn failed(error: AiError) -> JobSnapshot {
    JobSnapshot::new(JobState::Failed { error })
}

fn succeeded(payload: Payload) -> JobSnapshot {
    let mut snap = JobSnapshot::new(JobState::Succeeded);
    snap.events.push(Event::Output(Output {
        payload,
        label: None,
    }));
    snap
}

impl DeclarativeProvider {
    pub fn new(config: DeclarativeConfig) -> Result<Self, AiError> {
        let spec = config.spec;
        let key = match spec.auth {
            SpecAuth::None => None,
            _ => {
                let key = config.key.ok_or_else(|| {
                    AiError::invalid(format!("{} needs a key, and none is saved", spec.label))
                })?;
                if key.is_empty()
                    || key
                        .expose()
                        .chars()
                        .any(|c| c.is_control() || c.is_whitespace())
                {
                    return Err(AiError::invalid("a single-line key is required"));
                }
                Some(key)
            }
        };
        let http = Http::new(
            spec.base.trim_end_matches('/').to_owned(),
            spec.label.clone(),
            spec.auth.clone(),
            key,
            config.connect_timeout,
            config.request_timeout,
        );
        Ok(Self {
            models: spec.model_infos(),
            spec,
            http,
            jobs: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<JobId, Job>>, AiError> {
        self.jobs
            .lock()
            .map_err(|_| AiError::new(ErrorKind::Internal, "the job list is unavailable"))
    }

    fn send(&self, filled: &Filled) -> Result<serde_json::Value, AiError> {
        self.http.call(
            filled.method,
            &filled.path,
            &filled.query,
            filled.body.as_ref(),
        )
    }

    /// Makes room for one more job, dropping finished ones oldest first. Refuses when every
    /// tracked job is still running.
    fn make_room(&self, jobs: &mut HashMap<JobId, Job>) -> Result<(), AiError> {
        while jobs.len() >= MAX_JOBS {
            let oldest = jobs
                .iter()
                .filter(|(_, j)| j.snapshot.state.is_terminal())
                .min_by_key(|(_, j)| j.seq)
                .map(|(id, _)| id.clone());
            match oldest {
                Some(id) => {
                    jobs.remove(&id);
                }
                None => {
                    return Err(AiError::new(
                        ErrorKind::Quota,
                        format!("too many {} jobs are still running", self.spec.label),
                    ));
                }
            }
        }
        Ok(())
    }

    fn insert(&self, remote: Option<String>, direct: Option<Direct>) -> Result<JobId, AiError> {
        let seq = self.next.fetch_add(1, Ordering::SeqCst);
        let id = JobId::new(format!("decl-{seq}"))
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not name the job"))?;
        let mut jobs = self.lock()?;
        self.make_room(&mut jobs)?;
        jobs.insert(
            id.clone(),
            Job {
                seq,
                started: Instant::now(),
                polls: 0,
                remote,
                direct,
                attempted: false,
                snapshot: JobSnapshot::new(JobState::Queued),
            },
        );
        Ok(id)
    }

    /// Stores `snap` unless the job already ended (a cancel can land while a call is running).
    fn settle(&self, id: &JobId, snap: JobSnapshot) -> Result<JobSnapshot, AiError> {
        let mut jobs = self.lock()?;
        let job = jobs
            .get_mut(id)
            .ok_or_else(|| AiError::invalid("unknown job"))?;
        if job.snapshot.state.is_terminal() {
            return Ok(stable(job));
        }
        Ok(finish(job, snap))
    }

    fn read_status(
        &self,
        reply: &serde_json::Value,
        prior: JobState,
    ) -> Result<JobSnapshot, AiError> {
        let Flow::Queue { poll, .. } = &self.spec.flow else {
            return Err(AiError::new(ErrorKind::Internal, "not a queue spec"));
        };
        let label = &self.spec.label;
        let word = flow::status(reply, &poll.status_at, label)?;
        let state = poll.states.get(word).ok_or_else(|| {
            AiError::new(
                ErrorKind::Provider,
                format!("{label} gave a status the spec does not list"),
            )
        })?;
        Ok(match state {
            StateWord::Queued => JobSnapshot::new(prior),
            StateWord::Running => JobSnapshot::new(JobState::Running { percent: None }),
            StateWord::Succeeded => match flow::payload(reply, &poll.result, label) {
                Ok(payload) => succeeded(payload),
                Err(e) => failed(e),
            },
            StateWord::Failed => failed(AiError::new(
                ErrorKind::Provider,
                format!("{label} reported a failure ({word})"),
            )),
        })
    }
}

impl Provider for DeclarativeProvider {
    fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        if let Some(probe) = &self.spec.probe {
            self.send(&flow::fill(probe, &Values::default())?)?;
        }
        Ok(self.models.clone())
    }

    fn submit(&self, model: &ModelId, request: &Request) -> Result<JobId, AiError> {
        let info = self.models.iter().find(|m| &m.id == model).ok_or_else(|| {
            AiError::unsupported(format!("{} does not offer model {model}", self.spec.label))
        })?;
        let spec = info.spec_for(&request.capability).ok_or_else(|| {
            AiError::unsupported(format!("{model} cannot do {}", request.capability))
        })?;
        request.validate(spec, &Limits::default())?;
        if !request.params.is_empty() {
            return Err(AiError::unsupported(
                "this route takes no options yet; use the service's defaults",
            ));
        }
        let prompt = request
            .inputs
            .iter()
            .find(|i| i.role == InputRole::Prompt)
            .and_then(|i| match &i.payload {
                Payload::Text { text } => Some(text.clone()),
                _ => None,
            })
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| AiError::invalid("a prompt with some text is needed"))?;
        {
            // Make room before anything billable is sent.
            let mut jobs = self.lock()?;
            self.make_room(&mut jobs)?;
        }
        let model = model.as_str().to_owned();
        match &self.spec.flow {
            Flow::RequestResponse { .. } => self.insert(None, Some(Direct { prompt, model })),
            Flow::Queue { submit, .. } => {
                let values = Values {
                    prompt: Some(&prompt),
                    model: Some(&model),
                    job_id: None,
                };
                let reply = self.send(&flow::fill(&submit.call, &values)?)?;
                let remote = flow::job_id(&reply, &submit.job_id_at, &self.spec.label)?;
                self.insert(Some(remote), None)
            }
        }
    }

    fn poll(&self, id: &JobId) -> Result<JobSnapshot, AiError> {
        enum Work {
            Direct(Direct),
            Queue(String, JobState),
        }
        let work = {
            let mut jobs = self.lock()?;
            let job = jobs
                .get_mut(id)
                .ok_or_else(|| AiError::invalid("unknown job"))?;
            if job.snapshot.state.is_terminal() {
                return Ok(stable(job));
            }
            job.polls = job.polls.saturating_add(1);
            let limits = self.spec.limits;
            if job.polls > limits.max_polls
                || job.started.elapsed() > Duration::from_secs(u64::from(limits.max_seconds))
            {
                let why = format!("gave up waiting for {}", self.spec.label);
                return Ok(finish(job, failed(AiError::new(ErrorKind::Provider, why))));
            }
            match (&job.direct, &job.remote) {
                (Some(_), _) if job.attempted => return Ok(stable(job)),
                (Some(direct), _) => {
                    let direct = direct.clone();
                    job.attempted = true;
                    job.snapshot = JobSnapshot::new(JobState::Running { percent: None });
                    Work::Direct(direct)
                }
                (None, Some(remote)) => Work::Queue(remote.clone(), job.snapshot.state.clone()),
                (None, None) => {
                    return Err(AiError::new(ErrorKind::Internal, "a job with no work"));
                }
            }
        };
        let label = &self.spec.label;
        let snap = match (work, &self.spec.flow) {
            (Work::Direct(d), Flow::RequestResponse { call, result }) => {
                let values = Values {
                    prompt: Some(&d.prompt),
                    model: Some(&d.model),
                    job_id: None,
                };
                // One attempt only: a failure ends the job rather than sending the call again.
                match flow::fill(call, &values)
                    .and_then(|f| self.send(&f))
                    .and_then(|reply| flow::payload(&reply, result, label))
                {
                    Ok(payload) => succeeded(payload),
                    Err(e) => failed(e),
                }
            }
            (Work::Queue(remote, prior), Flow::Queue { poll, .. }) => {
                let values = Values {
                    prompt: None,
                    model: None,
                    job_id: Some(&remote),
                };
                let reply = self.send(&flow::fill(&poll.call, &values)?)?;
                self.read_status(&reply, prior)?
            }
            _ => {
                return Err(AiError::new(ErrorKind::Internal, "job and spec disagree"));
            }
        };
        self.settle(id, snap)
    }

    fn cancel(&self, id: &JobId) -> Result<(), AiError> {
        let remote = {
            let mut jobs = self.lock()?;
            let job = jobs
                .get_mut(id)
                .ok_or_else(|| AiError::invalid("unknown job"))?;
            if job.snapshot.state.is_terminal() {
                return Ok(());
            }
            match (&job.remote, &self.spec.flow) {
                (
                    Some(remote),
                    Flow::Queue {
                        cancel: Some(_), ..
                    },
                ) => remote.clone(),
                (Some(_), _) => {
                    return Err(AiError::unsupported(format!(
                        "{} has no way to cancel a job",
                        self.spec.label
                    )));
                }
                (None, _) => {
                    // Not sent yet, or already sent once: either way nothing more will be.
                    finish(job, JobSnapshot::new(JobState::Cancelled));
                    return Ok(());
                }
            }
        };
        if let Flow::Queue {
            cancel: Some(call), ..
        } = &self.spec.flow
        {
            let values = Values {
                prompt: None,
                model: None,
                job_id: Some(&remote),
            };
            self.send(&flow::fill(call, &values)?)?;
        }
        let mut jobs = self.lock()?;
        if let Some(job) = jobs.get_mut(id)
            && !job.snapshot.state.is_terminal()
        {
            finish(job, JobSnapshot::new(JobState::Cancelled));
        }
        Ok(())
    }
}

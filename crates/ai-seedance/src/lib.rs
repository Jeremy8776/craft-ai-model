//! Seedance text-to-video through BytePlus ModelArk asynchronous tasks.
//! Result URLs expire; callers must download or persist assets they wish to keep.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod http;
use craft_ai_core::{
    AiError, ErrorKind, Event, InputRole, JobId, JobSnapshot, JobState, ModelId, ModelInfo, Output,
    Payload, Provider, Request, SecretString, capability::CapabilityId, endpoint::Endpoint,
    model::CapabilitySpec, payload::Limits,
};
use http::Http;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Mutex, time::Duration};

const MODEL: &str = "dreamina-seedance-2-0-260128";
#[derive(Clone, Debug)]
pub struct SeedanceConfig {
    pub base_url: String,
    pub api_key: SecretString,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}
impl SeedanceConfig {
    pub fn new(base_url: impl Into<String>, api_key: SecretString) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(60),
        }
    }
}
struct Job {
    remote_id: String,
    snapshot: JobSnapshot,
}
pub struct SeedanceProvider {
    http: Http,
    jobs: Mutex<HashMap<JobId, Job>>,
}
impl std::fmt::Debug for SeedanceProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SeedanceProvider")
    }
}
impl SeedanceProvider {
    pub fn new(config: SeedanceConfig) -> Result<Self, AiError> {
        if config.api_key.is_empty()
            || config
                .api_key
                .expose()
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(AiError::invalid(
                "a single-line ModelArk API key is required",
            ));
        }
        let base = config.base_url.trim_end_matches('/');
        let endpoint =
            Endpoint::parse(base).map_err(|_| AiError::invalid("invalid ModelArk API address"))?;
        if base.contains(['?', '#'])
            || (!endpoint.is_loopback() && base != "https://ark.ap-southeast.bytepluses.com/api/v3")
        {
            return Err(AiError::invalid(
                "Seedance must use the official BytePlus ModelArk address",
            ));
        }
        Ok(Self {
            http: Http::new(
                base.to_owned(),
                config.api_key,
                config.connect_timeout,
                config.request_timeout,
            ),
            jobs: Mutex::new(HashMap::new()),
        })
    }
    fn catalog() -> Vec<ModelInfo> {
        match (ModelId::new(MODEL), CapabilityId::parse("video.generate")) {
            (Ok(id), Ok(cap)) => vec![
                ModelInfo::new(id, "Seedance 2.0").with(
                    CapabilitySpec::new(cap)
                        .accepting(InputRole::Prompt, 1)
                        .requiring(InputRole::Prompt),
                ),
            ],
            _ => Vec::new(),
        }
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<JobId, Job>>, AiError> {
        self.jobs
            .lock()
            .map_err(|_| AiError::new(ErrorKind::Internal, "Seedance job registry unavailable"))
    }
}
impl Provider for SeedanceProvider {
    fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        self.http
            .get("/contents/generations/tasks?page_num=1&page_size=1")?;
        Ok(Self::catalog())
    }
    fn submit(&self, model: &ModelId, request: &Request) -> Result<JobId, AiError> {
        let info = Self::catalog()
            .into_iter()
            .find(|m| &m.id == model)
            .ok_or_else(|| {
                AiError::unsupported(format!("Seedance does not serve model {model}"))
            })?;
        let spec = info.spec_for(&request.capability).ok_or_else(|| {
            AiError::unsupported(format!("{model} cannot do {}", request.capability))
        })?;
        request.validate(spec, &Limits::default())?;
        if !request.params.is_empty() {
            return Err(AiError::unsupported(
                "Seedance options are not mapped yet; use provider defaults",
            ));
        }
        let prompt = request
            .inputs
            .iter()
            .find(|i| i.role == InputRole::Prompt)
            .and_then(|i| match &i.payload {
                Payload::Text { text } => Some(text),
                _ => None,
            })
            .filter(|p| !p.is_empty())
            .ok_or_else(|| AiError::invalid("Seedance needs a non-empty text prompt"))?;
        if self.lock()?.len() >= 128 {
            return Err(AiError::new(
                ErrorKind::Quota,
                "too many tracked Seedance jobs",
            ));
        }
        let reply = self.http.post(
            "/contents/generations/tasks",
            &json!({"model": MODEL, "content": [{"type":"text","text":prompt}]}),
        )?;
        let remote = reply
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 96
                    && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            })
            .ok_or_else(|| AiError::new(ErrorKind::Provider, "ModelArk task ID is invalid"))?;
        let id = JobId::new(format!("ark-{remote}")).map_err(|_| {
            AiError::new(
                ErrorKind::Provider,
                "ModelArk task ID cannot be represented",
            )
        })?;
        self.lock()?.insert(
            id.clone(),
            Job {
                remote_id: remote.to_owned(),
                snapshot: JobSnapshot::new(JobState::Queued),
            },
        );
        Ok(id)
    }
    fn poll(&self, id: &JobId) -> Result<JobSnapshot, AiError> {
        let (remote, prior) = {
            let jobs = self.lock()?;
            let job = jobs
                .get(id)
                .ok_or_else(|| AiError::invalid("unknown Seedance job"))?;
            if job.snapshot.state.is_terminal() {
                let mut stable = job.snapshot.clone();
                stable.events.clear();
                return Ok(stable);
            }
            (job.remote_id.clone(), job.snapshot.state.clone())
        };
        let reply = self
            .http
            .get(&format!("/contents/generations/tasks/{remote}"))?;
        if reply.get("id").and_then(Value::as_str) != Some(remote.as_str()) {
            return Err(AiError::new(
                ErrorKind::Provider,
                "ModelArk returned a different task ID",
            ));
        }
        let mut snap = match reply.get("status").and_then(Value::as_str).unwrap_or("") {
            "queued" | "pending" => JobSnapshot::new(prior),
            "running" => JobSnapshot::new(JobState::Running { percent: None }),
            "succeeded" => {
                let uri = reply
                    .pointer("/content/video_url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        AiError::new(ErrorKind::Provider, "Seedance result has no video URL")
                    })?;
                let endpoint = Endpoint::parse(uri).map_err(|_| {
                    AiError::new(ErrorKind::Provider, "Seedance result URL is invalid")
                })?;
                if !endpoint.https || endpoint.is_loopback() {
                    return Err(AiError::new(
                        ErrorKind::Provider,
                        "Seedance result URL is unsafe",
                    ));
                }
                let mut snap = JobSnapshot::new(JobState::Succeeded);
                snap.events.push(Event::Output(Output {
                    payload: Payload::handle(uri, "video/mp4", None)?,
                    label: None,
                }));
                snap
            }
            "failed" | "expired" => JobSnapshot::new(JobState::Failed {
                error: AiError::new(ErrorKind::Provider, "Seedance task failed"),
            }),
            "cancelled" => JobSnapshot::new(JobState::Cancelled),
            _ => {
                return Err(AiError::new(
                    ErrorKind::Provider,
                    "ModelArk returned an unknown status",
                ));
            }
        };
        let mut jobs = self.lock()?;
        if let Some(job) = jobs.get_mut(id) {
            if job.snapshot.state.is_terminal() {
                snap = job.snapshot.clone();
                snap.events.clear();
            } else {
                job.snapshot = snap.clone();
            }
        }
        Ok(snap)
    }
    fn cancel(&self, id: &JobId) -> Result<(), AiError> {
        let jobs = self.lock()?;
        match jobs.get(id) {
            None => Err(AiError::invalid("unknown Seedance job")),
            Some(job) if job.snapshot.state.is_terminal() => Ok(()),
            Some(_) => Err(AiError::unsupported(
                "Seedance cancellation is not wired; the job remains active",
            )),
        }
    }
}

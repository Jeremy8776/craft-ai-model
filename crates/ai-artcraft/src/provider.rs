//! API-key generation and polling, not subscription entitlement.
use crate::http::Http;
use craft_ai_core::{
    AiError, ErrorKind, Event, InputRole, JobId, JobSnapshot, JobState, ModelId, ModelInfo, Output,
    Payload, Provider, Request, SecretString, capability::CapabilityId, endpoint::Endpoint,
    model::CapabilitySpec, payload::Limits,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{collections::HashMap, sync::Mutex, time::Duration};

#[derive(Clone, Debug)]
pub struct ArtCraftConfig {
    pub base_url: String,
    pub api_key: SecretString,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    /// Limits tracked jobs and reserves a slot before a paid request is sent.
    pub max_active_jobs: usize,
}
impl ArtCraftConfig {
    pub fn new(base_url: impl Into<String>, api_key: SecretString) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(60),
            max_active_jobs: 16,
        }
    }
}
/// Most jobs remembered, finished ones included. Oldest finished jobs are dropped first.
const KEEP_JOBS: usize = 128;

struct Job {
    remote: String,
    video: bool,
    snapshot: JobSnapshot,
}
/// Holds a slot for a request that has been sent but has no job token yet, so concurrent
/// submits cannot all pass the limit before any of them is recorded.
struct Reservation<'a>(&'a AtomicUsize);
impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
pub struct ArtCraftProvider {
    http: Http,
    jobs: Mutex<HashMap<JobId, Job>>,
    pending: AtomicUsize,
    max_active: usize,
}
impl std::fmt::Debug for ArtCraftProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ArtCraftProvider(Omni API key)")
    }
}
impl ArtCraftProvider {
    pub fn new(config: ArtCraftConfig) -> Result<Self, AiError> {
        if config.api_key.is_empty()
            || config
                .api_key
                .expose()
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(AiError::invalid(
                "a single-line ArtCraft API key is required",
            ));
        }
        let base = config.base_url.trim_end_matches('/');
        let endpoint = Endpoint::parse(base)
            .map_err(|_| AiError::invalid("invalid ArtCraft Omni API address"))?;
        if base.contains(['?', '#'])
            || (!endpoint.is_loopback() && base != "https://api.storyteller.ai")
        {
            return Err(AiError::invalid(
                "ArtCraft Omni API must use its official address",
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
            pending: AtomicUsize::new(0),
            max_active: config.max_active_jobs.max(1),
        })
    }
    pub fn documented_models() -> Vec<ModelInfo> {
        [
            ("nano_banana", "Nano Banana (unverified)", "image.generate"),
            (
                "seedance_2p0",
                "Seedance 2.0 (unverified)",
                "video.generate",
            ),
        ]
        .into_iter()
        .filter_map(|(id, label, cap)| {
            Some(
                ModelInfo::new(ModelId::new(id).ok()?, label).with(
                    CapabilitySpec::new(CapabilityId::parse(cap).ok()?)
                        .accepting(InputRole::Prompt, 1)
                        .requiring(InputRole::Prompt),
                ),
            )
        })
        .collect()
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<JobId, Job>>, AiError> {
        self.jobs
            .lock()
            .map_err(|_| AiError::new(ErrorKind::Internal, "ArtCraft job registry unavailable"))
    }
}
impl Provider for ArtCraftProvider {
    fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Err(AiError::unsupported(
            "ArtCraft Omni API has no documented free key test; saved models are unverified until a job runs",
        ))
    }
    fn submit(&self, model: &ModelId, request: &Request) -> Result<JobId, AiError> {
        let info = Self::documented_models()
            .into_iter()
            .find(|m| &m.id == model)
            .ok_or_else(|| {
                AiError::unsupported(format!("ArtCraft Omni API model {model} is not configured"))
            })?;
        let spec = info.spec_for(&request.capability).ok_or_else(|| {
            AiError::unsupported(format!("{model} cannot do {}", request.capability))
        })?;
        request.validate(spec, &Limits::default())?;
        if !request.params.is_empty() {
            return Err(AiError::unsupported(
                "ArtCraft Omni API options are not mapped yet",
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
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| AiError::invalid("ArtCraft Omni API needs a text prompt"))?;
        let video = model.as_str() == "seedance_2p0";
        // Reserve before sending: a paid job that cannot be tracked must never be created.
        let _slot = {
            let jobs = self.lock()?;
            let active = jobs
                .values()
                .filter(|j| !j.snapshot.state.is_terminal())
                .count();
            if active + self.pending.load(Ordering::SeqCst) >= self.max_active {
                return Err(AiError::new(
                    ErrorKind::Quota,
                    "too many ArtCraft jobs are running; wait for one to finish",
                ));
            }
            self.pending.fetch_add(1, Ordering::SeqCst);
            Reservation(&self.pending)
        };
        let body = json!({"model":model.as_str(),"prompt":prompt,"idempotency_token":uuid::Uuid::new_v4().to_string()});
        let response = self.http.post(
            if video {
                "/v1/omni_api/generate/video"
            } else {
                "/v1/omni_api/generate/image"
            },
            &body,
        )?;
        if response.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(AiError::new(
                ErrorKind::Provider,
                "ArtCraft Omni API did not accept the generation",
            ));
        }
        let remote = response
            .get("inference_job_token")
            .and_then(Value::as_str)
            .filter(|s| {
                s.starts_with("jinf_")
                    && s.len() <= 128
                    && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
            .ok_or_else(|| {
                AiError::new(
                    ErrorKind::Provider,
                    "ArtCraft Omni API returned an invalid job token",
                )
            })?;
        let id = JobId::new(format!("artcraft-{remote}")).map_err(|_| {
            AiError::new(
                ErrorKind::Provider,
                "ArtCraft job token cannot be represented",
            )
        })?;
        let mut jobs = self.lock()?;
        while jobs.len() >= KEEP_JOBS {
            match jobs.iter().find(|(_, j)| j.snapshot.state.is_terminal()) {
                Some((old, _)) => {
                    let old = old.clone();
                    jobs.remove(&old);
                }
                None => break,
            }
        }
        jobs.insert(
            id.clone(),
            Job {
                remote: remote.to_owned(),
                video,
                snapshot: JobSnapshot::new(JobState::Queued),
            },
        );
        Ok(id)
    }
    fn poll(&self, id: &JobId) -> Result<JobSnapshot, AiError> {
        let (remote, video, prior) = {
            let jobs = self.lock()?;
            let job = jobs
                .get(id)
                .ok_or_else(|| AiError::invalid("unknown ArtCraft job"))?;
            if job.snapshot.state.is_terminal() {
                let mut stable = job.snapshot.clone();
                stable.events.clear();
                return Ok(stable);
            }
            (job.remote.clone(), job.video, job.snapshot.state.clone())
        };
        let reply = self
            .http
            .get(&format!("/v1/omni_api/job_status/job/{remote}"))?;
        if reply.get("success").and_then(Value::as_bool) != Some(true)
            || reply.pointer("/state/job_token").and_then(Value::as_str) != Some(remote.as_str())
        {
            return Err(AiError::new(
                ErrorKind::Provider,
                "ArtCraft Omni API returned the wrong job",
            ));
        }
        let status = reply
            .pointer("/state/status/status")
            .and_then(Value::as_str)
            .unwrap_or("");
        let percent = reply
            .pointer("/state/status/progress_percentage")
            .and_then(Value::as_u64)
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| *n <= 100);
        let mut snap = match status {
            "pending" => JobSnapshot::new(prior.clone()),
            "started" | "attempt_failed" => JobSnapshot::new(JobState::Running {
                percent: match &prior {
                    JobState::Running {
                        percent: Some(previous),
                    } => Some(percent.unwrap_or(*previous).max(*previous)),
                    _ => percent,
                },
            }),
            "complete_success" => {
                let uri = reply
                    .pointer("/state/maybe_result/media_links/cdn_url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        AiError::new(ErrorKind::Provider, "ArtCraft job result has no CDN URL")
                    })?;
                let endpoint = Endpoint::parse(uri).map_err(|_| {
                    AiError::new(ErrorKind::Provider, "ArtCraft job result URL is invalid")
                })?;
                if !endpoint.https || endpoint.is_loopback() || uri.contains('@') {
                    return Err(AiError::new(
                        ErrorKind::Provider,
                        "ArtCraft job result URL is unsafe",
                    ));
                }
                let path = uri.split('?').next().unwrap_or(uri).to_ascii_lowercase();
                let media = if video {
                    if path.ends_with(".mp4") {
                        "video/mp4"
                    } else if path.ends_with(".webm") {
                        "video/webm"
                    } else {
                        return Err(AiError::unsupported(
                            "ArtCraft returned an unknown video format",
                        ));
                    }
                } else if path.ends_with(".png") {
                    "image/png"
                } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
                    "image/jpeg"
                } else if path.ends_with(".webp") {
                    "image/webp"
                } else {
                    return Err(AiError::unsupported(
                        "ArtCraft returned an unknown image format",
                    ));
                };
                let mut done = JobSnapshot::new(JobState::Succeeded);
                done.events.push(Event::Output(Output {
                    payload: Payload::handle(uri, media, None)?,
                    label: None,
                }));
                done
            }
            "complete_failure" | "dead" => JobSnapshot::new(JobState::Failed {
                error: AiError::new(ErrorKind::Provider, format!("ArtCraft job ended: {status}")),
            }),
            "cancelled_by_user" | "cancelled_by_system" => JobSnapshot::new(JobState::Cancelled),
            _ => {
                return Err(AiError::new(
                    ErrorKind::Provider,
                    "ArtCraft returned an unknown job state",
                ));
            }
        };
        let mut jobs = self.lock()?;
        if let Some(job) = jobs.get_mut(id) {
            if job.snapshot.state.is_terminal() {
                snap = job.snapshot.clone();
                snap.events.clear();
            } else if job.snapshot.state.can_become(&snap.state) {
                job.snapshot = snap.clone();
            } else {
                snap = job.snapshot.clone();
                snap.events.clear();
            }
        }
        Ok(snap)
    }
    fn cancel(&self, id: &JobId) -> Result<(), AiError> {
        let jobs = self.lock()?;
        match jobs.get(id) {
            None => Err(AiError::invalid("unknown ArtCraft job")),
            Some(job) if job.snapshot.state.is_terminal() => Ok(()),
            Some(_) => Err(AiError::unsupported(
                "ArtCraft Omni API has no documented job cancellation route",
            )),
        }
    }
}

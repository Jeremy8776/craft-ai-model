//! BFL FLUX 3 image/video jobs via the official asynchronous API.
//! Polling uses our configured origin, never a server-supplied polling URL. Result media are
//! short-lived remote handles; callers must persist them if they need durable assets.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod http;
use craft_ai_core::{
    AiError, ErrorKind, Event, InputRole, JobId, JobSnapshot, JobState, ModelId, ModelInfo, Output,
    Payload, Provider, Request, SecretString, capability::CapabilityId, endpoint::Endpoint,
    model::CapabilitySpec, payload::Limits,
};
use http::{Http, encode_query};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Mutex, time::Duration};

#[derive(Clone, Debug)]
pub struct BflConfig {
    pub base_url: String,
    pub api_key: SecretString,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}
impl BflConfig {
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
    media_type: &'static str,
    snapshot: JobSnapshot,
}

pub struct BflProvider {
    http: Http,
    jobs: Mutex<HashMap<JobId, Job>>,
}
impl std::fmt::Debug for BflProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BflProvider")
    }
}
impl BflProvider {
    pub fn new(config: BflConfig) -> Result<Self, AiError> {
        if config.api_key.is_empty()
            || config
                .api_key
                .expose()
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(AiError::invalid("a single-line BFL API key is required"));
        }
        let base = config.base_url.trim_end_matches('/');
        let endpoint =
            Endpoint::parse(base).map_err(|_| AiError::invalid("invalid BFL API address"))?;
        if base.contains(['?', '#']) || (!endpoint.is_loopback() && base != "https://api.bfl.ai/v1")
        {
            return Err(AiError::invalid("BFL must use its official API address"));
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
        [
            ("flux-3-image", "FLUX 3 image", "image.generate"),
            ("flux-3-video", "FLUX 3 video", "video.generate"),
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
    fn job_lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<JobId, Job>>, AiError> {
        self.jobs
            .lock()
            .map_err(|_| AiError::new(ErrorKind::Internal, "BFL job registry unavailable"))
    }
}
impl Provider for BflProvider {
    fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        let value = self.http.get("/credits")?;
        if !value.get("credits").is_some_and(Value::is_number) {
            return Err(AiError::new(
                ErrorKind::Provider,
                "BFL credits reply is invalid",
            ));
        }
        Ok(Self::catalog())
    }
    fn submit(&self, model: &ModelId, request: &Request) -> Result<JobId, AiError> {
        let info = Self::catalog()
            .into_iter()
            .find(|m| &m.id == model)
            .ok_or_else(|| AiError::unsupported(format!("BFL does not serve model {model}")))?;
        let spec = info.spec_for(&request.capability).ok_or_else(|| {
            AiError::unsupported(format!("{model} cannot do {}", request.capability))
        })?;
        request.validate(spec, &Limits::default())?;
        if !request.params.is_empty() {
            return Err(AiError::unsupported(
                "BFL options are not mapped yet; use provider defaults",
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
            .ok_or_else(|| AiError::invalid("BFL needs a text prompt"))?;
        if prompt.is_empty() {
            return Err(AiError::invalid("BFL needs a non-empty prompt"));
        }
        let video = model.as_str() == "flux-3-video";
        let body = if video {
            json!({"prompt":prompt,"mode":"t2v"})
        } else {
            json!({"prompt":prompt})
        };
        let reply = self.http.post(
            if video {
                "/flux-3-video"
            } else {
                "/flux-3-image"
            },
            &body,
        )?;
        let remote = reply
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() < 256 && !s.chars().any(char::is_control))
            .ok_or_else(|| AiError::new(ErrorKind::Provider, "BFL task ID is invalid"))?;
        let id = JobId::new(format!("bfl-{remote}"))
            .map_err(|_| AiError::new(ErrorKind::Provider, "BFL task ID cannot be represented"))?;
        let mut jobs = self.job_lock()?;
        if jobs.len() >= 128 {
            return Err(AiError::new(ErrorKind::Quota, "too many tracked BFL jobs"));
        }
        jobs.insert(
            id.clone(),
            Job {
                remote_id: remote.to_owned(),
                media_type: if video { "video/mp4" } else { "image/png" },
                snapshot: JobSnapshot::new(JobState::Queued),
            },
        );
        Ok(id)
    }
    fn poll(&self, id: &JobId) -> Result<JobSnapshot, AiError> {
        let (remote, media, prior) = {
            let jobs = self.job_lock()?;
            let job = jobs
                .get(id)
                .ok_or_else(|| AiError::invalid("unknown BFL job"))?;
            if job.snapshot.state.is_terminal() {
                let mut stable = job.snapshot.clone();
                stable.events.clear();
                return Ok(stable);
            }
            (
                job.remote_id.clone(),
                job.media_type,
                job.snapshot.state.clone(),
            )
        };
        let reply = self
            .http
            .get(&format!("/get_result?id={}", encode_query(&remote)))?;
        if reply.get("id").and_then(Value::as_str) != Some(remote.as_str()) {
            return Err(AiError::new(
                ErrorKind::Provider,
                "BFL returned a different task ID",
            ));
        }
        let status = reply.get("status").and_then(Value::as_str).unwrap_or("");
        let mut snap = match status {
            "Pending" | "Task not found" => JobSnapshot::new(prior),
            "Reasoning" | "Generating" => JobSnapshot::new(JobState::Running { percent: None }),
            "Ready" => {
                let field = if media == "video/mp4" {
                    "video"
                } else {
                    "sample"
                };
                let uri = reply
                    .get("result")
                    .and_then(|v| v.get(field))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        AiError::new(ErrorKind::Provider, "BFL result has no media URL")
                    })?;
                let endpoint = Endpoint::parse(uri)
                    .map_err(|_| AiError::new(ErrorKind::Provider, "BFL result URL is invalid"))?;
                if !endpoint.https
                    || endpoint.is_loopback()
                    || uri.contains(['#', '?']) && uri.contains('@')
                {
                    return Err(AiError::new(
                        ErrorKind::Provider,
                        "BFL result URL is unsafe",
                    ));
                }
                let payload = Payload::handle(uri, media, None)?;
                let mut snap = JobSnapshot::new(JobState::Succeeded);
                snap.events.push(Event::Output(Output {
                    payload,
                    label: None,
                }));
                snap
            }
            "Error" | "Request Moderated" | "Content Moderated" => {
                JobSnapshot::new(JobState::Failed {
                    error: AiError::new(ErrorKind::Provider, format!("BFL task ended: {status}")),
                })
            }
            _ => {
                return Err(AiError::new(
                    ErrorKind::Provider,
                    "BFL returned an unknown status",
                ));
            }
        };
        let mut jobs = self.job_lock()?;
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
        let jobs = self.job_lock()?;
        match jobs.get(id) {
            None => Err(AiError::invalid("unknown BFL job")),
            Some(job) if job.snapshot.state.is_terminal() => Ok(()),
            Some(_) => Err(AiError::unsupported(
                "BFL API has no task cancellation route",
            )),
        }
    }
}

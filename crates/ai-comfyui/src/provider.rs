//! Local workflow tracer behind the shared Provider trait.
//! HTTP calls are synchronous and bounded; the caller should use its blocking worker pool.
use crate::Workflow;
use crate::http::Http;
use crate::output;
use craft_ai_core::endpoint::Endpoint;
use craft_ai_core::job::{JobSnapshot, JobState};
use craft_ai_core::model::InputRole;
use craft_ai_core::{AiError, ErrorKind, JobId, ModelId, ModelInfo, Provider, Request};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct ComfyConfig {
    pub base_url: String,
    pub workflows: Vec<Workflow>,
    /// An app-owned directory for durable results. Without it outputs stay inline (32 MiB cap).
    pub output_dir: Option<PathBuf>,
    /// Download cap in bytes when output_dir is set (default and maximum 1 GiB).
    pub max_output_bytes: u64,
    /// Largest source or mask image sent to ComfyUI, whether it arrives inline or as a file handle
    /// (default 32 MiB, never more than 256 MiB).
    pub max_upload_bytes: u64,
    /// Only opt in after confirming this server implements prompt-id-scoped /interrupt.
    pub targeted_interrupt: bool,
    /// Connection timeout for the HTTP agent (default 3 s).
    pub connect_timeout: Duration,
    /// Whole-request timeout for the HTTP agent (default 10 s; long jobs poll, so each
    /// individual call stays short).
    pub request_timeout: Duration,
}
impl ComfyConfig {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            workflows: Vec::new(),
            output_dir: None,
            max_output_bytes: 1024 * 1024 * 1024,
            max_upload_bytes: crate::input::DEFAULT_MAX_UPLOAD_BYTES,
            targeted_interrupt: false,
            connect_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_secs(10),
        }
    }
    /// This option must follow an explicit check of the installed ComfyUI server version.
    pub fn with_targeted_interrupt(mut self, confirmed: bool) -> Self {
        self.targeted_interrupt = confirmed;
        self
    }
    /// Persist finished media in an app-owned directory; the app controls its lifecycle.
    pub fn with_output_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.output_dir = Some(dir.into());
        self
    }
    /// Limit persisted downloads to at most 1 GiB, never zero bytes.
    pub fn with_max_output_bytes(mut self, bytes: u64) -> Self {
        self.max_output_bytes = bytes.clamp(1, 1024 * 1024 * 1024);
        self
    }
    /// Limit one uploaded source or mask image, never zero bytes and never over 256 MiB.
    pub fn with_max_upload_bytes(mut self, bytes: u64) -> Self {
        self.max_upload_bytes = bytes.clamp(1, crate::input::HARD_MAX_UPLOAD_BYTES);
        self
    }
    pub fn with_workflow(mut self, workflow: Workflow) -> Self {
        self.workflows.push(workflow);
        self
    }
}

struct Job {
    local_id: JobId,
    prompt_id: String,
    output_node: String,
    video: bool,
    snapshot: JobSnapshot,
}

pub struct ComfyProvider {
    http: Http,
    workflows: Vec<Workflow>,
    output_dir: Option<PathBuf>,
    max_output_bytes: u64,
    max_upload_bytes: u64,
    targeted_interrupt: bool,
    jobs: Mutex<Vec<Job>>,
}
impl std::fmt::Debug for ComfyProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ComfyProvider")
    }
}
impl ComfyProvider {
    pub fn new(config: ComfyConfig) -> Result<Self, AiError> {
        let base = config.base_url.trim_end_matches('/');
        let endpoint =
            Endpoint::parse(base).map_err(|_| AiError::invalid("invalid ComfyUI endpoint"))?;
        if !endpoint.is_loopback() || base != endpoint.fingerprint() {
            // A local API has no path prefix. Do not let a supplied path redirect API calls.
            return Err(AiError::invalid(
                "ComfyUI must use a loopback origin without a path",
            ));
        }
        for (i, w) in config.workflows.iter().enumerate() {
            if config.workflows[..i]
                .iter()
                .any(|other| other.id() == w.id())
            {
                return Err(AiError::invalid("duplicate workflow id"));
            }
        }
        Ok(Self {
            http: Http::new(
                base.to_owned(),
                config.connect_timeout,
                config.request_timeout,
            ),
            workflows: config.workflows,
            output_dir: config.output_dir,
            max_output_bytes: config.max_output_bytes.clamp(1, 1024 * 1024 * 1024),
            max_upload_bytes: config
                .max_upload_bytes
                .clamp(1, crate::input::HARD_MAX_UPLOAD_BYTES),
            targeted_interrupt: config.targeted_interrupt,
            jobs: Mutex::new(Vec::new()),
        })
    }
    fn jobs(&self) -> MutexGuard<'_, Vec<Job>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reads and checks one input image. Nothing is sent yet.
    fn read_role(
        &self,
        request: &Request,
        role: InputRole,
    ) -> Result<crate::input::Image, AiError> {
        let input = request
            .inputs
            .iter()
            .find(|i| i.role == role)
            .ok_or_else(|| AiError::invalid(format!("missing {role} input")))?;
        crate::input::read_image(&input.payload, self.max_upload_bytes)
    }

    fn upload(&self, role: InputRole, image: &crate::input::Image) -> Result<String, AiError> {
        self.http
            .upload_image(role.as_str(), &image.media_type, &image.data)
    }
}

impl Provider for ComfyProvider {
    fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        self.http.get_json("/system_stats")?;
        let installed = self.http.get_object_info()?;
        self.workflows
            .iter()
            .map(|w| {
                w.validate_installation(&installed)?;
                Ok(w.info())
            })
            .collect()
    }
    fn submit(&self, model: &ModelId, request: &Request) -> Result<JobId, AiError> {
        let workflow = self
            .workflows
            .iter()
            .find(|w| w.id() == model)
            .ok_or_else(|| AiError::unsupported("workflow is not configured"))?;
        workflow.validate_request(request)?;
        if workflow.capability().as_str() != "image.inpaint" {
            // Generation paths can render before any network traffic.
            workflow.render(request)?;
        }
        {
            let jobs = self.jobs();
            if jobs
                .iter()
                .filter(|j| !j.snapshot.state.is_terminal())
                .count()
                >= 8
            {
                return Err(AiError::new(
                    ErrorKind::RateLimited,
                    "too many local workflow jobs",
                ));
            }
        }
        let installed = self.http.get_object_info()?;
        workflow.validate_installation(&installed)?;
        let graph = if workflow.capability().as_str() == "image.inpaint" {
            // Both are read and checked before either is sent, so a bad mask costs no upload.
            let source = self.read_role(request, InputRole::Source)?;
            let mask = self.read_role(request, InputRole::Mask)?;
            let source = self.upload(InputRole::Source, &source)?;
            let mask = self.upload(InputRole::Mask, &mask)?;
            workflow.render_with_uploads(request, &source, &mask)?
        } else {
            workflow.render(request)?
        };
        let response = self.http.post("/prompt", &json!({"prompt":graph}))?;
        let prompt_id = response
            .get("prompt_id")
            .and_then(Value::as_str)
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            })
            .ok_or_else(|| {
                AiError::new(
                    ErrorKind::Provider,
                    "ComfyUI did not return a valid prompt id",
                )
            })?;
        let mut jobs = self.jobs();
        let id = JobId::new(format!("comfy-{}", jobs.len() + 1))
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not allocate job id"))?;
        jobs.push(Job {
            local_id: id.clone(),
            prompt_id: prompt_id.to_owned(),
            output_node: workflow.output_node().to_owned(),
            video: workflow.capability().as_str() == "video.generate",
            snapshot: JobSnapshot::new(JobState::Queued),
        });
        Ok(id)
    }
    fn poll(&self, id: &JobId) -> Result<JobSnapshot, AiError> {
        let mut jobs = self.jobs();
        let job = jobs
            .iter_mut()
            .find(|j| &j.local_id == id)
            .ok_or_else(|| AiError::invalid("unknown workflow job"))?;
        if job.snapshot.state.is_terminal() {
            return Ok(JobSnapshot {
                state: job.snapshot.state.clone(),
                events: Vec::new(),
                usage: None,
            });
        }
        let history = self.http.get_json(&format!("/history/{}", job.prompt_id))?;
        let Some(record) = history.get(&job.prompt_id) else {
            return Ok(JobSnapshot::new(job.snapshot.state.clone()));
        };
        if record.pointer("/status/status_str").and_then(Value::as_str) == Some("error") {
            job.snapshot.state = JobState::Failed {
                error: AiError::new(ErrorKind::Provider, "ComfyUI workflow execution failed"),
            };
        } else if record.pointer("/status/completed").and_then(Value::as_bool) == Some(true) {
            let node = record.get("outputs").and_then(|o| o.get(&job.output_node));
            let result = node
                .ok_or_else(|| {
                    AiError::new(
                        ErrorKind::Provider,
                        "workflow output node returned no media",
                    )
                })
                .and_then(|n| {
                    output::fetch(
                        &self.http,
                        n,
                        job.video,
                        self.output_dir.as_deref(),
                        self.max_output_bytes,
                    )
                });
            match result {
                Ok(event) => {
                    job.snapshot.state = JobState::Succeeded;
                    job.snapshot.events.push(event);
                }
                Err(error) => {
                    job.snapshot.state = JobState::Failed { error };
                }
            }
        } else {
            job.snapshot.state = JobState::Running { percent: None };
        }
        Ok(JobSnapshot {
            state: job.snapshot.state.clone(),
            events: std::mem::take(&mut job.snapshot.events),
            usage: None,
        })
    }
    fn cancel(&self, id: &JobId) -> Result<(), AiError> {
        let mut jobs = self.jobs();
        let job = jobs
            .iter_mut()
            .find(|j| &j.local_id == id)
            .ok_or_else(|| AiError::invalid("unknown workflow job"))?;
        if job.snapshot.state.is_terminal() {
            return Ok(());
        }
        if !self.targeted_interrupt {
            return Err(AiError::unsupported(
                "cancel requires a confirmed prompt-id-scoped ComfyUI interrupt route",
            ));
        }
        self.http
            .post("/queue", &json!({"delete":[job.prompt_id]}))?;
        self.http
            .post("/interrupt", &json!({"prompt_id":job.prompt_id}))?;
        job.snapshot.state = JobState::Cancelled;
        job.snapshot.events.clear();
        Ok(())
    }
}

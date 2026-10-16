//! Loopback-only SAM 3 bridge for text-prompted object detection.
//! The bridge lives in `bridge/server.py` and requires a separately acquired SAM checkpoint.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use craft_ai_core::{
    AiError, ErrorKind, Event, InputRole, JobId, JobSnapshot, JobState, ModelId, ModelInfo, Output,
    Payload, Provider, Request,
    capability::CapabilityId,
    endpoint::Endpoint,
    model::CapabilitySpec,
    payload::{Limits, base64_encode},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::Read,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Sam3Config {
    pub base_url: String,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}
impl Sam3Config {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            connect_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_secs(180),
        }
    }
}
pub struct Sam3Provider {
    base: String,
    agent: ureq::Agent,
    jobs: Mutex<HashMap<JobId, Option<JobSnapshot>>>,
    sequence: AtomicU64,
}
impl std::fmt::Debug for Sam3Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sam3Provider")
    }
}
impl Sam3Provider {
    pub fn new(config: Sam3Config) -> Result<Self, AiError> {
        let base = config.base_url.trim_end_matches('/');
        let endpoint =
            Endpoint::parse(base).map_err(|_| AiError::invalid("invalid SAM 3 bridge address"))?;
        if !endpoint.is_loopback() || base.contains(['?', '#']) {
            return Err(AiError::invalid("SAM 3 bridge must be on this computer"));
        }
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .tls_config(craft_ai_http::tls_config())
                .http_status_as_error(false)
                .max_redirects(0)
                .max_redirects_will_error(false)
                .timeout_connect(Some(config.connect_timeout))
                .timeout_global(Some(config.request_timeout))
                .build(),
        );
        Ok(Self {
            base: base.to_owned(),
            agent,
            jobs: Mutex::new(HashMap::new()),
            sequence: AtomicU64::new(0),
        })
    }
    fn lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<JobId, Option<JobSnapshot>>>, AiError> {
        self.jobs
            .lock()
            .map_err(|_| AiError::new(ErrorKind::Internal, "SAM 3 job registry unavailable"))
    }
    fn fetch(
        &self,
        response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<Value, AiError> {
        let response = response
            .map_err(|_| AiError::new(ErrorKind::Network, "could not reach local SAM 3 bridge"))?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(AiError::new(
                if status == 400 || status == 413 {
                    ErrorKind::InvalidRequest
                } else {
                    ErrorKind::Provider
                },
                format!("SAM 3 bridge answered {status}"),
            ));
        }
        let mut bytes = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| AiError::new(ErrorKind::Network, "SAM 3 reply ended early"))?;
        if bytes.len() > 1024 * 1024 {
            return Err(AiError::new(
                ErrorKind::Provider,
                "SAM 3 reply is too large",
            ));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| AiError::new(ErrorKind::Provider, "SAM 3 returned invalid JSON"))
    }
    fn catalog() -> Vec<ModelInfo> {
        match (ModelId::new("sam3"), CapabilityId::parse("image.detect")) {
            (Ok(id), Ok(cap)) => vec![
                ModelInfo::new(id, "SAM 3 object detection").with(
                    CapabilitySpec::new(cap)
                        .accepting(InputRole::Prompt, 1)
                        .accepting(InputRole::Source, 1)
                        .requiring(InputRole::Prompt)
                        .requiring(InputRole::Source),
                ),
            ],
            _ => Vec::new(),
        }
    }
}
impl Provider for Sam3Provider {
    fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        let url = format!("{}/health", self.base);
        let response = self.fetch(self.agent.get(url).call())?;
        if response.get("ready").and_then(Value::as_bool) != Some(true)
            || response.get("model").and_then(Value::as_str) != Some("sam3")
        {
            return Err(AiError::new(
                ErrorKind::Provider,
                "SAM 3 bridge is not ready",
            ));
        }
        Ok(Self::catalog())
    }
    fn submit(&self, model: &ModelId, request: &Request) -> Result<JobId, AiError> {
        let info = Self::catalog()
            .into_iter()
            .find(|m| &m.id == model)
            .ok_or_else(|| AiError::unsupported("SAM 3 model is unknown"))?;
        let spec = info
            .spec_for(&request.capability)
            .ok_or_else(|| AiError::unsupported("SAM 3 only supports image.detect"))?;
        request.validate(spec, &Limits::default())?;
        if !request.params.is_empty() {
            return Err(AiError::unsupported(
                "SAM 3 detection options are not mapped yet",
            ));
        }
        let prompt = request
            .inputs
            .iter()
            .find(|i| i.role == InputRole::Prompt)
            .and_then(|i| match &i.payload {
                Payload::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .filter(|p| !p.is_empty())
            .ok_or_else(|| AiError::invalid("SAM 3 needs a text prompt"))?;
        let source = request
            .inputs
            .iter()
            .find(|i| i.role == InputRole::Source)
            .ok_or_else(|| AiError::invalid("SAM 3 needs an image"))?;
        let Payload::Inline { media_type, data } = &source.payload else {
            return Err(AiError::unsupported(
                "SAM 3 needs an inline image, not a file or URL",
            ));
        };
        if !matches!(
            media_type.as_str(),
            "image/png" | "image/jpeg" | "image/webp"
        ) || data.len() > 16 * 1024 * 1024
            || data.is_empty()
        {
            return Err(AiError::unsupported(
                "SAM 3 accepts PNG, JPEG or WebP under 16 MiB",
            ));
        }
        if self.lock()?.len() >= 128 {
            return Err(AiError::new(
                ErrorKind::Quota,
                "too many tracked SAM 3 jobs",
            ));
        }
        let bytes = serde_json::to_vec(&json!({"prompt":prompt,"image":{"mime_type":media_type.as_str(),"data":base64_encode(data)}}))
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not encode SAM 3 image"))?;
        let url = format!("{}/detect", self.base);
        let response = self.fetch(
            self.agent
                .post(url)
                .content_type("application/json")
                .send(&bytes),
        )?;
        let boxes = response
            .get("boxes")
            .and_then(Value::as_array)
            .ok_or_else(|| AiError::new(ErrorKind::Provider, "SAM 3 returned no boxes"))?;
        let scores = response
            .get("scores")
            .and_then(Value::as_array)
            .ok_or_else(|| AiError::new(ErrorKind::Provider, "SAM 3 returned no scores"))?;
        if boxes.len() != scores.len()
            || boxes.len() > 256
            || !boxes.iter().all(|b| {
                b.as_array()
                    .is_some_and(|coords| coords.len() == 4 && coords.iter().all(Value::is_number))
            })
            || !scores.iter().all(Value::is_number)
            || !response.get("width").is_some_and(Value::is_number)
            || !response.get("height").is_some_and(Value::is_number)
        {
            return Err(AiError::new(
                ErrorKind::Provider,
                "SAM 3 returned malformed detection data",
            ));
        }
        let payload = Payload::inline(
            "application/json",
            serde_json::to_vec(&response)
                .map_err(|_| AiError::new(ErrorKind::Internal, "could not encode SAM 3 result"))?,
        )?;
        let id = JobId::new(format!(
            "sam3-{}",
            self.sequence.fetch_add(1, Ordering::Relaxed)
        ))
        .map_err(|_| AiError::new(ErrorKind::Internal, "cannot make SAM 3 job ID"))?;
        let mut snapshot = JobSnapshot::new(JobState::Succeeded);
        snapshot.events.push(Event::Output(Output {
            payload,
            label: None,
        }));
        self.lock()?.insert(id.clone(), Some(snapshot));
        Ok(id)
    }
    fn poll(&self, id: &JobId) -> Result<JobSnapshot, AiError> {
        let mut jobs = self.lock()?;
        let cell = jobs
            .get_mut(id)
            .ok_or_else(|| AiError::invalid("unknown SAM 3 job"))?;
        Ok(cell
            .take()
            .unwrap_or_else(|| JobSnapshot::new(JobState::Succeeded)))
    }
    fn cancel(&self, id: &JobId) -> Result<(), AiError> {
        if self.lock()?.contains_key(id) {
            Ok(())
        } else {
            Err(AiError::invalid("unknown SAM 3 job"))
        }
    }
}

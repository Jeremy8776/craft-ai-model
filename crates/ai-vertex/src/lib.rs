//! Vertex express-mode Gemini image generation using an API key.
//! Full-project Vertex OAuth/service accounts are a different connection type and are not
//! represented by this adapter. There is no known non-billable key probe: `models()` refuses
//! to claim that a saved key is verified. The preconfigured model can still be bound and used.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use craft_ai_core::{
    AiError, ErrorKind, Event, InputRole, JobId, JobSnapshot, JobState, ModelId, Output, Payload,
    Provider, Request, SecretString,
    endpoint::Endpoint,
    payload::{Limits, base64_decode},
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

const MODEL: &str = "gemini-2.5-flash-image";
#[derive(Clone, Debug)]
pub struct VertexConfig {
    pub base_url: String,
    pub api_key: SecretString,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}
impl VertexConfig {
    pub fn new(base_url: impl Into<String>, api_key: SecretString) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(90),
        }
    }
}
pub struct VertexProvider {
    base: String,
    key: SecretString,
    agent: ureq::Agent,
    jobs: Mutex<HashMap<JobId, Option<JobSnapshot>>>,
    sequence: AtomicU64,
}
impl std::fmt::Debug for VertexProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VertexProvider")
    }
}
impl VertexProvider {
    pub fn new(config: VertexConfig) -> Result<Self, AiError> {
        if config.api_key.is_empty()
            || config
                .api_key
                .expose()
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(AiError::invalid(
                "a single-line Vertex express API key is required",
            ));
        }
        let base = config.base_url.trim_end_matches('/');
        let endpoint = Endpoint::parse(base)
            .map_err(|_| AiError::invalid("invalid Vertex express API address"))?;
        if base.contains(['?', '#'])
            || (!endpoint.is_loopback() && base != "https://aiplatform.googleapis.com/v1")
        {
            return Err(AiError::invalid(
                "Vertex express must use the official API address",
            ));
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
            key: config.api_key,
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
            .map_err(|_| AiError::new(ErrorKind::Internal, "Vertex job registry unavailable"))
    }
    fn url(&self) -> String {
        let mut key = String::new();
        for byte in self.key.expose().bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                key.push(char::from(byte));
            } else {
                key.push_str(&format!("%{byte:02X}"));
            }
        }
        format!(
            "{}/publishers/google/models/{MODEL}:generateContent?key={key}",
            self.base
        )
    }
    fn generate(&self, prompt: &str) -> Result<Payload, AiError> {
        let request = json!({"contents":[{"role":"user","parts":[{"text":prompt}]}],
            "generationConfig":{"responseModalities":["TEXT","IMAGE"]}});
        let body = serde_json::to_vec(&request)
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not encode Vertex request"))?;
        let response = self
            .agent
            .post(self.url())
            .content_type("application/json")
            .send(&body)
            .map_err(|_| AiError::new(ErrorKind::Network, "could not reach Vertex"))?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            let kind = match status {
                401 | 403 => ErrorKind::Auth,
                429 => ErrorKind::RateLimited,
                400 | 404 | 405 | 413 | 422 => ErrorKind::InvalidRequest,
                _ => ErrorKind::Provider,
            };
            return Err(AiError::new(kind, format!("Vertex answered {status}")));
        }
        let mut bytes = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(48 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| AiError::new(ErrorKind::Network, "Vertex reply ended early"))?;
        if bytes.len() > 48 * 1024 * 1024 {
            return Err(AiError::new(
                ErrorKind::Provider,
                "Vertex image reply is too large",
            ));
        }
        let reply: Value = serde_json::from_slice(&bytes)
            .map_err(|_| AiError::new(ErrorKind::Provider, "Vertex returned invalid JSON"))?;
        let parts = reply
            .pointer("/candidates/0/content/parts")
            .and_then(Value::as_array)
            .ok_or_else(|| AiError::new(ErrorKind::Provider, "Vertex returned no image parts"))?;
        let data = parts
            .iter()
            .find_map(|p| p.get("inlineData"))
            .ok_or_else(|| AiError::new(ErrorKind::Provider, "Vertex returned no inline image"))?;
        let mime = data
            .get("mimeType")
            .and_then(Value::as_str)
            .ok_or_else(|| AiError::new(ErrorKind::Provider, "Vertex image has no media type"))?;
        if !matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
            return Err(AiError::new(
                ErrorKind::Provider,
                "Vertex returned an unsupported image format",
            ));
        }
        let raw = data
            .get("data")
            .and_then(Value::as_str)
            .and_then(base64_decode)
            .ok_or_else(|| AiError::new(ErrorKind::Provider, "Vertex image data is invalid"))?;
        Payload::inline(mime, raw)
    }
}
impl Provider for VertexProvider {
    fn models(&self) -> Result<Vec<craft_ai_core::ModelInfo>, AiError> {
        Err(AiError::unsupported(
            "Vertex express key cannot be checked without a billable generation; saved model is unverified",
        ))
    }
    fn submit(&self, model: &ModelId, request: &Request) -> Result<JobId, AiError> {
        if model.as_str() != MODEL {
            return Err(AiError::unsupported(
                "Vertex express serves only Gemini 2.5 Flash Image",
            ));
        }
        if request.capability.as_str() != "image.generate" {
            return Err(AiError::unsupported(
                "this Vertex model only generates images",
            ));
        }
        if request.outputs != 1
            || request.stream
            || !request.params.is_empty()
            || request.inputs.len() != 1
            || request.inputs[0].role != InputRole::Prompt
        {
            return Err(AiError::unsupported(
                "Vertex express first cut accepts one text prompt and one output with no extra options",
            ));
        }
        request.inputs[0].payload.check(&Limits::default())?;
        let Payload::Text { text } = &request.inputs[0].payload else {
            return Err(AiError::invalid("Vertex needs a text prompt"));
        };
        if text.is_empty() {
            return Err(AiError::invalid("Vertex needs a non-empty prompt"));
        }
        if self.lock()?.len() >= 128 {
            return Err(AiError::new(
                ErrorKind::Quota,
                "too many tracked Vertex jobs",
            ));
        }
        let payload = self.generate(text)?;
        let id = JobId::new(format!(
            "vertex-{}",
            self.sequence.fetch_add(1, Ordering::Relaxed)
        ))
        .map_err(|_| AiError::new(ErrorKind::Internal, "cannot make Vertex job ID"))?;
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
            .ok_or_else(|| AiError::invalid("unknown Vertex job"))?;
        Ok(cell
            .take()
            .unwrap_or_else(|| JobSnapshot::new(JobState::Succeeded)))
    }
    fn cancel(&self, id: &JobId) -> Result<(), AiError> {
        if self.lock()?.contains_key(id) {
            Ok(())
        } else {
            Err(AiError::invalid("unknown Vertex job"))
        }
    }
}

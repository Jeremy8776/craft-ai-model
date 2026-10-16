//! OpenAI-compatible provider for the Craft AI layer (tasks p1 and p2, first cut).
//!
//! Talks to any server that speaks the OpenAI REST shape: OpenAI itself, OpenRouter, Ollama,
//! llama.cpp, LM Studio, vLLM and others. Native only: it needs sockets and threads, so it lives
//! beside the wasm-safe model, never inside it.
//!
//! | Capability | Endpoint | Notes |
//! |---|---|---|
//! | `text.complete` | `/chat/completions` | streaming, tool calls, structured output |
//! | `image.generate` | `/images/generations` | up to the model's output limit |
//! | `image.edit` | `/images/edits` | source plus optional mask and references |
//! | `image.inpaint` | `/images/edits` | as edit, mask required |
//!
//! Not yet: audio, embeddings, image input to chat (vision), reading files or URLs for inputs, or
//! cost estimates. Each is a request the model's spec refuses with a clear `Unsupported` error.
//!
//! # Using it
//!
//! ```no_run
//! use craft_ai_core::{Provider, SecretString};
//! use craft_ai_openai::{OpenAiConfig, OpenAiProvider};
//!
//! let config = OpenAiConfig::new("https://api.openai.com/v1")
//!     .with_key(SecretString::new("sk-..."));
//! let provider = OpenAiProvider::new(config)?;
//! let models = provider.models()?;
//! # Ok::<(), craft_ai_core::AiError>(())
//! ```
//!
//! # Safety properties
//!
//! - The key is sent only as a bearer header to the configured base URL. Redirects are never
//!   followed, so a server cannot forward it elsewhere.
//! - Plain `http` is accepted only for loopback hosts (the model's endpoint rule).
//! - Errors are redacted with the key. Replies and stream lines are size-capped.
//! - Only listed request params are forwarded (see the `chat` and `images` module docs); unknown
//!   params are ignored, so a param can never override the URL, key or model.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod catalog;
mod chat;
mod http;
mod images;
mod jobs;
mod meta;
pub mod probe;

use std::sync::Arc;
use std::time::Duration;

use craft_ai_core::endpoint::Endpoint;
use craft_ai_core::payload::Limits;
use craft_ai_core::{
    AiError, JobId, JobSnapshot, ModelId, ModelInfo, Provider, Request, SecretString,
};

use crate::http::Http;
use crate::jobs::{Plan, Registry};

/// Settings for one OpenAI-compatible connection.
#[derive(Clone, Debug)]
pub struct OpenAiConfig {
    /// Base URL including the version path, such as `https://api.openai.com/v1` or
    /// `http://127.0.0.1:11434/v1`.
    pub base_url: String,
    /// Bearer key. `None` for local servers that need none.
    pub api_key: Option<SecretString>,
    /// Header that carries the key, for a server that does not take `Authorization: Bearer`.
    /// The key is then the header's whole value. `None` is the bearer default.
    pub key_header: Option<String>,
    /// Use these models instead of asking the server and guessing from ids.
    pub models: Option<Vec<ModelInfo>>,
    pub connect_timeout: Duration,
    /// Longest a whole request, including a stream, may take.
    pub request_timeout: Duration,
    /// Jobs that may run at once on this connection.
    pub max_active_jobs: usize,
}

impl OpenAiConfig {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: None,
            key_header: None,
            models: None,
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(600),
            max_active_jobs: 8,
        }
    }

    pub fn with_key(mut self, key: SecretString) -> Self {
        self.api_key = Some(key);
        self
    }

    pub fn with_key_header(mut self, name: impl Into<String>) -> Self {
        self.key_header = Some(name.into());
        self
    }

    pub fn with_models(mut self, models: Vec<ModelInfo>) -> Self {
        self.models = Some(models);
        self
    }
}

/// The provider.
pub struct OpenAiProvider {
    http: Arc<Http>,
    models: Option<Vec<ModelInfo>>,
    limits: Limits,
    registry: Registry,
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenAiProvider")
    }
}

impl OpenAiProvider {
    /// Checks the base URL and builds the provider. Nothing is sent yet.
    pub fn new(config: OpenAiConfig) -> Result<Self, AiError> {
        let base = config.base_url.trim().trim_end_matches('/').to_owned();
        if base.contains(['?', '#']) {
            return Err(AiError::invalid(
                "the base URL must not have a query or fragment",
            ));
        }
        Endpoint::parse(&base).map_err(|e| AiError::invalid(format!("base URL: {e}")))?;
        let http = Http::new(
            base,
            config.api_key,
            config.connect_timeout,
            config.request_timeout,
        )
        .with_key_header(config.key_header);
        Ok(Self {
            http: Arc::new(http),
            models: config.models,
            limits: Limits::default(),
            registry: Registry::new(config.max_active_jobs),
        })
    }

    fn model_info(&self, id: &ModelId) -> Option<ModelInfo> {
        match &self.models {
            Some(list) => list.iter().find(|m| &m.id == id).cloned(),
            None => catalog::classify(id.as_str()),
        }
    }
}

impl Provider for OpenAiProvider {
    fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        if let Some(list) = &self.models {
            return Ok(list.clone());
        }
        let v = self.http.get_json("/models")?;
        let openrouter = self.http.is_openrouter();
        let mut found: Vec<ModelInfo> = v
            .get("data")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let id = entry.get("id").and_then(serde_json::Value::as_str)?;
                let mut info = catalog::classify(id)?;
                if openrouter {
                    meta::add_openrouter_meta(&mut info, entry);
                }
                Some(info)
            })
            .collect();
        found.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        found.dedup_by(|a, b| a.id == b.id);
        Ok(found)
    }

    fn submit(&self, model: &ModelId, request: &Request) -> Result<JobId, AiError> {
        let info = self.model_info(model).ok_or_else(|| {
            AiError::unsupported(format!(
                "model {model} is not a chat or image model this adapter serves"
            ))
        })?;
        let spec = info.spec_for(&request.capability).ok_or_else(|| {
            AiError::unsupported(format!("model {model} cannot do {}", request.capability))
        })?;
        request.validate(spec, &self.limits)?;
        let plan = match request.capability.as_str() {
            "text.complete" => Plan::Chat(chat::plan(model.as_str(), request)?),
            "image.generate" => Plan::Image(images::generate_plan(model.as_str(), request)?),
            "image.edit" | "image.inpaint" => {
                Plan::Image(images::edit_plan(model.as_str(), request)?)
            }
            other => {
                return Err(AiError::unsupported(format!(
                    "this adapter does not serve {other}"
                )));
            }
        };
        self.registry.spawn(Arc::clone(&self.http), plan)
    }

    fn poll(&self, job: &JobId) -> Result<JobSnapshot, AiError> {
        self.registry
            .get(job)
            .map(|cell| cell.snapshot())
            .ok_or_else(|| AiError::invalid(format!("unknown job {job}")))
    }

    fn cancel(&self, job: &JobId) -> Result<(), AiError> {
        self.registry
            .get(job)
            .map(|cell| cell.cancel())
            .ok_or_else(|| AiError::invalid(format!("unknown job {job}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_rules() {
        assert!(OpenAiProvider::new(OpenAiConfig::new("https://api.openai.com/v1/")).is_ok());
        assert!(OpenAiProvider::new(OpenAiConfig::new("http://127.0.0.1:11434/v1")).is_ok());
        assert!(OpenAiProvider::new(OpenAiConfig::new("http://example.com/v1")).is_err());
        assert!(OpenAiProvider::new(OpenAiConfig::new("https://u:p@example.com/v1")).is_err());
        assert!(OpenAiProvider::new(OpenAiConfig::new("https://example.com/v1?x=1")).is_err());
        assert!(OpenAiProvider::new(OpenAiConfig::new("")).is_err());
        assert!(OpenAiProvider::new(OpenAiConfig::new("ftp://example.com")).is_err());
    }

    #[test]
    fn debug_never_shows_the_key() {
        let c =
            OpenAiConfig::new("https://example.com/v1").with_key(SecretString::new("sk-needle"));
        assert!(!format!("{c:?}").contains("needle"));
        let p = OpenAiProvider::new(c).unwrap();
        assert!(!format!("{p:?}").contains("needle"));
    }
}

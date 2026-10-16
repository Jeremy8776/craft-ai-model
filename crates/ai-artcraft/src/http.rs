//! Same-origin Omni API calls. No cookies, redirects or key-bearing error text.
use craft_ai_core::{AiError, ErrorKind, SecretString};
use serde_json::Value;
use std::{io::Read, time::Duration};
use ureq::{Body, http::Response};

pub(crate) struct Http {
    base: String,
    key: SecretString,
    agent: ureq::Agent,
}
impl Http {
    pub fn new(base: String, key: SecretString, connect: Duration, request: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .tls_config(craft_ai_http::tls_config())
            .http_status_as_error(false)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .timeout_connect(Some(connect))
            .timeout_global(Some(request))
            .build();
        Self {
            base,
            key,
            agent: ureq::Agent::new_with_config(config),
        }
    }
    fn decode(&self, reply: Result<Response<Body>, ureq::Error>) -> Result<Value, AiError> {
        let mut response = reply
            .map_err(|_| AiError::new(ErrorKind::Network, "could not reach ArtCraft Omni API"))?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            let kind = match status {
                401 | 403 => ErrorKind::Auth,
                402 => ErrorKind::Quota,
                429 => ErrorKind::RateLimited,
                400 | 404 | 405 | 413 | 422 => ErrorKind::InvalidRequest,
                _ => ErrorKind::Provider,
            };
            return Err(AiError::new(
                kind,
                format!("ArtCraft Omni API answered {status}"),
            ));
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                AiError::new(
                    ErrorKind::Network,
                    "ArtCraft API response broke while reading",
                )
            })?;
        if bytes.len() > 1024 * 1024 {
            return Err(AiError::new(
                ErrorKind::Provider,
                "ArtCraft API response is too large",
            ));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| AiError::new(ErrorKind::Provider, "ArtCraft API returned invalid JSON"))
    }
    pub fn get(&self, path: &str) -> Result<Value, AiError> {
        self.decode(
            self.agent
                .get(format!("{}{path}", self.base))
                .header("Authorization", format!("Bearer {}", self.key.expose()))
                .call(),
        )
    }
    pub fn post(&self, path: &str, body: &Value) -> Result<Value, AiError> {
        let bytes = serde_json::to_vec(body)
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not encode ArtCraft request"))?;
        self.decode(
            self.agent
                .post(format!("{}{path}", self.base))
                .header("Authorization", format!("Bearer {}", self.key.expose()))
                .content_type("application/json")
                .send(&bytes),
        )
    }
}

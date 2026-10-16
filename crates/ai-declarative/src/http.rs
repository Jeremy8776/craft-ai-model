//! Bounded, same-origin calls for a declarative route. The key goes in the one header the spec
//! names and nowhere else; error text is built from fixed words, never from a reply.

use std::io::Read;
use std::time::Duration;

use craft_ai_core::spec::{Method, SpecAuth};
use craft_ai_core::{AiError, ErrorKind, SecretString};
use serde_json::Value;
use ureq::{Body, http::Response};

/// Largest reply read, in bytes.
pub(crate) const MAX_REPLY: usize = 32 * 1024 * 1024;

pub(crate) struct Http {
    base: String,
    label: String,
    auth: SpecAuth,
    key: Option<SecretString>,
    agent: ureq::Agent,
}

impl Http {
    pub fn new(
        base: String,
        label: String,
        auth: SpecAuth,
        key: Option<SecretString>,
        connect: Duration,
        request: Duration,
    ) -> Self {
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
            label,
            auth,
            key,
            agent: ureq::Agent::new_with_config(config),
        }
    }

    fn url(&self, path: &str, query: &[(String, String)]) -> String {
        let mut url = format!("{}{path}", self.base);
        for (i, (name, value)) in query.iter().enumerate() {
            url.push(if i == 0 { '?' } else { '&' });
            url.push_str(&encode_query(name));
            url.push('=');
            url.push_str(&encode_query(value));
        }
        url
    }

    /// The key header for this connection, if the spec asks for one.
    fn key_header(&self) -> Option<(&str, String)> {
        let key = self.key.as_ref()?;
        match &self.auth {
            SpecAuth::Bearer => Some(("Authorization", format!("Bearer {}", key.expose()))),
            SpecAuth::Header { name } => Some((name.as_str(), key.expose().to_owned())),
            SpecAuth::None => None,
        }
    }

    pub fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Value, AiError> {
        let url = self.url(path, query);
        let key = self.key_header();
        let result = match method {
            Method::Get => {
                let mut r = self.agent.get(&url);
                if let Some((n, v)) = &key {
                    r = r.header(*n, v);
                }
                r.call()
            }
            Method::Delete => {
                let mut r = self.agent.delete(&url);
                if let Some((n, v)) = &key {
                    r = r.header(*n, v);
                }
                r.call()
            }
            Method::Post => {
                let mut r = self.agent.post(&url);
                if let Some((n, v)) = &key {
                    r = r.header(*n, v);
                }
                match body {
                    Some(b) => {
                        let bytes = serde_json::to_vec(b).map_err(|_| {
                            AiError::new(ErrorKind::Internal, "could not encode the request")
                        })?;
                        r.content_type("application/json").send(&bytes)
                    }
                    None => r.send_empty(),
                }
            }
        };
        self.reply(result)
    }

    fn reply(&self, response: Result<Response<Body>, ureq::Error>) -> Result<Value, AiError> {
        let label = &self.label;
        let mut response = response
            .map_err(|_| AiError::new(ErrorKind::Network, format!("could not reach {label}")))?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            let kind = match status {
                401 | 403 => ErrorKind::Auth,
                429 => ErrorKind::RateLimited,
                402 => ErrorKind::Quota,
                400 | 404 | 405 | 413 | 422 => ErrorKind::InvalidRequest,
                _ => ErrorKind::Provider,
            };
            return Err(AiError::new(kind, format!("{label} answered {status}")));
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(
                u64::try_from(MAX_REPLY)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
            )
            .read_to_end(&mut bytes)
            .map_err(|_| AiError::new(ErrorKind::Network, format!("{label} reply ended early")))?;
        if bytes.len() > MAX_REPLY {
            return Err(AiError::new(
                ErrorKind::Provider,
                format!("{label} reply is too large"),
            ));
        }
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            AiError::new(
                ErrorKind::Provider,
                format!("{label} returned invalid JSON"),
            )
        })
    }
}

pub(crate) fn encode_query(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

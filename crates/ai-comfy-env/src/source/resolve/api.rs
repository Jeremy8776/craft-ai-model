//! The listing requests the resolver makes: small JSON replies, no redirects, a token only for the
//! host its kind names, and every address checked against the kind's hosts first.

use std::io::Read;

use craft_ai_core::{AiError, ErrorKind, SecretString};
use craft_ai_media::links;
use serde_json::Value;
use url::Url;

use crate::source::hosts::{SourceKind, host_matches};
use crate::source::settings::SourceSettings;

/// Largest listing reply read, in bytes.
const MAX_REPLY_BYTES: u64 = 2 * 1024 * 1024;

pub(in crate::source) struct Api<'a> {
    agent: ureq::Agent,
    settings: &'a SourceSettings,
}

impl<'a> Api<'a> {
    pub fn new(settings: &'a SourceSettings) -> Self {
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .tls_config(craft_ai_http::tls_config())
                .http_status_as_error(false)
                .max_redirects(0)
                .max_redirects_will_error(false)
                .timeout_connect(Some(settings.connect_timeout))
                .timeout_global(Some(std::time::Duration::from_secs(60)))
                .user_agent("craft-ai/0.1")
                .build(),
        );
        Self { agent, settings }
    }

    pub fn get(
        &self,
        kind: SourceKind,
        url: &str,
        token: Option<&SecretString>,
    ) -> Result<Value, AiError> {
        let (url, token) = self.checked(kind, url, token)?;
        let mut request = self.agent.get(url.as_str());
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {}", token.expose()));
        }
        self.read(request.call())
    }

    pub fn post(
        &self,
        kind: SourceKind,
        url: &str,
        body: &Value,
        token: Option<&SecretString>,
    ) -> Result<Value, AiError> {
        let (url, token) = self.checked(kind, url, token)?;
        let mut request = self
            .agent
            .post(url.as_str())
            .content_type("application/json");
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {}", token.expose()));
        }
        self.read(request.send(body.to_string().as_bytes()))
    }

    /// The parsed address, and the token only if this address is the kind's token host.
    fn checked<'t>(
        &self,
        kind: SourceKind,
        url: &str,
        token: Option<&'t SecretString>,
    ) -> Result<(Url, Option<&'t SecretString>), AiError> {
        let parsed = Url::parse(url).map_err(|_| refused("the host's address is not valid"))?;
        links::check(&parsed, self.settings.allow_loopback)?;
        let host = parsed.host_str().unwrap_or_default();
        let allowed = self.settings.download_hosts(kind);
        if !allowed.iter().any(|pattern| host_matches(pattern, host)) {
            return Err(refused(
                "the address is not one of the hosts this kind of source uses",
            ));
        }
        let token = token.filter(|_| {
            self.settings
                .token_host(kind)
                .is_some_and(|t| host_matches(&t, host))
        });
        Ok((parsed, token))
    }

    fn read(
        &self,
        result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<Value, AiError> {
        let response = result
            .map_err(|_| AiError::new(ErrorKind::Network, "could not reach the model source"))?;
        let status = response.status().as_u16();
        match status {
            200..=299 => {}
            401 | 403 => {
                return Err(AiError::new(
                    ErrorKind::Auth,
                    "the source needs a token with access to this file; the licence may need accepting on the host first",
                ));
            }
            404 => {
                return Err(AiError::invalid(
                    "the source has no such file (check the address)",
                ));
            }
            429 => {
                return Err(AiError::new(
                    ErrorKind::RateLimited,
                    "the source is busy; try again later",
                ));
            }
            other => {
                return Err(AiError::new(
                    ErrorKind::Provider,
                    format!("the source answered {other}"),
                ));
            }
        }
        let mut bytes = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_REPLY_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                AiError::new(
                    ErrorKind::Network,
                    "the connection broke while reading the reply",
                )
            })?;
        if bytes.len() as u64 > MAX_REPLY_BYTES {
            return Err(refused("the source's reply was too large"));
        }
        serde_json::from_slice(&bytes).map_err(|_| refused("the source's reply was not readable"))
    }
}

pub(in crate::source) fn refused(why: &str) -> AiError {
    AiError::new(ErrorKind::Provider, why)
}

/// `base` followed by `pieces`, each encoded as one path piece.
pub(in crate::source) fn join(base: &str, pieces: &[&str]) -> Result<String, AiError> {
    let mut url = Url::parse(base).map_err(|_| refused("the source address is not valid"))?;
    url.path_segments_mut()
        .map_err(|()| refused("the source address is not valid"))?
        .pop_if_empty()
        .extend(pieces);
    Ok(url.to_string())
}

pub(in crate::source) fn is_hex(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(in crate::source) fn sane_size(size: Option<u64>) -> Result<u64, AiError> {
    size.filter(|s| (1..=crate::source::candidate::MAX_FILE_BYTES).contains(s))
        .ok_or_else(|| refused("the source gave no believable file size"))
}

/// A licence name safe to show: one short line.
pub(in crate::source) fn tidy(text: &str) -> String {
    let line: String = text.chars().filter(|c| !c.is_control()).take(80).collect();
    if line.trim().is_empty() {
        "unknown".to_owned()
    } else {
        line.trim().to_owned()
    }
}

//! The network side of [`Downloader`]: opening a link, following redirects by hand, and the two
//! small reads built on it (a size probe and a bounded in-memory body).
//!
//! Invariants: every hop passes the shared link rules and the allowed-host list; the token goes
//! only to `auth_host`; redirects are never followed by the HTTP client itself.

use craft_ai_core::{AiError, ErrorKind, SecretString};
use craft_ai_media::links;
use std::io::Read;
use url::Url;

use super::Downloader;
use crate::source::host_matches;

impl Downloader {
    /// The exact size of the file at `url`, from a one-byte range request (`Content-Range`) or,
    /// if the server ignores the range, its `Content-Length`. Nothing is saved. The same host
    /// and token rules apply as for a download. A server that states no length is refused.
    pub fn probe_size(&self, url: &str, token: Option<&SecretString>) -> Result<u64, AiError> {
        let response = self.open(url, Some("bytes=0-0".to_owned()), token)?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let size = match response.status().as_u16() {
            206 => header("content-range").and_then(|v| {
                v.rsplit_once('/')
                    .and_then(|(_, total)| total.parse::<u64>().ok())
            }),
            200 => header("content-length").and_then(|v| v.parse::<u64>().ok()),
            401 | 403 => {
                return Err(AiError::new(
                    ErrorKind::Auth,
                    "the source refused the request; the licence may need accepting or a token is needed",
                ));
            }
            404 => {
                return Err(AiError::new(
                    ErrorKind::Provider,
                    "the model file is no longer at its source",
                ));
            }
            other => {
                return Err(AiError::new(
                    ErrorKind::Provider,
                    format!("the source answered {other}"),
                ));
            }
        };
        size.filter(|s| *s > 0).ok_or_else(|| {
            AiError::new(
                ErrorKind::Provider,
                "the source does not say how large the file is",
            )
        })
    }

    /// The body at `url` (status 200 only), read whole into memory. Longer than `max_bytes` is
    /// refused, from the declared length if there is one and otherwise as the bytes arrive. The
    /// same host and token rules apply as for a download.
    pub fn get_bytes(
        &self,
        url: &str,
        max_bytes: u64,
        token: Option<&SecretString>,
    ) -> Result<Vec<u8>, AiError> {
        let response = self.open(url, None, token)?;
        match response.status().as_u16() {
            200 => {}
            401 | 403 => {
                return Err(AiError::new(
                    ErrorKind::Auth,
                    "the source refused the download",
                ));
            }
            404 => {
                return Err(AiError::new(
                    ErrorKind::Provider,
                    "the file is no longer at its source",
                ));
            }
            other => {
                return Err(AiError::new(
                    ErrorKind::Provider,
                    format!("the source answered {other}"),
                ));
            }
        }
        let too_large = || AiError::new(ErrorKind::Provider, "the download is larger than allowed");
        let declared = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        if declared.is_some_and(|d| d > max_bytes) {
            return Err(too_large());
        }
        let mut bytes = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| AiError::new(ErrorKind::Network, "the download was interrupted"))?;
        if bytes.len() as u64 > max_bytes {
            return Err(too_large());
        }
        Ok(bytes)
    }

    /// Opens the file's URL, following redirects by hand. The token goes only to `auth_host`.
    pub(super) fn open(
        &self,
        first: &str,
        range: Option<String>,
        token: Option<&SecretString>,
    ) -> Result<ureq::http::Response<ureq::Body>, AiError> {
        let mut url = Url::parse(first).map_err(|_| {
            AiError::new(ErrorKind::Internal, "the model source address is not valid")
        })?;
        let mut hops = 0u8;
        loop {
            links::check(&url, self.config.allow_loopback)?;
            if let Some(allowed) = &self.config.allowed_hosts {
                let host = url.host_str().unwrap_or_default();
                if !allowed.iter().any(|pattern| host_matches(pattern, host)) {
                    return Err(AiError::new(
                        ErrorKind::Provider,
                        "the address is not one of the hosts this kind of source uses",
                    ));
                }
            }
            let mut request = self.agent.get(url.as_str());
            if let Some(range) = &range {
                request = request.header("Range", range.as_str());
            }
            if let Some(token) = token
                && url
                    .host_str()
                    .is_some_and(|h| h.eq_ignore_ascii_case(&self.config.auth_host))
            {
                request = request.header("Authorization", format!("Bearer {}", token.expose()));
            }
            let response = request.call().map_err(|_| {
                AiError::new(ErrorKind::Network, "could not reach the model source")
            })?;
            if !(300..400).contains(&response.status().as_u16()) {
                return Ok(response);
            }
            hops += 1;
            if hops > self.config.max_redirects {
                return Err(AiError::new(
                    ErrorKind::Provider,
                    "the model source redirects too often",
                ));
            }
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            url = links::redirect_target(&url, location)?;
        }
    }
}

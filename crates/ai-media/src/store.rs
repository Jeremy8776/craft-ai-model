//! Copies one hosted media link into an app-owned directory.
//!
//! Invariants:
//! - No credential of any kind is sent. The request carries no authorization header and the agent
//!   keeps no cookies, so a delivery host or a redirect target never sees a provider key.
//! - Redirects are followed by hand, a few at most, and every hop passes the same URL rules as the
//!   first link. A hop may not downgrade from https.
//! - The body is bounded, identified by its first bytes (not by `Content-Type`), and written to a
//!   temporary file in the directory. Anything that fails leaves no file behind.
//! - The saved file's media type comes from its bytes. It must be the same kind (image, video,
//!   audio) the provider declared.
//! - A busy host is tried again, a few times, with a doubling pause. Only failures that can pass
//!   are retried (no connection, a cut transfer, 408, 429, 500, 502, 503, 504); an expired or
//!   refused link, a page that is not media, a file over the limit and a refused address are
//!   answered at once. A pause that would take longer than the retry budget is not taken: the
//!   store gives up and says so, rather than holding the job's worker for an hour because a host
//!   said `Retry-After: 3600`.

use crate::sniff::{self, HEAD_LEN};
use crate::url_rules;
use craft_ai_core::{AiError, ErrorKind, MediaType, Payload, PayloadHandle};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use url::Url;

/// How and where media is saved.
#[derive(Clone, Debug)]
pub struct MediaStoreConfig {
    /// An existing, absolute directory the app owns. The app decides when files are deleted.
    pub dir: PathBuf,
    /// Largest file accepted. Default 1 GiB.
    pub max_bytes: u64,
    pub connect_timeout: Duration,
    /// Time allowed for the whole transfer, including redirects. Default 30 minutes.
    pub transfer_timeout: Duration,
    pub max_redirects: u8,
    /// Permit links to a loopback host (http or https). For a connection the user runs on this
    /// computer, and for tests. Other private addresses stay refused.
    pub allow_loopback: bool,
    /// Most tries for one link, the first included. Clamped to 1..=5; 1 turns retries off.
    pub max_attempts: u8,
    /// Pause before the second try; each later pause doubles. Default 1 second.
    pub retry_delay: Duration,
    /// The most time the store will spend waiting between tries, in total (default 30 seconds).
    pub retry_budget: Duration,
}

impl MediaStoreConfig {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            max_bytes: 1024 * 1024 * 1024,
            connect_timeout: Duration::from_secs(15),
            transfer_timeout: Duration::from_secs(30 * 60),
            max_redirects: 3,
            allow_loopback: false,
            max_attempts: 3,
            retry_delay: Duration::from_secs(1),
            retry_budget: Duration::from_secs(30),
        }
    }
}

/// Saves hosted media to disk.
pub struct MediaStore {
    config: MediaStoreConfig,
    agent: ureq::Agent,
}

impl std::fmt::Debug for MediaStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaStore")
            .field("dir", &self.config.dir)
            .finish_non_exhaustive()
    }
}

impl MediaStore {
    pub fn new(config: MediaStoreConfig) -> Result<Self, AiError> {
        if !config.dir.is_absolute() || !config.dir.is_dir() {
            return Err(AiError::invalid(
                "the media directory must be an existing absolute folder",
            ));
        }
        if config.max_bytes == 0 {
            return Err(AiError::invalid("the media size limit must be above zero"));
        }
        let config = MediaStoreConfig {
            max_attempts: config.max_attempts.clamp(1, 5),
            ..config
        };
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .tls_config(craft_ai_http::tls_config())
                .http_status_as_error(false)
                .max_redirects(0)
                .max_redirects_will_error(false)
                .timeout_connect(Some(config.connect_timeout))
                .timeout_global(Some(config.transfer_timeout))
                .build(),
        );
        Ok(Self { config, agent })
    }

    /// True when the handle names a link we would download (not a `file:` or `data:` handle).
    pub fn is_remote(handle: &PayloadHandle) -> bool {
        let uri = handle.uri.as_bytes();
        let starts = |p: &[u8]| uri.len() >= p.len() && uri[..p.len()].eq_ignore_ascii_case(p);
        starts(b"http://") || starts(b"https://")
    }

    /// Downloads the link and returns a `file:` handle to the saved copy, trying again when the
    /// host is busy (see the module notes). Blocks for the pauses, so call it from a worker.
    pub fn save(&self, handle: &PayloadHandle) -> Result<PayloadHandle, AiError> {
        let started = Instant::now();
        let mut attempt = 1u8;
        loop {
            let error = match self.fetch_once(handle) {
                Ok(saved) => return Ok(saved),
                Err(error) => error,
            };
            if !error.is_retryable() || attempt >= self.config.max_attempts {
                return Err(tried(error, attempt));
            }
            let backoff = self.config.retry_delay.saturating_mul(1 << (attempt - 1));
            let wait = error
                .retry_after_secs
                .map_or(backoff, |secs| backoff.max(Duration::from_secs(secs)));
            if started.elapsed().saturating_add(wait) > self.config.retry_budget {
                return Err(tried(error, attempt));
            }
            std::thread::sleep(wait);
            attempt += 1;
        }
    }

    fn fetch_once(&self, handle: &PayloadHandle) -> Result<PayloadHandle, AiError> {
        let mut url = Url::parse(&handle.uri)
            .map_err(|_| AiError::new(ErrorKind::Provider, "media link is not a valid URL"))?;
        let mut hops = 0u8;
        let response = loop {
            url_rules::check_resolved(&url, self.config.allow_loopback)?;
            let response =
                self.agent.get(url.as_str()).call().map_err(|_| {
                    AiError::new(ErrorKind::Network, "could not download the media")
                })?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                hops += 1;
                if hops > self.config.max_redirects {
                    return Err(AiError::new(
                        ErrorKind::Provider,
                        "media link redirects too often",
                    ));
                }
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                let next = url_rules::redirect_target(&url, location)?;
                url = next;
                continue;
            }
            if status == 429 {
                let error =
                    AiError::new(ErrorKind::RateLimited, "the media host asked to slow down");
                return Err(match retry_after(&response) {
                    Some(secs) => error.with_retry_after(secs),
                    None => error,
                });
            }
            if matches!(status, 408 | 500 | 502 | 503 | 504) {
                return Err(AiError::new(
                    ErrorKind::Network,
                    format!("the media host answered {status}"),
                ));
            }
            if !(200..300).contains(&status) {
                return Err(AiError::new(
                    ErrorKind::Provider,
                    format!("media link answered {status}; it may have expired"),
                ));
            }
            break response;
        };
        if let Some(declared) = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            && declared > self.config.max_bytes
        {
            return Err(too_large());
        }
        self.write(response.into_body().into_reader(), &handle.media_type)
    }

    fn write(&self, mut body: impl Read, declared: &MediaType) -> Result<PayloadHandle, AiError> {
        let mut head = [0u8; HEAD_LEN];
        let mut filled = 0;
        while filled < HEAD_LEN {
            match body.read(&mut head[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => return Err(AiError::new(ErrorKind::Network, "media transfer failed")),
            }
        }
        let (media, extension) = sniff::media_type(&head[..filled])
            .ok_or_else(|| AiError::new(ErrorKind::Provider, "the link did not return media"))?;
        let sniffed = MediaType::new(media)
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not name the media type"))?;
        if sniffed.kind() != declared.kind() {
            return Err(AiError::new(
                ErrorKind::Provider,
                "the link returned a different kind of media than the provider declared",
            ));
        }
        let mut temp = tempfile::Builder::new()
            .prefix("craft-ai-")
            .suffix(&format!(".{extension}"))
            .tempfile_in(&self.config.dir)
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not create the media file"))?;
        let disk = |_| AiError::new(ErrorKind::Internal, "could not write the media file");
        temp.as_file_mut()
            .write_all(&head[..filled])
            .map_err(disk)?;
        let rest = std::io::copy(
            &mut body.take(self.config.max_bytes.saturating_sub(filled as u64) + 1),
            temp.as_file_mut(),
        )
        .map_err(|_| AiError::new(ErrorKind::Network, "media transfer failed"))?;
        let size = filled as u64 + rest;
        if size > self.config.max_bytes {
            return Err(too_large());
        }
        temp.as_file().sync_all().map_err(disk)?;
        let uri = Url::from_file_path(temp.path())
            .map_err(|()| AiError::new(ErrorKind::Internal, "could not name the media file"))?;
        temp.keep()
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not keep the media file"))?;
        match Payload::handle(uri.as_str(), media, Some(size))? {
            Payload::Handle(saved) => Ok(saved),
            _ => Err(AiError::new(
                ErrorKind::Internal,
                "could not describe the media file",
            )),
        }
    }
}

/// Seconds from a `Retry-After` header. A date form is ignored, so the normal pause applies.
fn retry_after(response: &ureq::http::Response<ureq::Body>) -> Option<u64> {
    response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
}

/// The final error, saying how many tries were made when there was more than one.
fn tried(error: AiError, attempts: u8) -> AiError {
    if attempts <= 1 {
        return error;
    }
    let retry_after = error.retry_after_secs;
    let error = AiError::new(
        error.kind,
        format!("{} (after {attempts} tries)", error.message),
    );
    match retry_after {
        Some(secs) => error.with_retry_after(secs),
        None => error,
    }
}

fn too_large() -> AiError {
    AiError::new(
        ErrorKind::Provider,
        "the media is larger than the configured limit",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A PNG header followed by endless zeros, counting what was pulled from it.
    struct Endless {
        sent: u64,
        pulled: Arc<AtomicU64>,
        limit: u64,
    }

    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
            let mut n = 0;
            for slot in buf.iter_mut() {
                if self.sent >= self.limit {
                    break;
                }
                *slot = header.get(self.sent as usize).copied().unwrap_or(0);
                self.sent += 1;
                n += 1;
            }
            self.pulled.fetch_add(n as u64, Ordering::SeqCst);
            Ok(n)
        }
    }

    #[test]
    fn the_running_limit_stops_the_transfer_not_just_the_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = MediaStoreConfig::new(dir.path());
        config.max_bytes = 100;
        let store = MediaStore::new(config).unwrap();
        let pulled = Arc::new(AtomicU64::new(0));
        let body = Endless {
            sent: 0,
            pulled: Arc::clone(&pulled),
            limit: 5 * 1024 * 1024,
        };
        let declared = MediaType::new("image/png").unwrap();
        let error = store.write(body, &declared).unwrap_err();
        assert!(error.message.contains("larger than"));
        assert!(pulled.load(Ordering::SeqCst) <= 101 + 8192, "{pulled:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}

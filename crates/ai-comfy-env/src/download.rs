//! Downloads one model file into a ComfyUI models folder, safely and resumably.
//!
//! Invariants:
//! - The file is written as `<name>.part` and renamed into place only after its size and SHA-256
//!   match the recipe, so a half-finished or wrong file never has the real name.
//! - An interrupted transfer keeps its `.part` and the next call continues from it with a
//!   `Range` request. A server that ignores the range makes the download start again.
//! - A Hugging Face token is sent only to `auth_host`. A redirect to the storage host that
//!   actually serves the bytes gets no `Authorization` header.
//! - Every hop passes the shared link rules (https, no user-info, no internal addresses).
//! - The transfer never writes more than the recipe's size.
//!
//! The link handling (redirects, host rules, token scoping) is in `transport`; this file holds the
//! resumable `.part` handling and the size and checksum checks.

use craft_ai_core::{AiError, ErrorKind, SecretString};
use craft_ai_features::ModelFile;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::verify::{hash_file, verify_file};

mod transport;

/// What the download is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Downloading,
    Verifying,
}

/// Bytes done out of the total for the current phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub phase: Phase,
    pub done: u64,
    pub total: u64,
}

/// Network behaviour of a [`Downloader`].
#[derive(Clone, Debug)]
pub struct DownloadConfig {
    pub connect_timeout: Duration,
    /// Longest one connection may take to deliver its body before it is treated as interrupted.
    pub body_timeout: Duration,
    pub max_redirects: u8,
    /// Extra connections tried, continuing from the `.part`, after an interruption that made progress.
    pub max_resumes: u8,
    pub retry_delay: Duration,
    /// The only host that receives the token.
    pub auth_host: String,
    /// When set, every address fetched, redirects included, must be on one of these hosts
    /// (`host` exactly, or `.host` for its subdomains). `None` leaves hosts to the shared link
    /// rules alone, as recipes do.
    pub allowed_hosts: Option<Vec<String>>,
    /// Permit links to a loopback host. For tests.
    pub allow_loopback: bool,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(15),
            body_timeout: Duration::from_secs(30 * 60),
            max_redirects: 5,
            max_resumes: 5,
            retry_delay: Duration::from_secs(2),
            auth_host: "huggingface.co".to_owned(),
            allowed_hosts: None,
            allow_loopback: false,
        }
    }
}

/// Downloads recipe files.
pub struct Downloader {
    agent: ureq::Agent,
    config: DownloadConfig,
}

/// Whether the finished file is compared with a checksum or its checksum is only read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    Expect,
    Observe,
}

enum Attempt {
    Done,
    /// The connection ended early; `moved` is whether any bytes arrived this time.
    Interrupted {
        moved: bool,
    },
}

impl Downloader {
    pub fn new(config: DownloadConfig) -> Self {
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .tls_config(craft_ai_http::tls_config())
                .http_status_as_error(false)
                .max_redirects(0)
                .max_redirects_will_error(false)
                .timeout_connect(Some(config.connect_timeout))
                .timeout_recv_body(Some(config.body_timeout))
                .build(),
        );
        Self { agent, config }
    }

    /// Makes sure `file` exists in `dest_dir`. An existing file of the right size is accepted
    /// without being read; use [`verify_file`] to hash it. Returns the final path.
    pub fn fetch(
        &self,
        file: &ModelFile,
        dest_dir: &Path,
        token: Option<&SecretString>,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<PathBuf, AiError> {
        self.fetch_checked(file, dest_dir, token, cancel, progress, Check::Expect)
            .map(|(path, _)| path)
    }

    /// Like [`fetch`](Self::fetch) for a source that publishes no checksum: `file.sha256` is
    /// ignored, the size is still enforced, and the SHA-256 of what arrived is returned. That
    /// digest is *observed*, not verified against anyone; the caller must say so and ask first.
    pub fn fetch_unverified(
        &self,
        file: &ModelFile,
        dest_dir: &Path,
        token: Option<&SecretString>,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<(PathBuf, String), AiError> {
        self.fetch_checked(file, dest_dir, token, cancel, progress, Check::Observe)
    }

    fn fetch_checked(
        &self,
        file: &ModelFile,
        dest_dir: &Path,
        token: Option<&SecretString>,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(Progress),
        check: Check,
    ) -> Result<(PathBuf, String), AiError> {
        if file.filename.is_empty()
            || file.filename.contains(['/', '\\', ':'])
            || file.filename.starts_with('.')
        {
            return Err(AiError::invalid("the model file name is not a plain name"));
        }
        std::fs::create_dir_all(dest_dir)
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not create the model folder"))?;
        let target = dest_dir.join(&file.filename);
        if check == Check::Expect
            && std::fs::metadata(&target).is_ok_and(|m| m.is_file() && m.len() == file.size_bytes)
        {
            return Ok((target, file.sha256.to_ascii_lowercase()));
        }
        let part = dest_dir.join(format!("{}.part", file.filename));
        let mut resumes = 0u8;
        loop {
            match self.attempt(file, &part, token, cancel, progress)? {
                Attempt::Done => break,
                Attempt::Interrupted { moved } => {
                    resumes += 1;
                    if !moved || resumes > self.config.max_resumes {
                        return Err(AiError::new(
                            ErrorKind::Network,
                            "the download was interrupted; run it again to continue",
                        ));
                    }
                    std::thread::sleep(self.config.retry_delay);
                }
            }
        }
        let checked = match check {
            Check::Expect => verify_file(&part, file, cancel, progress)
                .map(|()| file.sha256.to_ascii_lowercase()),
            Check::Observe => hash_file(&part, cancel, progress).and_then(|(size, digest)| {
                if size == file.size_bytes {
                    Ok(digest)
                } else {
                    Err(AiError::new(
                        ErrorKind::Provider,
                        "the model file does not have the size it should have",
                    ))
                }
            }),
        };
        let digest = match checked {
            Ok(digest) => digest,
            Err(error) => {
                if error.kind != ErrorKind::Cancelled {
                    let _ = std::fs::remove_file(&part);
                }
                return Err(error);
            }
        };
        // `rename` replaces an existing file in one step (on Windows too), so a failure here
        // leaves the old file where it was instead of deleting it first.
        std::fs::rename(&part, &target).map_err(|_| {
            AiError::new(
                ErrorKind::Internal,
                "could not move the finished file into place",
            )
        })?;
        Ok((target, digest))
    }

    fn attempt(
        &self,
        file: &ModelFile,
        part: &Path,
        token: Option<&SecretString>,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<Attempt, AiError> {
        let have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        if have > file.size_bytes {
            let _ = std::fs::remove_file(part);
            return self.attempt(file, part, token, cancel, progress);
        }
        if have == file.size_bytes {
            return Ok(Attempt::Done);
        }
        let from = (have > 0).then(|| format!("bytes={have}-"));
        let response = self.open(&file.source.url(), from, token)?;
        let status = response.status().as_u16();
        let append = match status {
            206 => {
                let starts = response
                    .headers()
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("bytes "))
                    .and_then(|v| v.split('-').next())
                    .and_then(|v| v.parse::<u64>().ok());
                if starts != Some(have) {
                    return Err(AiError::new(
                        ErrorKind::Provider,
                        "the server resumed at the wrong place",
                    ));
                }
                true
            }
            200 => false,
            416 => {
                let _ = std::fs::remove_file(part);
                return Ok(Attempt::Interrupted { moved: true });
            }
            401 | 403 => {
                return Err(AiError::new(
                    ErrorKind::Auth,
                    "the source refused the download; the licence may need accepting or a token is needed",
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
        let length = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let expected_body = if append {
            file.size_bytes - have
        } else {
            file.size_bytes
        };
        if length.is_some_and(|l| l != expected_body) {
            return Err(AiError::new(
                ErrorKind::Provider,
                "the source reports a different file size than expected",
            ));
        }
        let start = if append { have } else { 0 };
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(part)
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not open the download file"))?;
        self.copy(response, &mut out, start, file.size_bytes, cancel, progress)
    }

    fn copy(
        &self,
        response: ureq::http::Response<ureq::Body>,
        out: &mut std::fs::File,
        start: u64,
        total: u64,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<Attempt, AiError> {
        let mut body = response.into_body().into_reader().take(total - start + 1);
        let mut buffer = vec![0u8; 64 * 1024];
        let mut done = start;
        let mut reported = start;
        let mut moved = false;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(AiError::new(ErrorKind::Cancelled, "download cancelled"));
            }
            let n = match body.read(&mut buffer) {
                Ok(n) => n,
                Err(_) => return Ok(Attempt::Interrupted { moved }),
            };
            if n == 0 {
                break;
            }
            out.write_all(buffer.get(..n).unwrap_or_default())
                .map_err(|_| {
                    AiError::new(
                        ErrorKind::Internal,
                        "could not write the model file (is the disk full?)",
                    )
                })?;
            moved = true;
            done += n as u64;
            if done > total {
                return Err(AiError::new(
                    ErrorKind::Provider,
                    "the source sent more than the expected size",
                ));
            }
            if done - reported >= 1024 * 1024 || done == total {
                reported = done;
                progress(Progress {
                    phase: Phase::Downloading,
                    done,
                    total,
                });
            }
        }
        out.flush()
            .and_then(|()| out.sync_all())
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not save the model file"))?;
        Ok(if done == total {
            Attempt::Done
        } else {
            Attempt::Interrupted { moved }
        })
    }
}

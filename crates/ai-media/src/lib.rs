//! Durable local copies of media that hosted providers return as short-lived links.
//!
//! Hosted image and video providers answer a finished job with a URL on a delivery host, and that
//! URL expires. [`PersistingProvider`] wraps any [`craft_ai_core::Provider`]: when a poll carries
//! an output that is an http(s) link, it downloads the file into an app-owned directory and
//! replaces the link with a `file:` handle. Everything else passes through untouched.
//!
//! If a copy cannot be made (bad link, expired, too large, not media, disk problem) the job is
//! not failed, because the provider already did the paid work. The output keeps its original
//! link and a [`Event::Notice`] says no local copy was saved. The notice never contains the link,
//! which can embed a signed token.
//!
//! The app owns the directory, its retention and its cleanup. Downloads are synchronous inside
//! `poll`, which the client already runs on a worker thread.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

/// Media identified by its first bytes; shared with the adapters that check what they send or
/// receive.
pub mod sniff;
mod store;
mod url_rules;

pub use store::{MediaStore, MediaStoreConfig};

/// The link rules, for other downloaders that must follow the same ones.
pub mod links {
    pub use crate::url_rules::{check, check_resolved, redirect_target};
}

use craft_ai_core::{
    AiError, CostEstimate, Event, JobId, JobSnapshot, ModelId, ModelInfo, Payload, Provider,
    Request,
};
use std::sync::Arc;

/// A provider whose hosted outputs are copied to disk as they arrive.
pub struct PersistingProvider {
    inner: Arc<dyn Provider>,
    store: Arc<MediaStore>,
}

impl PersistingProvider {
    pub fn new(inner: Arc<dyn Provider>, store: Arc<MediaStore>) -> Self {
        Self { inner, store }
    }
}

impl Provider for PersistingProvider {
    fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        self.inner.models()
    }

    fn estimate(
        &self,
        model: &ModelId,
        request: &Request,
    ) -> Result<Option<CostEstimate>, AiError> {
        self.inner.estimate(model, request)
    }

    fn submit(&self, model: &ModelId, request: &Request) -> Result<JobId, AiError> {
        self.inner.submit(model, request)
    }

    fn poll(&self, job: &JobId) -> Result<JobSnapshot, AiError> {
        let mut snapshot = self.inner.poll(job)?;
        let mut notices = Vec::new();
        for event in &mut snapshot.events {
            let Event::Output(output) = event else {
                continue;
            };
            let Payload::Handle(handle) = &output.payload else {
                continue;
            };
            if !MediaStore::is_remote(handle) {
                continue;
            }
            match self.store.save(handle) {
                Ok(saved) => output.payload = Payload::Handle(saved),
                Err(error) => notices.push(Event::Notice {
                    message: format!("No local copy of the result was saved: {}", error.message),
                }),
            }
        }
        snapshot.events.extend(notices);
        Ok(snapshot)
    }

    fn cancel(&self, job: &JobId) -> Result<(), AiError> {
        self.inner.cancel(job)
    }
}

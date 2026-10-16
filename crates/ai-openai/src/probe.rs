//! Asking a custom server what it lists, for the connection form.
//!
//! `GET {base}/models` and nothing else, with the key sent only to that address (see
//! [`crate::OpenAiConfig`]; redirects are never followed). The answer is the server's ids, raw:
//! no guess about what each model can do is made here. A person (or the form, from
//! [`crate::catalog::suggest`]) declares that, so a custom connection never offers an ability
//! nobody confirmed.

use std::collections::BTreeSet;

use craft_ai_core::{AiError, ErrorKind, ModelId};
use serde_json::Value;

use crate::OpenAiConfig;
use crate::http::Http;

/// Most ids kept from one reply. A longer list is cut and marked, so a hostile or enormous server
/// cannot fill a form.
pub const MAX_LISTED: usize = 1000;

/// What `/models` said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    /// Distinct usable ids, sorted.
    pub ids: Vec<String>,
    /// Entries with no string id, an id this layer cannot carry, or beyond the cut.
    pub skipped: usize,
    /// The list was longer than [`MAX_LISTED`].
    pub truncated: bool,
}

/// Lists the model ids at `config.base_url`. Sends `config`'s key, if any, so to ask without a
/// key pass a config without one.
pub fn list_models(config: &OpenAiConfig) -> Result<Listing, AiError> {
    let provider = crate::OpenAiProvider::new(config.clone())?;
    let http: &Http = &provider.http;
    let reply = http.get_json("/models")?;
    let entries = reply.get("data").and_then(Value::as_array).ok_or_else(|| {
        http.scrub(AiError::new(
            ErrorKind::Provider,
            "the server's reply has no model list (expected a \"data\" array)",
        ))
    })?;
    let mut ids = BTreeSet::new();
    let mut skipped = 0;
    let mut truncated = false;
    for entry in entries {
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| ModelId::new(*id).is_ok());
        let Some(id) = id else {
            skipped += 1;
            continue;
        };
        if ids.len() >= MAX_LISTED && !ids.contains(id) {
            truncated = true;
            skipped += 1;
            continue;
        }
        ids.insert(id.to_owned());
    }
    Ok(Listing {
        ids: ids.into_iter().collect(),
        skipped,
        truncated,
    })
}

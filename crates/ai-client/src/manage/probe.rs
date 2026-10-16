//! The Test button for a custom connection: what the server answered, what that proves and what
//! it does not.
//!
//! A server that is not on the list speaks the OpenAI shape but nothing else is known about it.
//! So the test asks `GET /models` with the key (the one request that costs nothing), then asks
//! again without the key as a control, and reports in two lists:
//!
//! - **Confirmed**: only what the replies showed. A key counts as accepted only if the server
//!   refused the same request without it; a server that answers anyone has not checked it.
//! - **Unconfirmed**: what a free request cannot show, always including what each model can do.
//!
//! The model names come back with a suggested ability each, but nothing becomes a model of the
//! connection until a person declares it ([`declare_models`]). The key goes only to the
//! connection's own address, in the header it names, and a redirect is never followed.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use craft_ai_core::{AiError, Billing, Connection, ErrorKind, ModelInfo, SecretString};
use craft_ai_openai::OpenAiConfig;
use craft_ai_openai::catalog::{self, Kind};
use craft_ai_openai::probe::{MAX_LISTED, list_models};

/// The provider name of a connection to a server that is not on the list.
pub(super) const CUSTOM_PROVIDER: &str = "openai-compatible";

/// Most models one custom connection may declare.
pub const MAX_DECLARED: usize = 200;

const CONNECT: Duration = Duration::from_secs(5);
const REQUEST: Duration = Duration::from_secs(20);

/// Something the replies showed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Confirmed {
    /// The server returned a model list.
    ModelList { count: usize },
    /// The server answered with the key and refused the same request without it.
    KeyAccepted,
}

impl Confirmed {
    pub fn text(&self) -> String {
        match self {
            Self::ModelList { count } => format!("The server listed {count} models."),
            Self::KeyAccepted => {
                "The server accepted the key and refused a request without it.".to_owned()
            }
        }
    }
}

/// Something a free request cannot show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unconfirmed {
    /// What each model can do: the list has names only.
    Capabilities,
    /// How the provider bills, when the form says unknown.
    Billing,
    /// The server also answered without the key, so it may not check it.
    KeyNotEnforced,
    /// The request without the key failed for some other reason, so the key was not compared.
    KeyNotChecked,
    /// The server listed more models than are kept.
    ListCut,
}

impl Unconfirmed {
    pub fn text(&self) -> String {
        match self {
            Self::Capabilities => {
                "What each model can do. The server lists names only, so you choose.".to_owned()
            }
            Self::Billing => "How you are billed. Check the provider's own terms.".to_owned(),
            Self::KeyNotEnforced => {
                "Whether the key is checked. The server also answered without it.".to_owned()
            }
            Self::KeyNotChecked => {
                "Whether the key is checked. The test without it failed for another reason."
                    .to_owned()
            }
            Self::ListCut => format!(
                "The whole model list. The server listed more than {MAX_LISTED}, so only some are shown."
            ),
        }
    }
}

/// A model name the server listed, with a guess from the name to start the person's choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbedModel {
    pub id: String,
    /// A suggestion only. Empty when the name looks like something this layer does not serve.
    pub suggested: Vec<Kind>,
}

/// The result of testing a custom connection.
#[derive(Clone, Debug)]
pub struct ProbeReport {
    pub confirmed: Vec<Confirmed>,
    pub unconfirmed: Vec<Unconfirmed>,
    pub models: Vec<ProbedModel>,
    /// List entries that could not be used.
    pub skipped: usize,
    pub elapsed: Duration,
}

/// What a person decided one model does.
#[derive(Clone, Debug)]
pub struct ModelChoice {
    pub id: String,
    pub kinds: Vec<Kind>,
}

/// Turns the person's choices into the models a connection offers: exactly the declared
/// abilities, nothing guessed. Refuses repeats, a model with no ability, an id that cannot be
/// used, and more than [`MAX_DECLARED`].
pub fn declare_models(choices: &[ModelChoice]) -> Result<Vec<ModelInfo>, AiError> {
    if choices.len() > MAX_DECLARED {
        return Err(AiError::invalid(format!(
            "a connection can declare at most {MAX_DECLARED} models"
        )));
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(choices.len());
    for choice in choices {
        if !seen.insert(choice.id.as_str()) {
            return Err(AiError::invalid(format!(
                "model \"{}\" is listed twice",
                choice.id
            )));
        }
        let info = catalog::declare(&choice.id, &choice.kinds).ok_or_else(|| {
            AiError::invalid(format!(
                "model \"{}\" needs a usable name and at least one thing it does",
                choice.id
            ))
        })?;
        out.push(info);
    }
    Ok(out)
}

/// Tests a custom connection. `connection` is already checked (https unless loopback, no
/// user-info), and `secret` is its key if it uses one.
pub(super) fn run(
    connection: &Connection,
    secret: Option<SecretString>,
) -> Result<ProbeReport, AiError> {
    let endpoint = connection
        .endpoint
        .as_deref()
        .ok_or_else(|| AiError::invalid("this connection has no address to test"))?;
    let start = Instant::now();
    let mut config = OpenAiConfig::new(endpoint);
    config.connect_timeout = CONNECT;
    config.request_timeout = REQUEST;
    if let Some(name) = &connection.key_header {
        config = config.with_key_header(name.clone());
    }
    let mut keyed = config.clone();
    keyed.api_key = secret.clone();
    let listing = list_models(&keyed)?;

    let mut confirmed = vec![Confirmed::ModelList {
        count: listing.ids.len(),
    }];
    let mut unconfirmed = vec![Unconfirmed::Capabilities];
    if secret.as_ref().is_some_and(|k| !k.is_empty()) {
        // The control: the same request with no key at all.
        match list_models(&config) {
            Err(e) if e.kind == ErrorKind::Auth => confirmed.push(Confirmed::KeyAccepted),
            Ok(_) => unconfirmed.push(Unconfirmed::KeyNotEnforced),
            Err(_) => unconfirmed.push(Unconfirmed::KeyNotChecked),
        }
    }
    if connection.billing == Billing::Unknown {
        unconfirmed.push(Unconfirmed::Billing);
    }
    if listing.truncated {
        unconfirmed.push(Unconfirmed::ListCut);
    }
    Ok(ProbeReport {
        confirmed,
        unconfirmed,
        models: listing
            .ids
            .into_iter()
            .map(|id| ProbedModel {
                suggested: catalog::suggest(&id),
                id,
            })
            .collect(),
        skipped: listing.skipped,
        elapsed: start.elapsed(),
    })
}

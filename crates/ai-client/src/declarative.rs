//! Services described by a data file (`craft_ai_core::spec`): the factory entry that reads the
//! file for a connection and builds the runner.
//!
//! The file is `<specs folder>/<connection id>.json`. Two rules protect the person:
//! - the connection's address must be exactly the file's `base`, because "on this computer" and
//!   the agreement to send data are decided from the connection's address, and the file must not
//!   be able to send somewhere else;
//! - what a person reads here never names the file format or carries the key.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use craft_ai_core::endpoint::Endpoint;
use craft_ai_core::spec::Spec;
use craft_ai_core::{AiError, Connection, Provider, SecretString};
use craft_ai_declarative::{DeclarativeConfig, DeclarativeProvider};

/// Largest settings file read, a little over what the checker accepts.
const READ_LIMIT: u64 = 64 * 1024;

fn unreadable(connection: &Connection) -> AiError {
    AiError::invalid(format!(
        "This app can't read the settings for {}. Open Set up AI to fix or re-add them.",
        connection.label
    ))
}

fn load(connection: &Connection, dir: &std::path::Path) -> Result<Spec, AiError> {
    use std::io::Read;
    let file = dir.join(format!("{}.json", connection.id));
    let mut text = String::new();
    std::fs::File::open(&file)
        .and_then(|f| f.take(READ_LIMIT).read_to_string(&mut text))
        .map_err(|_| unreadable(connection))?;
    Spec::parse(&text).map_err(|_| unreadable(connection))
}

pub(crate) fn build(
    connection: &Connection,
    secret: Option<SecretString>,
    dir: Option<PathBuf>,
    timeouts: Option<(Duration, Duration)>,
) -> Result<Arc<dyn Provider>, AiError> {
    let dir = dir
        .or_else(craft_ai_native::paths::specs_dir)
        .ok_or_else(|| unreadable(connection))?;
    let spec = load(connection, &dir)?;
    let same_address = connection
        .endpoint
        .as_deref()
        .and_then(|e| Endpoint::parse(e).ok())
        .zip(Endpoint::parse(&spec.base).ok())
        .is_some_and(|(a, b)| a.fingerprint() == b.fingerprint());
    if !same_address {
        return Err(AiError::invalid(format!(
            "The settings for {} send to a different address than the connection. Open Set up AI to fix this.",
            connection.label
        )));
    }
    let mut config = DeclarativeConfig::new(spec, secret);
    if let Some((connect, request)) = timeouts {
        config.connect_timeout = connect;
        config.request_timeout = request;
    }
    Ok(Arc::new(DeclarativeProvider::new(config)?))
}

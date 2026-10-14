//! One TLS setting for every hosted call.
//!
//! Every adapter and downloader builds its own `ureq` agent, so the certificate rule lives here
//! once instead of ten times. The rule is: trust what the operating system trusts.
//!
//! The alternative, a bundled list of public roots (`ureq`'s default), fails on a machine where
//! antivirus or a company proxy re-signs HTTPS with its own root, which the operating system has
//! installed but a bundled list has not. The launcher hit exactly that. The price is that those
//! installed roots are trusted too, so software the person has chosen to install can read the
//! traffic, API keys included. For a desktop app acting for its user that is the usual rule;
//! a service that cannot accept it should not use this crate.
//!
//! `tests/every_agent_uses_it.rs` fails if any crate builds an agent without calling
//! [`tls_config`], so a new adapter cannot quietly go back to the bundled list.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ureq::tls::{RootCerts, TlsConfig};

/// The TLS setting for an agent: certificates are checked by the platform's own verifier.
///
/// Use it as `ureq::Agent::config_builder().tls_config(craft_ai_http::tls_config())`.
pub fn tls_config() -> TlsConfig {
    TlsConfig::builder()
        .root_certs(RootCerts::PlatformVerifier)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificates_are_checked_by_the_platform() {
        assert!(matches!(
            tls_config().root_certs(),
            RootCerts::PlatformVerifier
        ));
    }

    #[test]
    fn an_agent_built_with_it_keeps_the_setting() {
        let config = ureq::Agent::config_builder()
            .tls_config(tls_config())
            .build();
        assert!(matches!(
            config.tls_config().root_certs(),
            RootCerts::PlatformVerifier
        ));
    }
}

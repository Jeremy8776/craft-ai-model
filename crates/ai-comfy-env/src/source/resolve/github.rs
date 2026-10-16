//! GitHub release assets (public repositories): the digest GitHub computes at upload time, when
//! the asset has one. The download address is built here, never taken from the reply.

use craft_ai_core::{AiError, SecretString};
use craft_ai_features::Licence;
use serde_json::Value;

use super::api::{Api, is_hex, join, refused, sane_size, tidy};
use crate::source::candidate::{Candidate, licence_needs_accepting};
use crate::source::format::Format;
use crate::source::hosts::SourceKind;
use crate::source::settings::SourceSettings;

pub(super) fn resolve(
    settings: &SourceSettings,
    owner: &str,
    repo: &str,
    tag: &str,
    asset: &str,
    token: Option<&SecretString>,
) -> Result<Candidate, AiError> {
    let kind = SourceKind::GitHubRelease;
    let api = Api::new(settings);
    let release = api.get(
        kind,
        &join(
            &settings.bases.github_api,
            &["repos", owner, repo, "releases", "tags", tag],
        )?,
        token,
    )?;
    let entry = release
        .get("assets")
        .and_then(Value::as_array)
        .and_then(|assets| {
            assets
                .iter()
                .find(|a| a.get("name").and_then(Value::as_str) == Some(asset))
        })
        .ok_or_else(|| AiError::invalid("that release has no such file"))?;
    if entry.get("state").and_then(Value::as_str) != Some("uploaded") {
        return Err(refused("that release file has not finished uploading"));
    }
    let size = sane_size(entry.get("size").and_then(Value::as_u64))?;
    let sha256 = entry
        .get("digest")
        .and_then(Value::as_str)
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|hex| is_hex(hex, 64))
        .map(str::to_ascii_lowercase);

    // Best effort: a repository that states no licence is simply an unknown one.
    let licence_name = api
        .get(
            kind,
            &join(&settings.bases.github_api, &["repos", owner, repo])?,
            token,
        )
        .ok()
        .and_then(|r| {
            r.pointer("/license/spdx_id")
                .and_then(Value::as_str)
                .filter(|id| *id != "NOASSERTION")
                .map(tidy)
        })
        .unwrap_or_else(|| "unknown".to_owned());

    Ok(Candidate {
        kind,
        url: Some(join(
            &settings.bases.github_web,
            &[owner, repo, "releases", "download", tag, asset],
        )?),
        pin: tag.to_owned(),
        folder: String::new(),
        filename: asset.to_owned(),
        size_bytes: size,
        sha256,
        format: Format::from_name(asset),
        licence: Licence {
            accept_required: licence_needs_accepting(&licence_name),
            url: Some(join(&settings.bases.github_web, &[owner, repo])?),
            note: None,
            name: licence_name,
        },
        gated: false,
        local_path: None,
    })
}

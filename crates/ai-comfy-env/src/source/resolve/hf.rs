//! Hugging Face: pin the revision to a commit, then ask for that one file's size and LFS checksum.

use craft_ai_core::{AiError, SecretString};
use craft_ai_features::Licence;
use serde_json::{Value, json};

use super::api::{Api, is_hex, join, refused, sane_size, tidy};
use crate::source::candidate::{Candidate, licence_needs_accepting};
use crate::source::format::Format;
use crate::source::hosts::SourceKind;
use crate::source::settings::SourceSettings;

pub(super) fn resolve(
    settings: &SourceSettings,
    repo: &str,
    file: &str,
    revision: &str,
    token: Option<&SecretString>,
) -> Result<Candidate, AiError> {
    let kind = SourceKind::HuggingFace;
    let base = &settings.bases.huggingface;
    let api = Api::new(settings);
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| AiError::invalid("a Hugging Face repository is owner/name"))?;

    let info = api.get(
        kind,
        &join(base, &["api", "models", owner, name, "revision", revision])?,
        token,
    )?;
    let commit = info
        .get("sha")
        .and_then(Value::as_str)
        .filter(|s| is_hex(s, 40) && s.bytes().all(|b| !b.is_ascii_uppercase()))
        .ok_or_else(|| refused("the source did not name a commit"))?;
    let gated = match info.get("gated") {
        Some(Value::String(mode)) => mode != "false",
        Some(Value::Bool(flag)) => *flag,
        _ => false,
    };
    let licence_name = licence_of(&info);

    let listing = api.post(
        kind,
        &join(base, &["api", "models", owner, name, "paths-info", commit])?,
        &json!({"paths": [file]}),
        token,
    )?;
    let entry = listing
        .as_array()
        .and_then(|entries| {
            entries.iter().find(|e| {
                e.get("path").and_then(Value::as_str) == Some(file)
                    && e.get("type").and_then(Value::as_str) == Some("file")
            })
        })
        .ok_or_else(|| AiError::invalid("that file is not in the repository"))?;
    let size = sane_size(entry.get("size").and_then(Value::as_u64))?;
    let sha256 = match entry.get("lfs") {
        None | Some(Value::Null) => None,
        Some(lfs) => {
            let oid = lfs
                .get("oid")
                .and_then(Value::as_str)
                .filter(|o| is_hex(o, 64))
                .ok_or_else(|| refused("the source's checksum for this file is not readable"))?;
            if lfs
                .get("size")
                .and_then(Value::as_u64)
                .is_some_and(|s| s != size)
            {
                return Err(refused(
                    "the source reports two different sizes for this file",
                ));
            }
            Some(oid.to_ascii_lowercase())
        }
    };

    let mut pieces = vec![owner, name, "resolve", commit];
    pieces.extend(file.split('/'));
    Ok(Candidate {
        kind,
        url: Some(join(base, &pieces)?),
        pin: commit.to_owned(),
        folder: String::new(),
        filename: file.rsplit('/').next().unwrap_or(file).to_owned(),
        size_bytes: size,
        sha256,
        format: Format::from_name(file),
        licence: Licence {
            accept_required: gated || licence_needs_accepting(&licence_name),
            url: Some(join(base, &[owner, name])?),
            note: gated.then(|| {
                "The repository is gated: accept its terms on Hugging Face and use a token."
                    .to_owned()
            }),
            name: licence_name,
        },
        gated,
        local_path: None,
    })
}

fn licence_of(info: &Value) -> String {
    let from_card = info
        .pointer("/cardData/license")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let from_tags = info.get("tags").and_then(Value::as_array).and_then(|tags| {
        tags.iter()
            .filter_map(Value::as_str)
            .find_map(|t| t.strip_prefix("license:").map(str::to_owned))
    });
    tidy(&from_card.or(from_tags).unwrap_or_default())
}

//! Civitai: the model version's file list gives the name, SHA-256 and format; the exact size is
//! read from the download itself, because the list only states it in rounded kilobytes.

use craft_ai_core::{AiError, SecretString};
use craft_ai_features::Licence;
use serde_json::Value;
use url::Url;

use super::api::{Api, is_hex, join, refused, sane_size};
use crate::download::Downloader;
use crate::source::candidate::Candidate;
use crate::source::format::Format;
use crate::source::hosts::{SourceKind, host_matches};
use crate::source::settings::SourceSettings;
use crate::source::spec::plain_name;

const DOWNLOAD_PATH: &str = "/api/download/models/";

pub(super) fn resolve(
    settings: &SourceSettings,
    version_id: u64,
    token: Option<&SecretString>,
) -> Result<Candidate, AiError> {
    let kind = SourceKind::Civitai;
    let base = &settings.bases.civitai;
    let api = Api::new(settings);
    let id = version_id.to_string();
    let version = api.get(
        kind,
        &join(base, &["api", "v1", "model-versions", &id])?,
        token,
    )?;

    let files = version
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| refused("the source lists no files for this version"))?;
    let file = files
        .iter()
        .find(|f| f.get("primary").and_then(Value::as_bool) == Some(true))
        .or_else(|| {
            files
                .iter()
                .find(|f| f.get("type").and_then(Value::as_str) == Some("Model"))
        })
        .ok_or_else(|| refused("the source lists no model file for this version"))?;

    let name = file
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| plain_name(n))
        .ok_or_else(|| refused("the file name the source gave is not a plain name"))?;
    let format = Format::from_name(name);
    let download = file
        .get("downloadUrl")
        .and_then(Value::as_str)
        .and_then(|u| Url::parse(u).ok())
        .filter(|u| {
            u.path().starts_with(DOWNLOAD_PATH)
                && u.host_str().is_some_and(|h| {
                    settings
                        .download_hosts(kind)
                        .iter()
                        .any(|p| host_matches(p, h))
                })
        })
        .ok_or_else(|| refused("the download address the source gave is not one of its own"))?;
    let sha256 = match file.pointer("/hashes/SHA256") {
        None | Some(Value::Null) => None,
        Some(Value::String(hex)) if is_hex(hex, 64) => Some(hex.to_ascii_lowercase()),
        Some(_) => {
            return Err(refused(
                "the source's checksum for this file is not readable",
            ));
        }
    };

    let approximate = file.get("sizeKB").and_then(Value::as_f64).unwrap_or(0.0) * 1024.0;
    let exact = sane_size(Some(
        Downloader::new(settings.download_config(kind)).probe_size(download.as_str(), token)?,
    ))?;
    if (exact as f64 - approximate).abs() > 2048.0_f64.max(exact as f64 / 100.0) {
        return Err(refused(
            "the file size the source states does not match its download",
        ));
    }

    let model_page = match version.get("modelId").and_then(Value::as_u64) {
        Some(model) => Some(join(base, &["models", &model.to_string()])?),
        None => None,
    };
    Ok(Candidate {
        kind,
        url: Some(download.to_string()),
        pin: id,
        folder: folder_for(version.pointer("/model/type").and_then(Value::as_str)).to_owned(),
        filename: name.to_owned(),
        size_bytes: exact,
        sha256,
        format,
        licence: Licence {
            name: "Set by the creator".to_owned(),
            url: model_page,
            accept_required: true,
            note: Some("Civitai creators set their own terms; read the model page.".to_owned()),
        },
        gated: false,
        local_path: None,
    })
}

fn folder_for(model_type: Option<&str>) -> &'static str {
    match model_type {
        Some("Checkpoint") => "checkpoints",
        Some("LORA" | "LoCon" | "DoRA") => "loras",
        Some("TextualInversion") => "embeddings",
        Some("VAE") => "vae",
        Some("Controlnet") => "controlnet",
        Some("Upscaler") => "upscale_models",
        _ => "",
    }
}

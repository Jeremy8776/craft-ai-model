//! Turns a [`SourceSpec`] into a [`Candidate`]: asks the host (never a redirect target, never an
//! address off the kind's own hosts) for the file's exact size and published checksum, and refuses
//! a file whose name is not an accepted type before any request is made.

pub(in crate::source) mod api;
mod civitai;
mod github;
mod hf;

use craft_ai_core::{AiError, ErrorKind, SecretString};
use craft_ai_features::Licence;

use super::candidate::Candidate;
use super::format::Format;
use super::hosts::SourceKind;
use super::settings::SourceSettings;
use super::spec::SourceSpec;

pub struct Resolver {
    settings: SourceSettings,
}

impl Resolver {
    pub fn new(settings: SourceSettings) -> Self {
        Self { settings }
    }

    pub fn settings(&self) -> &SourceSettings {
        &self.settings
    }

    /// Describes the file `spec` names. `token` is used only for the host its kind names.
    pub fn resolve(
        &self,
        spec: &SourceSpec,
        token: Option<&SecretString>,
    ) -> Result<Candidate, AiError> {
        spec.validate()
            .map_err(|_| AiError::invalid("that address is not one this app can use"))?;
        let name = spec.file_name();
        // Civitai names its file in the reply; every other kind names it in the address.
        if let Some(name) = &name
            && Format::from_name(name) == Format::Unknown
        {
            return Err(unsupported(name));
        }
        match spec {
            SourceSpec::HuggingFace {
                repo,
                file,
                revision,
            } => hf::resolve(&self.settings, repo, file, revision, token),
            SourceSpec::GitHubRelease {
                owner,
                repo,
                tag,
                asset,
            } => github::resolve(&self.settings, owner, repo, tag, asset, token),
            SourceSpec::Civitai { version_id } => {
                let found = civitai::resolve(&self.settings, *version_id, token)?;
                if found.format == Format::Unknown {
                    return Err(unsupported(&found.filename));
                }
                Ok(found)
            }
            SourceSpec::Local { path } => local(path),
        }
    }
}

fn unsupported(name: &str) -> AiError {
    AiError::invalid(format!(
        "\"{name}\" is not a model file type this app installs (.safetensors, .gguf, .onnx, or a pickle format with your consent)"
    ))
}

fn local(path: &std::path::Path) -> Result<Candidate, AiError> {
    let meta =
        std::fs::metadata(path).map_err(|_| AiError::invalid("there is no file at that path"))?;
    if !meta.is_file() {
        return Err(AiError::invalid("that path is not a file"));
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| super::spec::plain_name(n))
        .ok_or_else(|| AiError::invalid("that file name cannot be installed"))?;
    if Format::from_name(name) == Format::Unknown {
        return Err(unsupported(name));
    }
    let size = api::sane_size(Some(meta.len()))
        .map_err(|_| AiError::new(ErrorKind::InvalidRequest, "that file is empty or too large"))?;
    Ok(Candidate {
        kind: SourceKind::Local,
        url: None,
        pin: "local".to_owned(),
        folder: String::new(),
        filename: name.to_owned(),
        size_bytes: size,
        sha256: None,
        format: Format::from_name(name),
        licence: Licence {
            name: "unknown".to_owned(),
            url: None,
            accept_required: true,
            note: Some("A file from this computer: its licence is yours to check.".to_owned()),
        },
        gated: false,
        local_path: Some(path.to_owned()),
    })
}

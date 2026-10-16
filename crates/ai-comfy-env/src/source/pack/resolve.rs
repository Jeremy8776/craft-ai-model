//! Pinning a GitHub repository to one commit and describing it for the person.

use craft_ai_core::{AiError, SecretString};
use serde_json::Value;

use super::{PackCandidate, PackSpec};
use crate::source::hosts::SourceKind;
use crate::source::resolve::Resolver;
use crate::source::resolve::api::{Api, is_hex, join, refused, tidy};
use crate::source::spec::revision_ok;

impl Resolver {
    /// Pins `spec` to a commit. Public repositories only: a private one is refused, because
    /// this app never holds a GitHub token. Nothing is downloaded.
    pub fn resolve_pack(
        &self,
        spec: &PackSpec,
        token: Option<&SecretString>,
    ) -> Result<PackCandidate, AiError> {
        let kind = SourceKind::NodePack;
        let settings = self.settings();
        let api = Api::new(settings);
        let (owner, repo) = (spec.owner.as_str(), spec.repo.as_str());
        let info = api.get(
            kind,
            &join(&settings.bases.github_api, &["repos", owner, repo])?,
            token,
        )?;
        if info.get("private").and_then(Value::as_bool) != Some(false) {
            return Err(refused("only public repositories can be installed"));
        }
        let reference = spec
            .reference
            .clone()
            .or_else(|| {
                info.get("default_branch")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|r| revision_ok(r))
            .ok_or_else(|| refused("the repository names no branch to install"))?;
        let commit = api.get(
            kind,
            &join(
                &settings.bases.github_api,
                &["repos", owner, repo, "commits", &reference],
            )?,
            token,
        )?;
        let sha = commit
            .get("sha")
            .and_then(Value::as_str)
            .filter(|s| is_hex(s, 40) && s.bytes().all(|b| !b.is_ascii_uppercase()))
            .ok_or_else(|| refused("the source did not name a commit"))?;
        let subject = commit
            .pointer("/commit/message")
            .and_then(Value::as_str)
            .map(|m| m.lines().next().unwrap_or_default())
            .map(subject_line)
            .unwrap_or_else(|| "(no message)".to_owned());
        let committed_at = commit
            .pointer("/commit/committer/date")
            .and_then(Value::as_str)
            .map(|d| d.chars().filter(|c| !c.is_control()).take(40).collect())
            .unwrap_or_else(|| "unknown".to_owned());
        let licence = info
            .pointer("/license/spdx_id")
            .and_then(Value::as_str)
            .filter(|id| *id != "NOASSERTION")
            .map_or_else(|| "unknown".to_owned(), tidy);
        Ok(PackCandidate {
            owner: owner.to_owned(),
            repo: repo.to_owned(),
            reference,
            commit: sha.to_owned(),
            subject,
            committed_at,
            licence,
            archived: info.get("archived").and_then(Value::as_bool) == Some(true),
            folder: repo.to_owned(),
            archive_url: join(
                &settings.bases.github_web,
                &[owner, repo, "archive", &format!("{sha}.tar.gz")],
            )?,
        })
    }
}

/// One short clean line: no control characters, at most 120 characters.
fn subject_line(line: &str) -> String {
    let clean: String = line.chars().filter(|c| !c.is_control()).take(120).collect();
    let clean = clean.trim();
    if clean.is_empty() {
        "(no message)".to_owned()
    } else {
        clean.to_owned()
    }
}

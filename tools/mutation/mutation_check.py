"""Deliberate-bug check for the connection, model-source, node-pack, declarative and client crates,
after `sam3_mutation_check.py`.

Each case breaks one safety rule in the source, runs the one test that should notice, and puts the
source back. A case is CAUGHT only if the mutated code compiled and the test run failed; a case that
does not compile counts as NOT CAUGHT (a bad mutant), so every survivor is either a test gap or a
mutant to fix. Cargo runs serially, targeted with -p, with -j 8.

Usage: mutation_check.py u1|u3|u4|p4|p10|v1|v2|v3|sd|lc|lr|nr|ct|u2 [name-substring]
Saved logs pass through scrub.py so no Windows account name is written. Run it from anywhere;
logs land in tools/mutation/verification/, which is not committed.
"""
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
from scrub import scrub  # noqa: E402

OUT = Path(__file__).resolve().parent / 'verification'
OUT.mkdir(exist_ok=True)
C = 'crates/'
CRLF, LF = '\r\n', '\n'
CONN = C + 'ai-core/src/connection.rs'
CUSTOM = C + 'ai-core/src/draft/custom.rs'
DRAFT = C + 'ai-core/src/draft.rs'
HTTP = C + 'ai-openai/src/http.rs'
PROBE_O = C + 'ai-openai/src/probe.rs'
CATALOG = C + 'ai-openai/src/catalog.rs'
PROBE_C = C + 'ai-client/src/manage/probe.rs'
MANAGE = C + 'ai-client/src/manage.rs'
FACTORY = C + 'ai-client/src/factory.rs'

PACK = C + 'ai-comfy-env/src/source/pack/mod.rs'
PACK_RESOLVE = C + 'ai-comfy-env/src/source/pack/resolve.rs'
PACK_INSTALL = C + 'ai-comfy-env/src/source/pack/install.rs'
PACK_ARCHIVE = C + 'ai-comfy-env/src/source/pack/archive.rs'
HOSTS = C + 'ai-comfy-env/src/source/hosts.rs'
FORMAT = C + 'ai-comfy-env/src/source/format.rs'
SPEC = C + 'ai-comfy-env/src/source/spec.rs'
DOWNLOAD = C + 'ai-comfy-env/src/download.rs'
TRANSPORT = C + 'ai-comfy-env/src/download/transport.rs'
HF = C + 'ai-comfy-env/src/source/resolve/hf.rs'
GH = C + 'ai-comfy-env/src/source/resolve/github.rs'
CIV = C + 'ai-comfy-env/src/source/resolve/civitai.rs'
API = C + 'ai-comfy-env/src/source/resolve/api.rs'
RESOLVE = C + 'ai-comfy-env/src/source/resolve/mod.rs'
REGISTRY = C + 'ai-comfy-env/src/source/registry.rs'
INSTALL = C + 'ai-comfy-env/src/source/install.rs'

M = 'craft-ai-core'
O = 'craft-ai-openai'
CL = 'craft-ai-client'
# (name, file, old, new, package, test target, test filter)
U1 = [
    ('header-length-unchecked', CONN, 'if name.is_empty() || name.len() > MAX_KEY_HEADER_BYTES {', 'if false {', M, 'custom_connection', 'header_names_that'),
    ('header-length-loosened', CONN, 'pub const MAX_KEY_HEADER_BYTES: usize = 64;', 'pub const MAX_KEY_HEADER_BYTES: usize = 65;', M, 'custom_connection', 'header_names_that'),
    ('header-chars-unrestricted', CONN, ".all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))", '.all(|_| true)', M, 'custom_connection', 'header_names_that'),
    ('proxy-headers-allowed', CONN, 'if lower.starts_with("proxy-") || lower.starts_with("sec-") {', 'if lower.starts_with("sec-") {', M, 'custom_connection', 'header_names_that'),
    ('transport-headers-allowed', CONN, 'if RESERVED_HEADERS.contains(&lower.as_str()) {', 'if false {', M, 'custom_connection', 'header_names_that'),
    ('stored-header-not-validated', CONN, 'check_key_header(name).map_err(ConnectionError::KeyHeader)?;', 'let _ = name;', M, 'custom_connection', 'stored_file'),
    ('stored-header-without-key-allowed', CONN, 'if self.auth.secret_ref().is_none() {\n                return Err(ConnectionError::KeyHeader(', 'if false {\n                return Err(ConnectionError::KeyHeader(', M, 'custom_connection', 'stored_file'),
    ('form-header-without-key-allowed', CUSTOM, 'if !self.uses_key {', 'if false {', M, 'custom_connection', 'without_a_key'),
    ('form-header-not-trimmed', CUSTOM, 'let name = self.key_header.trim();', 'let name = self.key_header.as_str();', M, 'custom_connection', 'named_header_is_kept'),
    ('form-header-dropped', DRAFT, 'enabled: self.enabled,\n            key_header,\n        };', 'enabled: self.enabled,\n            key_header: None,\n        };', M, 'custom_connection', 'named_header_is_kept'),
    ('edit-loses-header', DRAFT, 'key_header: c.key_header.clone().unwrap_or_default(),', 'key_header: String::new(),', M, 'custom_connection', 'named_header_is_kept'),
    ('custom-test-wrong-provider', CUSTOM, 'self.provider.trim() == "openai-compatible"', 'self.provider.trim() == "openai"', M, 'custom_connection', 'starts_from_the_other'),
    ('custom-billing-assumed-free', DRAFT, 'billing: Billing::Unknown,\n        needs_key: true,\n        note: "Any server that speaks the OpenAI API.",', 'billing: Billing::Free,\n        needs_key: true,\n        note: "Any server that speaks the OpenAI API.",', M, 'custom_connection', 'starts_from_the_other'),
    ('named-key-sent-as-bearer', HTTP, 'Some(name) => (name.clone(), key.expose().to_owned()),', 'Some(_) => ("Authorization".to_owned(), format!("Bearer {}", key.expose())),', O, 'custom', 'named_header_carries'),
    ('named-key-value-prefixed', HTTP, 'Some(name) => (name.clone(), key.expose().to_owned()),', 'Some(name) => (name.clone(), format!("Bearer {}", key.expose())),', O, 'custom', 'named_header_carries'),
    ('redirects-followed', HTTP, '.max_redirects(0)', '.max_redirects(5)', O, 'custom', 'redirect_never_carries'),
    ('error-text-not-redacted', HTTP, 'let mut e = scrub(AiError::new(kind, message), key);', 'let mut e = AiError::new(kind, message);', O, 'custom', 'echoes_a_named_key'),
    ('list-cut-disabled', PROBE_O, 'if ids.len() >= MAX_LISTED && !ids.contains(id) {', 'if false {', O, 'custom', 'huge_list'),
    ('list-cut-off-by-one', PROBE_O, 'if ids.len() >= MAX_LISTED && !ids.contains(id) {', 'if ids.len() > MAX_LISTED && !ids.contains(id) {', O, 'custom', 'huge_list'),
    ('list-keeps-unusable-ids', PROBE_O, '.filter(|id| ModelId::new(*id).is_ok());', '.filter(|_| true);', O, 'custom', 'raw_list_keeps'),
    ('list-error-unreadable', PROBE_O, "the server's reply has no model list (expected", "the server's reply is odd (expected", O, 'custom', 'without_a_data_list'),
    ('declared-chat-gets-features', CATALOG, 'Kind::Chat => plain_text_spec(),', 'Kind::Chat => text_spec(),', O, 'custom', 'declared_chat_model'),
    ('declared-image-many-outputs', CATALOG, 'Kind::ImageGenerate => generate_spec(1),', 'Kind::ImageGenerate => generate_spec(10),', O, 'custom', 'image_kinds_map'),
    ('declared-inpaint-without-mask', CATALOG, 'Kind::ImageInpaint => edit_spec("image.inpaint", 1, 0),', 'Kind::ImageInpaint => edit_spec("image.edit", 1, 0),', O, 'custom', 'image_kinds_map'),
    ('declared-nothing-accepted', CATALOG, 'if specs.is_empty() || specs.len() != kinds.len() {', 'if specs.len() != kinds.len() {', O, 'custom', 'nothing_is_declared'),
    ('declared-repeats-kept', CATALOG, 'kinds.sort();\n    kinds.dedup();\n    let specs', 'kinds.sort();\n    let specs', O, 'custom', 'image_kinds_map'),
    ('declared-order-unfixed', CATALOG, 'kinds.sort();\n    kinds.dedup();\n    let specs', 'kinds.dedup();\n    let specs', O, 'custom', 'image_kinds_map'),
    ('control-request-carries-key', PROBE_C, 'match list_models(&config) {', 'match list_models(&keyed) {', CL, 'custom', 'control_request_none'),
    ('key-credited-on-any-failure', PROBE_C, 'Err(e) if e.kind == ErrorKind::Auth => confirmed.push(Confirmed::KeyAccepted),', 'Err(_) => confirmed.push(Confirmed::KeyAccepted),', CL, 'custom', 'fails_for_another_reason'),
    ('open-server-credited', PROBE_C, 'Ok(_) => unconfirmed.push(Unconfirmed::KeyNotEnforced),', 'Ok(_) => {}', CL, 'custom', 'answers_without_the_key'),
    ('capabilities-forgotten', PROBE_C, 'let mut unconfirmed = vec![Unconfirmed::Capabilities];', 'let mut unconfirmed = vec![];', CL, 'custom', 'probe_confirms'),
    ('billing-always-unconfirmed', PROBE_C, 'if connection.billing == Billing::Unknown {', 'if true {', CL, 'custom', 'stated_billing'),
    ('billing-never-unconfirmed', PROBE_C, 'if connection.billing == Billing::Unknown {', 'if false {', CL, 'custom', 'probe_confirms'),
    ('cut-list-not-reported', PROBE_C, 'if listing.truncated {', 'if false {', CL, 'custom', 'cut_list'),
    ('probe-ignores-header-name', PROBE_C, 'config = config.with_key_header(name.clone());', 'let _ = name;', CL, 'custom', 'probe_confirms'),
    ('declared-repeats-allowed', PROBE_C, 'if !seen.insert(choice.id.as_str()) {', 'if !seen.insert(choice.id.as_str()) && false {', CL, 'custom', 'declared_models_are_checked'),
    ('declared-limit-loosened', PROBE_C, 'pub const MAX_DECLARED: usize = 200;', 'pub const MAX_DECLARED: usize = 201;', CL, 'custom', 'declared_models_are_checked'),
    ('declared-limit-unchecked', PROBE_C, 'if choices.len() > MAX_DECLARED {', 'if false {', CL, 'custom', 'declared_models_are_checked'),
    ('test-replaces-declared-models', MANAGE, 'if remember && report.probe.is_none() {', 'if remember {', CL, 'custom', 'never_replaces_declared'),
    ('factory-drops-header', FACTORY, 'config.key_header = connection.key_header.clone();', 'config.key_header = None;', CL, 'custom', 'named_header_from_the_saved'),
]

E = 'craft-ai-comfy-env'
U3 = [
    ('host-suffix-by-text', HOSTS, 'host.len() > rest.len() + 1 && host.ends_with(&format!(".{rest}"))', 'host.ends_with(rest)', E, 'source_basics', 'host_pattern_matches'),
    ('host-exact-by-suffix', HOSTS, 'None => host == pattern,', 'None => host.ends_with(&pattern),', E, 'source_basics', 'host_pattern_matches'),
    ('subdomain-pattern-matches-apex', HOSTS, 'host.len() > rest.len() + 1 && host.ends_with(&format!(".{rest}"))', 'host == rest || host.ends_with(&format!(".{rest}"))', E, 'source_basics', 'host_pattern_matches'),
    ('github-gets-a-token-host', HOSTS, 'download: &["github.com", ".githubusercontent.com"],\n                token_host: None,', 'download: &["github.com", ".githubusercontent.com"],\n                token_host: Some("github.com"),', E, 'source_basics', 'names_its_own_hosts'),
    ('hugging-face-token-host-widened', HOSTS, 'download: &["huggingface.co", ".huggingface.co", ".hf.co"],\n                token_host: Some("huggingface.co"),', 'download: &["huggingface.co", ".huggingface.co", ".hf.co"],\n                token_host: Some(".hf.co"),', E, 'source_basics', 'names_its_own_hosts'),
    ('pickle-runs-no-code', FORMAT, 'self == Self::Pickle', 'false', E, 'source_basics', 'extension_and_pickle'),
    ('bin-not-pickle', FORMAT, '"ckpt" | "pt" | "pth" | "bin" | "pkl" | "pickle"', '"ckpt" | "pt" | "pth" | "pkl" | "pickle"', E, 'source_basics', 'extension_and_pickle'),
    ('zip-not-sniffed', FORMAT, 'if head.starts_with(b"PK\\x03\\x04") ||', 'if false ||', E, 'source_basics', 'bytes_are_read'),
    ('safetensors-header-unbounded', FORMAT, '<= MAX_SAFETENSORS_HEADER', '<= u64::MAX', E, 'source_basics', 'bytes_are_read'),
    ('mislabel-unchecked', FORMAT, 'Self::Safetensors | Self::Gguf if found != claims =>', 'Self::Safetensors | Self::Gguf if false =>', E, 'source_basics', 'what_its_name_says'),
    ('onnx-pickle-allowed', FORMAT, 'Self::Onnx if found != Self::Unknown =>', 'Self::Onnx if false =>', E, 'source_basics', 'what_its_name_says'),
    ('plain-http-accepted', SPEC, '"http" => return Err(SpecError::Insecure),', '"http" => {}', E, 'source_basics', 'could_point_somewhere'),
    ('user-info-accepted', SPEC, 'if !url.username().is_empty() || url.password().is_some() {', 'if false {', E, 'source_basics', 'could_point_somewhere'),
    ('host-matched-by-suffix', SPEC, 'Some("huggingface.co") => hugging_face(&parts),', 'Some(h) if h.ends_with("huggingface.co") => hugging_face(&parts),', E, 'source_basics', 'could_point_somewhere'),
    ('path-not-decoded', SPEC, 'let decoded = percent_decode_str(piece)\n                .decode_utf8()\n                .map_err(|_| SpecError::BadName)?;', 'let decoded = std::borrow::Cow::Borrowed(piece);', E, 'source_basics', 'could_point_somewhere'),
    ('dotfiles-accepted', SPEC, '&& !segment.starts_with(\'.\')', '&& true', E, 'source_basics', 'could_point_somewhere'),
    ('path-characters-unchecked', SPEC, '.any(|c| c.is_control() || "/\\\\:*?\\"<>|".contains(c))', '.any(|_| false)', E, 'source_basics', 'could_point_somewhere'),
    ('local-parent-dir-accepted', SPEC, 'if path.components().any(|c| c == Component::ParentDir) {\n            return Err(SpecError::BadName);\n        }\n        return Ok(SourceSpec::Local', 'if false {\n            return Err(SpecError::BadName);\n        }\n        return Ok(SourceSpec::Local', E, 'source_basics', 'absolute_path'),
    ('revision-unchecked', SPEC, 'if !revision_ok(revision) {', 'if false {', E, 'source_basics', 'could_point_somewhere'),
    ('download-hosts-unchecked', TRANSPORT, 'if !allowed.iter().any(|pattern| host_matches(pattern, host)) {', 'if false {', E, 'download_sources', 'allowed_hosts'),
    ('size-probe-asks-for-more', TRANSPORT, 'Some("bytes=0-0".to_owned())', 'Some("bytes=0-1".to_owned())', E, 'download_sources', 'one_byte_range'),
    ('size-probe-accepts-zero', TRANSPORT, '.filter(|s| *s > 0)', '.filter(|_| true)', E, 'download_sources', 'one_byte_range'),
    ('commit-not-required', HF, '.filter(|s| is_hex(s, 40) && s.bytes().all(|b| !b.is_ascii_uppercase()))', '.filter(|_| true)', E, 'source_resolve', 'asked_file_are_refused'),
    ('lfs-size-mismatch-ignored', HF, '.is_some_and(|s| s != size)', '.is_some_and(|s| s == 0)', E, 'source_resolve', 'host_got_wrong'),
    ('lfs-checksum-unchecked', HF, '.filter(|o| is_hex(o, 64))', '.filter(|_| true)', E, 'source_resolve', 'host_got_wrong'),
    ('gated-ignored', HF, 'Some(Value::String(mode)) => mode != "false",', 'Some(Value::String(_)) => false,', E, 'source_resolve', 'gated_repo_needs'),
    ('gate-alone-needs-no-acceptance', HF, 'accept_required: gated || licence_needs_accepting(&licence_name),', 'accept_required: licence_needs_accepting(&licence_name),', E, 'source_resolve', 'gated_repo_needs'),
    ('directory-accepted-as-file', HF, '&& e.get("type").and_then(Value::as_str) == Some("file")', '&& true', E, 'source_resolve', 'asked_file_are_refused'),
    ('other-path-accepted', HF, 'e.get("path").and_then(Value::as_str) == Some(file)', 'true', E, 'source_resolve', 'asked_file_are_refused'),
    ('token-sent-to-any-host', API, '.is_some_and(|t| host_matches(&t, host))', '.is_some()', E, 'source_resolve', 'token_goes_only'),
    ('listing-host-unchecked', API, 'if !allowed.iter().any(|pattern| host_matches(pattern, host)) {', 'if false {', E, 'source_resolve', 'obey_the_hosts'),
    ('listing-reply-unbounded', API, 'if bytes.len() as u64 > MAX_REPLY_BYTES {', 'if false {', E, 'source_resolve', 'enormous_reply'),
    ('listing-follows-redirects', API, '.max_redirects(0)', '.max_redirects(5)', E, 'source_resolve', 'redirect_from_the_host'),
    ('unfinished-asset-accepted', GH, 'if entry.get("state").and_then(Value::as_str) != Some("uploaded") {', 'if false {', E, 'source_resolve', 'missing_or_unfinished'),
    ('other-digest-algorithm-accepted', GH, '.and_then(|d| d.strip_prefix("sha256:"))', '.and_then(|d| d.split_once(\':\').map(|x| x.1))', E, 'source_resolve', 'without_a_usable_digest'),
    ('civitai-size-crosscheck-removed', CIV, 'if (exact as f64 - approximate).abs() > 2048.0_f64.max(exact as f64 / 100.0) {', 'if false {', E, 'source_resolve', 'pickle_file_is_flagged'),
    ('civitai-download-path-unchecked', CIV, 'u.path().starts_with(DOWNLOAD_PATH)\n                && u.host_str()', 'true\n                && u.host_str()', E, 'source_resolve', 'pickle_file_is_flagged'),
    ('civitai-checksum-unchecked', CIV, 'Some(Value::String(hex)) if is_hex(hex, 64) =>', 'Some(Value::String(hex)) =>', E, 'source_resolve', 'pickle_file_is_flagged'),
    ('civitai-name-unchecked', CIV, '.filter(|n| plain_name(n))', '.filter(|_| true)', E, 'source_resolve', 'pickle_file_is_flagged'),
    ('unsupported-type-resolved', RESOLVE, '&& Format::from_name(name) == Format::Unknown\n        {\n            return Err(unsupported(name));', '&& false\n        {\n            return Err(unsupported(name));', E, 'source_resolve', 'unsupported_file_type'),
    ('directory-taken-as-local-file', RESOLVE, 'if !meta.is_file() {', 'if false {', E, 'source_resolve', 'this_computer_is_described'),
    ('licence-record-ignores-version', REGISTRY, '&& e.pin == candidate.pin\n', '\n', E, 'source_install', 'licence_is_accepted_once'),
    ('licence-record-ignores-link', REGISTRY, '&& e.licence.url == candidate.licence.url\n', '\n', E, 'source_install', 'licence_is_accepted_once'),
    ('licence-record-ignores-kind', REGISTRY, 'e.licence_accepted.is_some()\n                    && e.kind == candidate.kind', 'e.licence_accepted.is_some()\n                    && true', E, 'source_install', 'licence_is_accepted_once'),
    ('local-files-share-a-licence', REGISTRY, '&& (candidate.kind != SourceKind::Local || e.filename == candidate.filename)', '', E, 'source_install', 'copied_and_the_original'),
    ('pickle-consent-ignored', INSTALL, 'if needs.pickle && !consents.pickle {', 'if false {', E, 'source_install', 'pickle_formats_are_refused'),
    ('licence-consent-ignored', INSTALL, 'if needs.licence && !consents.licence && !self.registry.licence_on_record(candidate) {', 'if false {', E, 'source_install', 'licence_is_accepted_once'),
    ('unverified-consent-ignored', INSTALL, 'if needs.unverified && !consents.unverified {', 'if false {', E, 'source_install', 'no_published_checksum'),
    ('gated-without-token-allowed', INSTALL, 'if needs.token && !has_token {', 'if false {', E, 'source_install', 'gated_file_needs'),
    ('existing-file-overwritten', INSTALL, 'if std::fs::symlink_metadata(target_dir.join(&candidate.filename)).is_ok() {', 'if false {', E, 'source_install', 'never_overwritten'),
    ('space-margin-dropped', INSTALL, 'let needed = candidate.size_bytes.saturating_add(MARGIN_BYTES);', 'let needed = candidate.size_bytes;', E, 'source_install', 'disk_space_is_checked'),
    ('space-boundary-off-by-one', INSTALL, 'available < needed', 'available <= needed', E, 'source_install', 'disk_space_is_checked'),
    ('space-reads-the-roomier-folder', INSTALL, '.min();', '.max();', E, 'source_install', 'both_the_staging'),
    ('size-limit-loosened', INSTALL, 'candidate.size_bytes > MAX_FILE_BYTES', 'candidate.size_bytes > MAX_FILE_BYTES + 1', E, 'source_install', 'absurd_size'),
    ('empty-file-accepted', INSTALL, 'candidate.size_bytes == 0 ||', '', E, 'source_install', 'absurd_size'),
    ('format-bytes-unread', INSTALL, 'Format::check(name, &head).map_err(|problem| {', 'Ok::<Format, FormatProblem>(Format::from_name(name)).map_err(|problem| {', E, 'source_install', 'pickle_bytes_under_a_safe_name'),
    ('disguised-file-kept-in-staging', INSTALL, 'let _ = std::fs::remove_file(&staged_file);\n                return Err(refusal);', 'return Err(refusal);', E, 'source_install', 'pickle_bytes_under_a_safe_name'),
    ('observed-called-published', INSTALL, 'Ok((observed, Checksum::Observed))', 'Ok((observed, Checksum::Published))', E, 'source_install', 'no_published_checksum'),
    ('staging-shared-between-sources', INSTALL, 'self.staging.join(name)', 'self.staging.clone()', E, 'source_install', 'do_not_share_a_partial'),
    ('staged-leftover-trusted', INSTALL, '        let _ = std::fs::remove_file(&staged_file);\n\n        let (digest', '        let (digest', E, 'source_install', 'leftover_file_in_staging'),
    ('local-copy-size-unchecked', INSTALL, 'if size != candidate.size_bytes {', 'if false {', E, 'source_install', 'copied_and_the_original'),
    ('folder-unchecked', INSTALL, 'let target_dir = self.comfy.models_dir_for(folder).ok_or(Refusal::Folder)?;', 'let target_dir = self.comfy.base.join("models").join(folder);', E, 'source_install', 'cannot_leave_the_models'),
    ('settings-hosts-not-passed-on', C + 'ai-comfy-env/src/source/settings.rs', 'allowed_hosts: Some(self.download_hosts(kind)),', 'allowed_hosts: None,', E, 'source_install', 'reach_the_downloader'),
    ('registry-overwrites-damage', REGISTRY, 'serde_json::from_str(&text).map_err(|_| unreadable())', 'Ok(serde_json::from_str(&text).unwrap_or_default())', E, 'source_install', 'read_with_care'),
]
# `..` is also stopped by the trailing-dot rule, so this mutant lifts both checks for `..` alone.
DOT_DOT_OLD = """if part.is_empty() || part == "." || part == ".." || part.len() > 255 {
        return false;
    }
    if part.ends_with('.') || part.ends_with(' ') {"""
DOT_DOT_NEW = """if part.is_empty() || part.len() > 255 {
        return false;
    }
    if (part.ends_with('.') && part != "..") || part.ends_with(' ') {"""
U4 = [
    ('pack-plain-http-accepted', PACK, '"http" => return Err(SpecError::Insecure),', '"http" => {}', E, 'nodepack', 'not_a_plain_https'),
    ('pack-user-info-accepted', PACK, 'if !url.username().is_empty() || url.password().is_some() {', 'if false {', E, 'nodepack', 'not_a_plain_https'),
    ('pack-any-host-accepted', PACK, 'if url.host_str() != Some("github.com") || url.port().is_some() {', 'if url.port().is_some() {', E, 'nodepack', 'not_a_plain_https'),
    ('pack-port-accepted', PACK, 'if url.host_str() != Some("github.com") || url.port().is_some() {', 'if url.host_str() != Some("github.com") {', E, 'nodepack', 'not_a_plain_https'),
    ('pack-ref-unchecked', PACK, 'if reference.is_some_and(|r| !revision_ok(r)) {', 'if false {', E, 'nodepack', 'not_a_plain_https'),
    ('pack-names-unchecked', PACK, 'if !simple_name(owner) || !simple_name(repo) {', 'if false {', E, 'nodepack', 'not_a_plain_https'),
    ('pack-private-repo-allowed', PACK_RESOLVE, 'if info.get("private").and_then(Value::as_bool) != Some(false) {', 'if false {', E, 'nodepack', 'private_repository'),
    ('pack-commit-unchecked', PACK_RESOLVE, '.filter(|s| is_hex(s, 40) && s.bytes().all(|b| !b.is_ascii_uppercase()))', '.filter(|_| true)', E, 'nodepack', 'private_repository'),
    ('pack-subject-unclean', PACK_RESOLVE, 'let clean: String = line.chars().filter(|c| !c.is_control()).take(120).collect();', 'let clean: String = line.chars().take(120).collect();', E, 'nodepack', 'one_short_clean_line'),
    ('pack-subject-unbounded', PACK_RESOLVE, '.take(120)', '.take(1200)', E, 'nodepack', 'one_short_clean_line'),
    ('pack-archive-unpinned', PACK_RESOLVE, '&format!("{sha}.tar.gz")', '&format!("{}.tar.gz", spec.reference.as_deref().unwrap_or("main"))', E, 'nodepack', 'pinned_to_a_commit'),
    ('pack-consent-for-any-commit', PACK_INSTALL, 'if !consent.is_some_and(|c| c.commit == candidate.commit) {', 'if consent.is_none() {', E, 'nodepack', 'consent_to_run_code'),
    ('pack-consent-not-needed', PACK_INSTALL, 'if !consent.is_some_and(|c| c.commit == candidate.commit) {', 'if false {', E, 'nodepack', 'consent_to_run_code'),
    ('pack-folder-checked-after-download', PACK_INSTALL, 'if std::fs::symlink_metadata(&target).is_ok() {\n            return Err(PackError::Refused(PackRefusal::FolderTaken));\n        }\n        if cancel', 'if false {\n            return Err(PackError::Refused(PackRefusal::FolderTaken));\n        }\n        if cancel', E, 'nodepack', 'existing_folder_is_never'),
    ('pack-staging-kept-on-refusal', PACK_INSTALL, 'let _ = std::fs::remove_dir_all(&stage);\n                return Err(e);', 'return Err(e);', E, 'nodepack_archive', 'hostile_archives'),
    ('pack-requirements-not-flagged', PACK_INSTALL, '.any(|n| n == "requirements.txt")', '.any(|n| n == "requirements.tx")', E, 'nodepack', 'unpacked_exactly_as_shipped'),
    ('pack-install-script-not-flagged', PACK_INSTALL, '.any(|n| n == "install.py" || n == "setup.py")', '.any(|n| n == "setup.py")', E, 'nodepack', 'unpacked_exactly_as_shipped'),
    ('pack-removes-what-it-did-not-install', PACK_INSTALL, 'if !self.registry.packs()?.iter().any(|p| p.folder == folder) {', 'if false {', E, 'nodepack', 'recorded_pack_can_be_removed'),
    ('pack-follows-links-on-removal', PACK_INSTALL, 'Ok(meta) if meta.file_type().is_symlink() => {', 'Ok(meta) if false => {', E, 'nodepack', 'folder_that_is_a_link'),
    ('archive-links-and-devices-accepted', PACK_ARCHIVE, "b'0' | 0 | b'5' => {}", "b'0' | 0 | b'5' | b'1' | b'2' | b'3' => {}", E, 'nodepack_archive', 'hostile_archives'),
    ('archive-dot-dot-accepted', PACK_ARCHIVE, DOT_DOT_OLD, DOT_DOT_NEW, E, 'nodepack_archive', 'hostile_archives'),
    ('archive-backslash-accepted', PACK_ARCHIVE, r'"\\:<>\"|?*".contains(c)', r'":<>\"|?*".contains(c)', E, 'nodepack_archive', 'hostile_archives'),
    ('archive-drive-letter-accepted', PACK_ARCHIVE, r'"\\:<>\"|?*".contains(c)', r'"\\<>\"|?*".contains(c)', E, 'nodepack_archive', 'hostile_archives'),
    ('archive-device-names-accepted', PACK_ARCHIVE, '!RESERVED.contains(&stem.as_str())', 'true', E, 'nodepack_archive', 'hostile_archives'),
    ('archive-trailing-dot-accepted', PACK_ARCHIVE, "if part.ends_with('.') || part.ends_with(' ') {", 'if false {', E, 'nodepack_archive', 'hostile_archives'),
    ('archive-case-duplicates-accepted', PACK_ARCHIVE, 'Entry::Occupied(_) => Err(same_in_any_case()),', 'Entry::Occupied(_) => Ok(()),', E, 'nodepack_archive', 'hostile_archives'),
    ('archive-case-folders-accepted', PACK_ARCHIVE, 'Entry::Occupied(_) => return Err(same_in_any_case()),', 'Entry::Occupied(_) => {}', E, 'nodepack_archive', 'hostile_archives'),
    ('archive-case-folders-compared-whole', PACK_ARCHIVE, 'let path = parts[..end].join("/");', 'let path = parts[..end].join("/").to_lowercase();', E, 'nodepack_archive', 'hostile_archives'),
    ('archive-second-top-folder-accepted', PACK_ARCHIVE, 'Some(_) => return Err(unsafe_archive("the archive has more than one top folder")),', 'Some(_) => {}', E, 'nodepack_archive', 'hostile_archives'),
    ('archive-checksum-unchecked', PACK_ARCHIVE, 'if sum == stored {', 'if true {', E, 'nodepack_archive', 'hostile_archives'),
    ('archive-entry-limit-unchecked', PACK_ARCHIVE, 'if entries > limits.max_entries {', 'if false {', E, 'nodepack_archive', 'size_limits'),
    ('archive-size-limits-unchecked', PACK_ARCHIVE, 'if size > limits.max_file || bytes.saturating_add(size) > limits.max_expanded {', 'if false {', E, 'nodepack_archive', 'size_limits'),
    ('archive-pax-size-accepted', PACK_ARCHIVE, '"size" | "linkpath" => {', '"linkpath" => {', E, 'nodepack_archive', 'hostile_archives'),
    ('archive-empty-accepted', PACK_ARCHIVE, 'if files == 0 {', 'if false {', E, 'nodepack_archive', 'hostile_archives'),
    ('download-body-unbounded', TRANSPORT, 'if bytes.len() as u64 > max_bytes {', 'if false {', E, 'download_sources', 'only_up_to_its_limit'),
]

CF = 'craft-ai-comfyui'
CF_OUT = C + 'ai-comfyui/src/output.rs'
CF_IN = C + 'ai-comfyui/src/input.rs'
CF_PROV = C + 'ai-comfyui/src/provider.rs'
CF_FLOW = C + 'ai-comfyui/src/workflow.rs'
UPLOAD_ORDER_OLD = """let source = self.read_role(request, InputRole::Source)?;
            let mask = self.read_role(request, InputRole::Mask)?;
            let source = self.upload(InputRole::Source, &source)?;
            let mask = self.upload(InputRole::Mask, &mask)?;"""
UPLOAD_ORDER_NEW = """let source = self.read_role(request, InputRole::Source)?;
            let source = self.upload(InputRole::Source, &source)?;
            let mask = self.read_role(request, InputRole::Mask)?;
            let mask = self.upload(InputRole::Mask, &mask)?;"""
MASK_JPEG_OLD = """|| (role == InputRole::Source
                                && h.media_type.as_str() == "image/jpeg") => {}"""
MASK_JPEG_NEW = """|| h.media_type.as_str() == "image/jpeg" => {}"""
# p4: media going to and coming from ComfyUI is judged by its bytes (crates/ai-comfyui).
P4 = [
    ('output-not-checked', CF_OUT, 'if sniff::media_type(head).map(|(media, _)| media) == Some(expected) {', 'if true {', CF, 'media_checks', 'an_output_that_is'),
    ('output-any-media-accepted', CF_OUT, 'if sniff::media_type(head).map(|(media, _)| media) == Some(expected) {', 'if sniff::media_type(head).is_some() {', CF, 'media_checks', 'an_output_that_is'),
    ('output-on-disk-not-checked', CF_OUT, 'check_bytes(&head, media)?;', 'let _ = (&head, media);', CF, 'media_checks', 'an_output_that_is'),
    ('output-inline-not-checked', CF_OUT, 'check_bytes(&bytes, media)?;', 'let _ = (&bytes, media);', CF, 'media_checks', 'an_output_that_is'),
    ('input-not-checked', CF_IN, 'if sniff::media_type(head).map(|(media, _)| media) != Some(declared) {', 'if false {', CF, 'media_checks', 'never_uploaded'),
    ('input-any-image-accepted', CF_IN, 'if sniff::media_type(head).map(|(media, _)| media) != Some(declared) {', 'if sniff::media_type(head).is_none() {', CF, 'media_checks', 'never_uploaded'),
    ('input-size-unchecked', CF_IN, 'if u64::try_from(data.len()).unwrap_or(u64::MAX) > max {', 'if false {', CF, 'media_checks', 'over_the_limit'),
    ('input-limit-ignored', CF_PROV, 'self.max_upload_bytes = bytes.clamp(1, crate::input::HARD_MAX_UPLOAD_BYTES);', 'let _ = bytes;', CF, 'media_checks', 'over_the_limit'),
    ('input-limit-not-passed-on', CF_PROV, 'crate::input::read_image(&input.payload, self.max_upload_bytes)', 'crate::input::read_image(&input.payload, crate::input::DEFAULT_MAX_UPLOAD_BYTES)', CF, 'media_checks', 'over_the_limit'),
    ('input-web-address-read', CF_IN, 'if url.scheme() != "file" {', 'if false {', CF, 'media_checks', 'only_plain_local'),
    ('input-other-machine-read', CF_IN, 'if !matches!(url.host_str(), None | Some("" | "localhost")) {', 'if false {', CF, 'media_checks', 'only_plain_local'),
    ('input-folder-read', CF_IN, 'if !meta.is_file() {', 'if false {', CF, 'media_checks', 'only_plain_local'),
    ('input-file-size-looked-at-late', CF_IN, 'if meta.len() > max {', 'if false {', CF, 'lib', 'a_file_over_the_limit'),
    ('input-file-read-unbounded', CF_IN, 'reader.take(max.saturating_add(1))', 'reader.take(u64::MAX)', CF, 'lib', 'a_reader_is_read_no_further'),
    ('input-sent-before-all-checked', CF_PROV, UPLOAD_ORDER_OLD, UPLOAD_ORDER_NEW, CF, 'media_checks', 'never_uploaded'),
    ('input-jpeg-mask-accepted', CF_FLOW, MASK_JPEG_OLD, MASK_JPEG_NEW, CF, 'media_checks', 'mask_must_be_a_png'),
]

MEDIA = C + 'ai-media/src/store.rs'
CM = 'craft-ai-media'
RETRY_GATE = 'if !error.is_retryable() || attempt >= self.config.max_attempts {'
# p10: a busy media host is tried again, a refused link is not, and the pauses are bounded
# (crates/ai-media). Each mutant runs only the test that should notice it, so none can hang on a
# real hour-long pause.
P10 = [
    ('retry-never', MEDIA, RETRY_GATE, 'if true {', CM, 'retry', 'tried_again_until'),
    ('retry-everything', MEDIA, RETRY_GATE, 'if attempt >= self.config.max_attempts {', CM, 'retry', 'will_not_get_better'),
    ('one-try-too-many', MEDIA, RETRY_GATE, 'if !error.is_retryable() || attempt > self.config.max_attempts {', CM, 'retry', 'number_of_tries'),
    ('tries-clamped-too-high', MEDIA, 'max_attempts: config.max_attempts.clamp(1, 5),', 'max_attempts: config.max_attempts.clamp(1, 50),', CM, 'retry', 'number_of_tries'),
    ('tries-clamped-too-low', MEDIA, 'max_attempts: config.max_attempts.clamp(1, 5),', 'max_attempts: config.max_attempts.clamp(2, 5),', CM, 'retry', 'number_of_tries'),
    ('pause-constant', MEDIA, '.saturating_mul(1 << (attempt - 1));', '.saturating_mul(1);', CM, 'retry', 'pause_doubles'),
    ('pause-none', MEDIA, 'let backoff = self.config.retry_delay.saturating_mul(1 << (attempt - 1));', 'let backoff = Duration::ZERO;', CM, 'retry', 'pause_doubles'),
    ('retry-after-ignored', MEDIA, '.map_or(backoff, |secs| backoff.max(Duration::from_secs(secs)));', '.map_or(backoff, |_| backoff);', CM, 'retry', 'asks_for_a_long_wait'),
    ('budget-ignores-the-pause', MEDIA, 'if started.elapsed().saturating_add(wait) > self.config.retry_budget {', 'if started.elapsed() > self.config.retry_budget {', CM, 'retry', 'overrun_the_budget'),
    ('budget-loosened', MEDIA, 'if started.elapsed().saturating_add(wait) > self.config.retry_budget {', 'if started.elapsed().saturating_add(wait) > self.config.retry_budget.saturating_mul(1000) {', CM, 'retry', 'overrun_the_budget'),
    ('gateway-timeout-not-retried', MEDIA, 'matches!(status, 408 | 500 | 502 | 503 | 504)', 'matches!(status, 408 | 500 | 502 | 503)', CM, 'retry', 'tried_again_until'),
    ('request-timeout-not-retried', MEDIA, 'matches!(status, 408 | 500 | 502 | 503 | 504)', 'matches!(status, 500 | 502 | 503 | 504)', CM, 'retry', 'tried_again_until'),
    ('slow-down-not-retried', MEDIA, 'if status == 429 {', 'if false {', CM, 'retry', 'says_slow_down'),
    ('retry-after-not-kept', MEDIA, 'let retry_after = error.retry_after_secs;', 'let retry_after: Option<u64> = None;', CM, 'retry', 'last_answer'),
    ('tries-not-reported', MEDIA, 'if attempts <= 1 {', 'if true {', CM, 'retry', 'number_of_tries'),
    ('cut-transfer-not-retried', MEDIA, '.map_err(|_| AiError::new(ErrorKind::Network, "media transfer failed"))?;', '.map_err(|_| AiError::new(ErrorKind::Provider, "media transfer failed"))?;', CM, 'retry', 'cut_transfer'),
]

STORE = C + 'ai-core/src/store.rs'
ROLE = C + 'ai-core/src/role.rs'
TOOLS = C + 'ai-client/src/tools.rs'
CLIENT = C + 'ai-client/src/client.rs'
STATUS_RESOLVE = 'let resolved = match store.resolve(&self.app, role) {'
# v1: one binding, every app (ai-client/tests/one_binding.rs)
V1 = [
    ('launcher-default-ignored', STORE, '&[overrides, self.defaults.as_slice()],', '&[overrides],', CL, 'one_binding', ''),
    ('app-choice-ignored', STORE, '&[overrides, self.defaults.as_slice()],', '&[self.defaults.as_slice()],', CL, 'one_binding', ''),
    ('default-beats-app-choice', STORE, '&[overrides, self.defaults.as_slice()],', '&[self.defaults.as_slice(), overrides],', CL, 'one_binding', ''),
    ('status-reads-one-fixed-app', CLIENT, STATUS_RESOLVE, 'let resolved = match store.resolve(&AppId::new("launcher").unwrap_or_else(|_| self.app.clone()), role) {', CL, 'one_binding', ''),
    ('unbound-reads-as-broken', ROLE, "Self::Unbound(r) => write!(f, \"no model is chosen for '{r}'\"),", "Self::Unbound(r) => write!(f, \"'{r}' is broken\"),", CL, 'one_binding', ''),
    ('unbound-hides-the-tool', ROLE, "Self::Unbound(r) => write!(f, \"no model is chosen for '{r}'\"),", "Self::Unbound(_) => write!(f, \"no model is chosen\"),", CL, 'one_binding', ''),
    ('choose-writes-nothing', TOOLS, '            set_default(store, binding);\n            Ok::<(), AiError>(())', '            let _ = binding;\n            Ok::<(), AiError>(())', CL, 'one_binding', ''),
]

PICK_G = C + 'ai-client/src/picker_groups.rs'
PICK_C = C + 'ai-client/src/picker.rs'
PICK_E = C + 'ai-egui/src/picker.rs'
EG = 'craft-ai-egui'
# v2: the tool-first model picker (ai-client/tests/picker.rs, ai-egui/tests/picker_view.rs)
V2 = [
    ('fits-ignores-inputs', ROLE, '.all(|i| spec.accepts.contains_key(i))', '.all(|_| true)', CL, 'picker', ''),
    ('fits-ignores-capability', ROLE, 'let spec = model.spec_for(&role.capability)?;', 'let spec = model.capabilities.first()?;', CL, 'picker', ''),
    ('switched-off-counts-as-ready', PICK_G, 'let (state, note) = if !connection.enabled {', 'let (state, note) = if false {', CL, 'picker', ''),
    ('missing-key-counts-as-ready', PICK_G, '} else if secret_for(&*client.secrets, connection).is_err() {', '} else if false {', CL, 'picker', ''),
    ('ready-group-offers-a-fix', PICK_G, 'setup: (state != GroupState::Ready && state != GroupState::NeedsAdapter).then(|| {', 'setup: (state != GroupState::NeedsAdapter).then(|| {', CL, 'picker', ''),
    ('service-listed-twice', PICK_G, '.any(|c| c.provider == preset.provider)', '.any(|_| false)', CL, 'picker', ''),
    ('local-listed-twice', PICK_G, 'if have_local {', 'if false {', CL, 'picker', ''),
    ('unusable-option-offered', PICK_G, '.filter(|o| o.usable && o.unlocks', '.filter(|o| o.unlocks', CL, 'picker', ''),
    ('other-tools-options-offered', PICK_G, 'o.unlocks.iter().any(|u| u == role.id.as_str())', 'true', CL, 'picker', ''),
    ('own-choice-ignored', PICK_G, '.get(app)', '.get(&AppId::new("zzz-other").unwrap_or_else(|_| app.clone()))', CL, 'picker', ''),
    ('picker-launcher-default-ignored', PICK_G, '.or_else(|| store.defaults.iter().find(|b| &b.role == role))', '.or_else(|| None)', CL, 'picker', ''),
    ('selected-never-marked', PICK_G, 'selected\n                    .is_some_and(|b| b.connection == connection.id && b.model == m.id),', 'false,', CL, 'picker', ''),
    ('groups-unsorted', PICK_C, 'groups.sort_by_key(|g| g.state);', 'groups.sort_by_key(|_| 0);', CL, 'picker', ''),
    ('problem-never-reported', PICK_C, 'RoleStatus::Unavailable { reason } => Some(reason),', 'RoleStatus::Unavailable { reason } => { let _ = reason; None }', CL, 'picker', ''),
    ('input-order-changed', PICK_G, 'InputRole::Source => 0,', 'InputRole::Source => 9,', CL, 'picker', ''),
    ('local-reads-as-hosted', PICK_G, 'Locality::Local => "Runs on this computer",', 'Locality::Local => "Sent to the service",', CL, 'picker', ''),
    ('metered-reads-as-free', PICK_G, 'Billing::Metered => Some("billed per use"),', 'Billing::Metered => Some("free"),', CL, 'picker', ''),
    ('greyed-models-pickable', PICK_E, 'ui.add_enabled_ui(ready, |ui| {', 'ui.add_enabled_ui(true, |ui| {', EG, 'picker_view', ''),
    ('current-model-re-chosen', PICK_E, '(clicked && !model.selected).then(', 'clicked.then(', EG, 'picker_view', ''),
    ('add-key-reads-as-set-up', PICK_E, '(Some(SetupLink::Fix { .. }), GroupState::NeedsKey) => Some(ADD_KEY),', '(Some(SetupLink::Fix { .. }), GroupState::NeedsKey) => Some(SET_UP),', EG, 'picker_view', ''),
    ('new-reads-as-add-key', PICK_E, '(Some(SetupLink::New { .. }), _) => Some(SET_UP),', '(Some(SetupLink::New { .. }), _) => Some(ADD_KEY),', EG, 'picker_view', ''),
    ('set-up-click-dropped', PICK_E, 'action = Some(PickerAction::SetUp(link.clone()));', 'action = None;', EG, 'picker_view', ''),
    ('empty-says-nothing', PICK_E, 'if view.groups.is_empty() {', 'if false {', EG, 'picker_view', ''),
    ('in-use-not-marked', PICK_E, '    if model.selected {\n        name.push_str', '    if false {\n        name.push_str', EG, 'picker_view', ''),
    ('problem-banner-silent', PICK_E, 'is not working: {problem}', 'has been changed', EG, 'picker_view', ''),
    ('locality-badge-hidden', PICK_E, 'if let Some(locality) = group.locality {', 'if let Some(locality) = group.locality.filter(|_| false) {', EG, 'picker_view', ''),
    ('note-hidden', PICK_E, 'if !group.note.is_empty() {', 'if false {', EG, 'picker_view', ''),
]

OPT = C + 'ai-features/src/options.rs'
POPUP_E = C + 'ai-egui/src/popup.rs'
TOOLS_RS = C + 'ai-client/src/tools.rs'
FE = 'craft-ai-features'
# v3: the in-app missing-model popup (ai-client/tests/popup.rs, ai-egui/tests/popup_view.rs)
V3 = [
    ('size-not-carried', OPT, 'size_bytes: Some(recipe.total_bytes()),', 'size_bytes: None,', FE, 'options', ''),
    ('service-claims-a-download', OPT, 'usable: true,\n                    size_bytes: None,', 'usable: true,\n                    size_bytes: Some(1),', FE, 'options', ''),
    ('size-dropped-in-the-picker', PICK_G, 'size_bytes: option.size_bytes,', 'size_bytes: None,', CL, 'picker', ''),
    ('fix-group-claims-a-download', PICK_G, '            size_bytes: None,', '            size_bytes: Some(1),', CL, 'picker', ''),
    ('choose-skips-the-check', TOOLS_RS, '            resolve(role, &[std::slice::from_ref(&binding)], &store.connections)\n                .map_err(|e| AiError::invalid(e.to_string()))?;\n            set_default(store, binding);', '            resolve(role, &[std::slice::from_ref(&binding)], &store.connections)\n                .ok();\n            set_default(store, binding);', CL, 'popup', ''),
    ('popup-shown-when-ready', TOOLS_RS, 'let RoleStatus::Unavailable { reason } = self.client.status(role) else {\n            return Ok(None);\n        };', 'let reason = match self.client.status(role) {\n            RoleStatus::Unavailable { reason } => reason,\n            RoleStatus::Ready(_) => String::new(),\n        };', CL, 'popup', ''),
    ('popup-without-ways-to-set-up', TOOLS_RS, 'self.client.picker(role, &setup_options(features))?', 'self.client.picker(role, &[])?', CL, 'popup', ''),
    ('popup-reason-blank', TOOLS_RS, 'title: role.label.clone(),\n            reason,', 'title: role.label.clone(),\n            reason: String::new(),', CL, 'popup', ''),
    ('popup-heading-loses-the-need', POPUP_E, 'ui.heading(format!("{} {HEADING_SUFFIX}", popup.title));', 'ui.heading(popup.title.clone());', EG, 'popup_view', ''),
    ('popup-reason-hidden', POPUP_E, 'ui.colored_label(ui.visuals().warn_fg_color, &popup.reason);', 'let _ = &popup.reason;', EG, 'popup_view', ''),
    ('popup-choice-dropped', POPUP_E, 'action = Some(PopupAction::Choose { connection, model });', 'let _ = (connection, model);', EG, 'popup_view', ''),
    ('popup-set-up-dropped', POPUP_E, 'Some(PickerAction::SetUp(link)) => action = Some(PopupAction::SetUp(link)),', 'Some(PickerAction::SetUp(_)) => {}', EG, 'popup_view', ''),
    ('popup-not-now-dropped', POPUP_E, 'action = Some(PopupAction::NotNow);', 'action = None;', EG, 'popup_view', ''),
    ('size-hidden', PICK_E, 'if let Some(size) = group.size_bytes {', 'if let Some(size) = group.size_bytes.filter(|_| false) {', EG, 'picker_view', ''),
    ('size-in-the-wrong-unit', PICK_E, 'text::bytes(size)', 'text::bytes(size / 1000)', EG, 'popup_view', ''),
]

SD_LIB = C + 'ai-seedance/src/lib.rs'
DRAFT_RS = C + 'ai-core/src/draft.rs'
SDC = 'craft-ai-seedance'
BYTEPLUS = 'https://ark.ap-southeast.bytepluses.com/api/v3'
# sd: Seedance connects through BytePlus ModelArk (ai-seedance/tests/provider.rs, draft tests)
SD = [
    ('any-address-accepted', SD_LIB, '(!endpoint.is_loopback() && base != "' + BYTEPLUS + '")', '(!endpoint.is_loopback() && false)', SDC, 'provider', ''),
    ('volcengine-address-accepted', SD_LIB, 'base != "' + BYTEPLUS + '"', 'base != "https://ark.cn-beijing.volces.com/api/v3"', SDC, 'provider', ''),
    ('query-string-allowed', SD_LIB, "base.contains(['?', '#'])", 'false', SDC, 'provider', ''),
    ('old-model-served', SD_LIB, 'const MODEL: &str = "dreamina-seedance-2-0-260128";', 'const MODEL: &str = "doubao-seedance-2-0-260128";', SDC, 'provider', ''),
    ('preset-points-at-volcengine', DRAFT_RS, 'endpoint: "' + BYTEPLUS + '",', 'endpoint: "https://ark.cn-beijing.volces.com/api/v3",', M, 'lib', 'seedance_connects'),
    ('preset-named-volcengine', DRAFT_RS, 'name: "Seedance (BytePlus ModelArk)",', 'name: "Seedance (Volcengine Ark)",', M, 'lib', 'seedance_connects'),
]

LOCAL_RS = C + 'ai-client/src/local.rs'
# lc: a tool's local model as a connection an app can read (ai-client/tests/local.rs)
LC = [
    ('non-local-address-accepted', LOCAL_RS, "if !endpoint.is_loopback() || trimmed.contains(['?', '#']) {", "if trimmed.contains(['?', '#']) {", CL, 'local', ''),
    ('query-in-address-accepted', LOCAL_RS, "if !endpoint.is_loopback() || trimmed.contains(['?', '#']) {", 'if !endpoint.is_loopback() {', CL, 'local', ''),
    ('trailing-slash-kept', LOCAL_RS, "input.trim_end_matches('/')", "input.trim_end_matches('~')", CL, 'local', ''),
    ('unrunnable-recipe-accepted', LOCAL_RS, 'feature.local.as_ref().filter(|r| r.usable)', 'feature.local.as_ref().filter(|_| true)', CL, 'local', ''),
    ('replaces-a-choice-made', LOCAL_RS, 'if !store.defaults.iter().any(|b| b.role == binding.role) {', 'if true {', CL, 'local', ''),
    ('never-binds', LOCAL_RS, 'if !store.defaults.iter().any(|b| b.role == binding.role) {', 'if false {', CL, 'local', ''),
    ('moved-address-kept-old', LOCAL_RS, 'Some(existing) => *existing = connection,', 'Some(_) => {}', CL, 'local', ''),
    ('local-model-billed', LOCAL_RS, 'billing: Billing::Free,', 'billing: Billing::Metered,', CL, 'local', ''),
    ('local-model-needs-a-key', LOCAL_RS, 'auth: Auth::None,', 'auth: Auth::ApiKey { secret: craft_ai_core::SecretRef::new("k").unwrap_or_else(|_| unreachable!()) },', CL, 'local', ''),
    ('tool-inputs-dropped', LOCAL_RS, 'for input in &feature.inputs {', 'for input in feature.inputs.iter().take(1) {', CL, 'local', ''),
    ('address-not-given-to-apps', LOCAL_RS, 'route.endpoint.map(Some)', 'None', CL, 'local', 'it_binds_the_tool'),
]

# lr: the chosen connection, model and inference backend are one route (local_routes tests).
LR = [
    ('wrong-backend-selected', LOCAL_RS, 'provider: resolved.connection.provider.clone(),', 'provider: "comfyui".to_owned(),', CL, 'local_routes', 'every_app_reads'),
    ('wrong-model-selected', LOCAL_RS, 'model: resolved.model.id.clone(),', 'model: ModelId::new("other").unwrap_or_else(|_| resolved.model.id.clone()),', CL, 'local_routes', 'every_app_reads'),
    ('broken-choice-looks-unbound', LOCAL_RS, 'Err(error) => return Err(AiError::unsupported(error.to_string())),', 'Err(_error) => return Ok(None),', CL, 'local_routes', 'a_broken_choice'),
    ('remote-route-leaked', LOCAL_RS, 'if resolved.connection.locality != Locality::Local {', 'if false {', CL, 'local_routes', 'a_remote_choice'),
    ('native-route-treated-as-comfyui', LOCAL_RS, 'if route.provider != "comfyui" {', 'if false {', CL, 'local_routes', 'an_endpoint_cannot'),
    ('unsupported-native-looks-ready', CLIENT, 'if !self.factory.knows(&resolved.connection.provider) {', 'if false {', CL, 'local_routes', 'a_selected_native'),
    ('picker-offers-uninstalled-adapter', ROOT / 'crates/ai-client/src/picker_groups.rs', '} else if !client.has_adapter(&connection.provider) {', '} else if false {', CL, 'local_routes', 'a_selected_native'),
    ('picker-false-setup-for-adapter', ROOT / 'crates/ai-client/src/picker_groups.rs', 'state != GroupState::Ready && state != GroupState::NeedsAdapter', 'state != GroupState::Ready', CL, 'local_routes', 'a_selected_native'),
]

# nr: the built-in runtime route reads one weights file (ai-client/tests/local_native.rs)
NR = [
    ('relative-path-accepted', LOCAL_RS, '!path.is_absolute() || !safetensors || !path.is_file()', '!safetensors || !path.is_file()', CL, 'local_native', 'relative_path'),
    ('any-extension-accepted', LOCAL_RS, '!path.is_absolute() || !safetensors || !path.is_file()', '!path.is_absolute() || !path.is_file()', CL, 'local_native', 'cannot_be_the_weights'),
    ('missing-file-accepted', LOCAL_RS, '!path.is_absolute() || !safetensors || !path.is_file()', '!path.is_absolute() || !safetensors', CL, 'local_native', 'cannot_be_the_weights'),
    ('unusable-recipe-accepted', LOCAL_RS, '.filter(|r| r.usable)\n            .and_then(|r| r.native.iter().find(|n| n.provider == provider))', '.and_then(|r| r.native.iter().find(|n| n.provider == provider))', CL, 'local_native', 'not_ready_to_use'),
    ('file-not-given-to-apps', LOCAL_RS, 'file: resolved.model.local_file.clone(),', 'file: None,', CL, 'local_native', 'registered_bound'),
    ('file-not-stored', LOCAL_RS, 'model.local_file = Some(file);', 'model.local_file = None;', CL, 'local_native', 'registered_bound'),
    ('other-backend-file-served', LOCAL_RS, 'if route.provider != provider {', 'if false {', CL, 'local_native', 'comfyui_route_has_no_file'),
    ('vanished-file-served', LOCAL_RS, 'if !path.is_file() {', 'if false {', CL, 'local_native', 'vanished'),
    ('unbound-is-an-error', LOCAL_RS, 'let Some(route) = self.local_route_for(role)? else {\n            return Ok(None);\n        };\n        if route.provider != provider', 'let Some(route) = self.local_route_for(role)? else {\n            return Err(AiError::unsupported("unbound"));\n        };\n        if route.provider != provider', CL, 'local_native', 'nothing_bound'),
    ('native-replaces-a-choice', LOCAL_RS, 'if !store.defaults.iter().any(|b| b.role == binding.role) {', 'if true {', CL, 'local_native', 'left_alone'),
    ('native-connection-id-shared-with-comfyui', LOCAL_RS, 'format!("local-{}-{}", feature.id, native.provider)', 'format!("local-{}", feature.id)', CL, 'local_native', 'left_alone'),
]

CT_CLIENT = C + 'ai-client/src/client.rs'
CT_TOOLS = C + 'ai-client/src/tools.rs'
CT_PICKER = C + 'ai-client/src/picker.rs'
CT_STORE = C + 'ai-core/src/store.rs'
T = 'custom_tool_conformance'
# Declared custom tool, two app clients (tests/custom_tool_conformance.rs).
CT = [
    ('adapter-absence-reads-ready', CT_CLIENT, 'if !self.factory.knows(&resolved.connection.provider) {', 'if false {', CL, T, 'without_the_adapter'),
    ('picker-hides-missing-adapter', CT_PICKER, 'RoleStatus::Unavailable { reason } => Some(reason),', 'RoleStatus::Unavailable { reason } => {\n                    let _ = reason;\n                    None\n                }', CL, T, 'without_the_adapter'),
    ('consent-not-enforced', CT_CLIENT, 'if plan.needs_consent(ledger) {', 'if false {', CL, T, 'remote_service'),
    ('request-not-forwarded', CT_CLIENT, 'provider.submit(&plan.model, &plan.request) {', 'provider.submit(&plan.model, &craft_ai_core::Request::new(plan.request.capability.clone())) {', CL, T, 'two_apps_run'),
    ('cancel-not-forwarded', CT_CLIENT, 'pub fn cancel(&self) -> Result<(), AiError> {\n        self.provider.cancel(&self.id)', 'pub fn cancel(&self) -> Result<(), AiError> {\n        let _ = &self.provider;\n        Ok(())', CL, T, 'cancelling_one'),
    ('old-store-needs-the-new-field', CT_STORE, '#[serde(default, skip_serializing_if = "Vec::is_empty")]\n    pub custom_tools', '#[serde(skip_serializing_if = "Vec::is_empty")]\n    pub custom_tools', CL, T, 'before_custom_tools'),
    ('removal-keeps-the-launcher-choice', CT_TOOLS, 'store.defaults.retain(|binding| &binding.role != id);', 'let _ = id;', CL, T, 'before_custom_tools'),
]

from mutation_u2 import U2, U2_EQUIVALENT  # noqa: E402

PS_RS = C + 'ai-comfy-env/src/source/pack/status.rs'
PS = [
    ('record-not-consulted', PS_RS, '.find(|r| r.folder.eq_ignore_ascii_case(&folder.name))', '.find(|_| false)', E, 'nodepack_status', 'reports_the_commit'),
    ('first-record-taken-for-any-folder', PS_RS, '.find(|r| r.folder.eq_ignore_ascii_case(&folder.name))', '.next()', E, 'nodepack_status', 'changes_nothing_on_disk'),
    ('wrong-commit-reported', PS_RS, 'commit: entry.commit.clone(),', 'commit: entry.reference.clone(),', E, 'nodepack_status', 'reports_the_commit'),
    ('link-vouched-for-by-record', PS_RS, 'let real = kind.is_dir();', 'let real = true;', E, 'nodepack_status', 'no_longer_ours'),
    ('dangling-link-counts', PS_RS, 'kind.is_symlink() && entry.path().is_dir()', 'kind.is_symlink()', E, 'nodepack_status', 'link_to_nowhere'),
    ('file-counts-as-a-pack', PS_RS, '(real || linked_folder).then_some', 'true.then_some', E, 'nodepack_status', 'file_with_the_packs_name'),
    ('hidden-folder-counts', PS_RS, "name.starts_with('.') || name == \"__pycache__\"", 'false', E, 'nodepack_status', 'hidden_and_cache'),
    ('names-compared-case-sensitively', PS_RS, '.find(|f| f.name.eq_ignore_ascii_case(known))', '.find(|f| f.name == *known)', E, 'nodepack_status', 'person_put_there'),
    ('damaged-record-read-as-empty', PS_RS, 'registry.packs()?', 'registry.packs().unwrap_or_default()', E, 'nodepack_status', 'damaged_record'),
    ('only-the-first-known-name-tried', PS_RS, '.find_map(|known|', '.take(1).find_map(|known|', E, 'nodepack_status', 'any_of_the_known'),
]
LG_RS = C + 'ai-comfy-env/src/source/registry/licences.rs'
LG_IN = C + 'ai-comfy-env/src/source/install.rs'
LGT = 'source_licences'
LG = [
    ('licence-name-ignored', LG_RS, '&& self.name == c.licence.name', '&& true', E, LGT, 'new_version_changed_terms'),
    ('licence-link-ignored', LG_RS, '&& self.url == c.licence.url', '&& true', E, LGT, 'new_version_changed_terms'),
    ('version-ignored', LG_RS, '&& self.pin == c.pin', '&& true', E, LGT, 'new_version_changed_terms'),
    ('source-kind-ignored', LG_RS, 'self.kind == c.kind\n', 'true\n', E, LGT, 'new_version_changed_terms'),
    ('local-files-share-an-acceptance', LG_RS, '|| self.filename.as_deref() == Some(&c.filename)', '|| true', E, LGT, 'one_local_file'),
    ('permissive-terms-recorded', LG_RS, 'if !candidate.licence.accept_required {', 'if false {', E, LGT, 'need_no_acceptance'),
    ('acceptance-recorded-twice', LG_RS, 'if document.licences.iter().any(|r| r.covers(candidate)) {', 'if false {', E, LGT, 'accepting_twice'),
    ('old-installs-forgotten', LG_RS, '.any(|e| e.licence_accepted.is_some() && entry_covers(e, candidate))', '.any(|_| false)', E, LGT, 'before_the_record_existed_still_counts'),
    ('forget-leaves-old-installs', LG_RS, 'if entry_covers(e, candidate) && e.licence_accepted.take().is_some() {', 'if false {', E, LGT, 'before_the_record_existed_still_counts'),
    ('forget-keeps-the-record', LG_RS, 'document.licences.retain(|r| !r.covers(candidate));', 'document.licences.retain(|_| true);', E, LGT, 'taken_back'),
    ('damaged-record-read-as-empty', LG_RS, 'let document = self.read()?;\n        Ok(document.licences.iter()', 'let document = self.read().unwrap_or_default();\n        Ok(document.licences.iter()', E, LGT, 'damaged_record'),
    ('damage-found-only-after-download', LG_IN, 'self.registry.list().map_err(|_| Refusal::Record)?;', '', E, LGT, 'damaged_record'),
    ('install-forgets-the-yes', LG_IN, 'if candidate.licence.accept_required && consents.licence {', 'if false {', E, LGT, 'outlives_the_file'),
]
GROUPS = {'lg': LG, 'ps': PS, 'ct': CT, 'u2': U2, 'nr': NR, 'u1': U1, 'u3': U3, 'u4': U4, 'p4': P4, 'p10': P10, 'v1': V1, 'v2': V2, 'v3': V3, 'sd': SD, 'lc': LC, 'lr': LR}
# Mutants whose effect is also refused by a second, independent check on this machine. They are
# recorded as equivalent here, with the reason, and are never counted as caught.
EQUIVALENT = {
    'non-local-address-accepted': 'SharedStore::update calls write, which validates every connection; a non-loopback Local endpoint is refused even without the earlier local_address check (local.rs test confirms nothing written)',
    'input-folder-read': 'a directory cannot be opened on Windows, and where it can be opened reading it fails with the same message; the check exists for pipes and devices, which cannot be made here (not proven on Linux or macOS)',
    'directory-taken-as-local-file': 'on Windows a directory reports length 0, which the size check also refuses; elsewhere the is_file check is what refuses it',
}
EQUIVALENT.update(U2_EQUIVALENT)
# Both reasons above are about Windows; on Linux and macOS they are observed, not assumed.
WINDOWS_ONLY_EQUIVALENT = {'directory-taken-as-local-file', 'input-folder-read'}
TIMEOUTS = {}


def main():
    group = sys.argv[1] if len(sys.argv) > 1 else 'u1'
    only = sys.argv[2] if len(sys.argv) > 2 else ''
    if group not in GROUPS:
        raise SystemExit('group must be one of ' + ', '.join(GROUPS))
    results = []
    if only == '--dry':
        # Check every mutation still has exactly one target, without running cargo.
        bad = []
        for name, rel, old, *_ in GROUPS[group]:
            text = (ROOT / rel).read_bytes().decode('utf-8').replace(CRLF, LF)
            if text.count(old) != 1:
                bad.append((name, text.count(old)))
        print('mutations:', len(GROUPS[group]), 'without exactly one target:', bad)
        raise SystemExit(1 if bad else 0)
    for name, rel, old, new, package, target, test in GROUPS[group]:
        if only not in name:
            continue
        path = ROOT / rel
        original = path.read_bytes()
        text = original.decode('utf-8').replace('\r\n', '\n')
        assert text.count(old) == 1, (name, 'mutation did not have exactly one target', text.count(old))
        newline = '\r\n' if b'\r\n' in original else '\n'
        try:
            path.write_bytes(text.replace(old, new).replace('\n', newline).encode('utf-8'))
            # target 'lib' runs the crate's own unit tests instead of an integration test file.
            where = ['--lib'] if target == 'lib' else ['--test', target]
            command = ['cargo', 'test', '-p', package, *where, test, '-j', '8']
            proc = subprocess.Popen(command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            timed_out = False
            try:
                log, _ = proc.communicate(timeout=TIMEOUTS.get(name, 400))
            except subprocess.TimeoutExpired:
                timed_out = True
                if os.name == 'nt':
                    subprocess.run(['taskkill', '/T', '/F', '/PID', str(proc.pid)], capture_output=True)
                else:
                    proc.kill()
                log, _ = proc.communicate()
                log += '\nTIMED OUT: the mutated code made the test hang'
            (OUT / ('mutation-' + name + '.txt')).write_text(scrub(log), encoding='utf-8')
            caught = timed_out or (proc.returncode != 0 and 'test result: FAILED' in log)
            # Equivalent on Windows only: elsewhere the case is expected to be caught, and a miss there is a finding.
            equivalent = (not caught) and name in EQUIVALENT and not (name in WINDOWS_ONLY_EQUIVALENT and os.name != 'nt')
            results.append({'mutation': name, 'caught_by_runtime_test': caught, 'timed_out': timed_out, 'equivalent': EQUIVALENT[name] if equivalent else None})
            print(name, 'CAUGHT' if caught else ('EQUIVALENT' if equivalent else 'NOT CAUGHT'), flush=True)
        finally:
            path.write_bytes(original)
            os.utime(path, None)
            # The drive-letter mutant really does write `C:evil.py` into the working folder of the
            # test (a drive-relative path), which is what the check exists to stop. Tidy it away.
            (ROOT / 'crates/ai-comfy-env/evil.py').unlink(missing_ok=True)
    saved = OUT / (group + '-mutations.json')
    if only and saved.exists():
        # A filtered rerun updates its own entries and keeps the rest.
        merged = {r['mutation']: r for r in json.loads(saved.read_text(encoding='utf-8'))}
        merged.update({r['mutation']: r for r in results})
        results = list(merged.values())
    saved.write_text(json.dumps(results, indent=2), encoding='utf-8')
    survivors = [r['mutation'] for r in results if not r['caught_by_runtime_test'] and not r.get('equivalent')]
    equivalent = [r['mutation'] for r in results if r.get('equivalent')]
    print(json.dumps({'mutations': len(results), 'caught': sum(r['caught_by_runtime_test'] for r in results), 'equivalent': equivalent, 'survivors': survivors}))
    if survivors:
        raise SystemExit(1)


main()

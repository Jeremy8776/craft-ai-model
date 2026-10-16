"""Deliberate-bug cases for u2, the declarative adapter (spec checker, runner, factory entry).

Loaded by mutation_check.py as group `u2`. Each case breaks one rule in the source and names the
one test that should notice; see that file for how a case is judged.
"""
C = 'crates/'
M = 'craft-ai-core'
D = 'craft-ai-declarative'
CL = 'craft-ai-client'
S = C + 'ai-core/src/spec.rs'
SC = C + 'ai-core/src/spec/check.rs'
ST = C + 'ai-core/src/spec/template.rs'
DL = C + 'ai-declarative/src/lib.rs'
DH = C + 'ai-declarative/src/http.rs'
DF = C + 'ai-declarative/src/flow.rs'
DC = C + 'ai-client/src/declarative.rs'

# (name, file, old, new, package, test target, test filter)
U2 = [
    ('spec-size-unchecked', SC, 'if text.len() > MAX_SPEC_BYTES {', 'if false {', M, 'spec_rules', 'file_size_limit'),
    ('spec-size-off-by-one', SC, 'if text.len() > MAX_SPEC_BYTES {', 'if text.len() >= MAX_SPEC_BYTES {', M, 'spec_rules', 'file_size_limit'),
    ('spec-unknown-field-ignored', S, '#[serde(deny_unknown_fields)]\npub struct Spec {', 'pub struct Spec {', M, 'spec_rules', 'refused_by_name'),
    ('spec-nested-unknown-field-ignored', S, '#[serde(deny_unknown_fields)]\npub struct Submit {', 'pub struct Submit {', M, 'spec_rules', 'refused_by_name'),
    ('spec-version-unchecked', SC, 'if spec.spec_version != SPEC_VERSION {', 'if false {', M, 'spec_rules', 'only_version_one'),
    ('spec-base-query-allowed', SC, "if base.contains(['?', '#']) {", 'if false {', M, 'spec_rules', 'base_address'),
    ('spec-base-prefix-unchecked', SC, 'check_path(prefix).map_err(|why| err("base", why))?;', 'let _ = prefix;', M, 'spec_rules', 'base_address'),
    ('spec-path-names-another-host', SC, 'if path.contains("://") || path.starts_with("//") {', 'if false {', M, 'spec_rules', 'path_cannot_leave'),
    ('spec-path-dot-dot-allowed', SC, 'if path.split(\'/\').any(|s| s == "..") {', 'if false {', M, 'spec_rules', 'path_cannot_leave'),
    ('spec-path-double-slash-allowed', SC, 'if path.contains("//") {', 'if false {', M, 'spec_rules', 'path_cannot_leave'),
    ('spec-path-escape-allowed', SC, "if path.contains(['%', '\\\\']) {", 'if false {', M, 'spec_rules', 'path_cannot_leave'),
    ('spec-path-query-allowed', SC, "if path.contains(['?', '#']) {", 'if false {', M, 'spec_rules', 'path_cannot_leave'),
    ('spec-path-length-loosened', SC, 'if path.len() > MAX_PATH_BYTES {', 'if path.len() > MAX_PATH_BYTES + 1 {', M, 'spec_rules', 'path_cannot_leave'),
    ('spec-reserved-header-allowed', SC, 'check_key_header(name).map_err(|why| err("auth.name", why))?;', 'let _ = name;', M, 'spec_rules', 'key_header'),
    ('spec-prompt-count-loosened', SC, 'if *count != 1 {', 'if *count > 2 {', M, 'spec_rules', 'model_is_declared'),
    ('spec-other-input-allowed', SC, 'if parsed != InputRole::Prompt {', 'if false {', M, 'spec_rules', 'model_is_declared'),
    ('spec-duplicate-model-allowed', SC, 'if !seen.insert(&m.id) {', 'if !seen.insert(&m.id) && false {', M, 'spec_rules', 'model_is_declared'),
    ('spec-model-limit-loosened', SC, 'if models.len() > MAX_MODELS {', 'if models.len() > MAX_MODELS + 1 {', M, 'spec_rules', 'model_is_declared'),
    ('spec-outputs-unchecked', SC, 'if !(1..=4).contains(&m.max_outputs) {', 'if false {', M, 'spec_rules', 'model_is_declared'),
    ('spec-states-limit-loosened', SC, 'if states.len() > MAX_STATES {', 'if states.len() > MAX_STATES + 1 {', M, 'spec_rules', 'status_table'),
    ('spec-pointer-start-unchecked', SC, "if !pointer.starts_with('/') {", 'if false {', M, 'spec_rules', 'pointer_is'),
    ('spec-pointer-depth-loosened', SC, "if pointer.matches('/').count() > MAX_POINTER_DEPTH {", "if pointer.matches('/').count() > MAX_POINTER_DEPTH + 1 {", M, 'spec_rules', 'pointer_is'),
    ('spec-base64-without-type-allowed', SC, '(ExtractAs::Url | ExtractAs::Base64, None) => Err(err(\n            at,\n            "a media_type is needed for a link or base64 result",\n        )),', '(ExtractAs::Url, None) => Err(err(\n            at,\n            "a media_type is needed for a link or base64 result",\n        )),\n        (ExtractAs::Base64, None) => Ok(()),', M, 'spec_rules', 'result_says'),
    ('spec-limits-unchecked', SC, 'if !(1..=10_000).contains(&spec.limits.max_polls) {', 'if false {', M, 'spec_rules', 'limits_are_bounded'),
    ('spec-probe-may-write', SC, 'if probe.method != Method::Get {', 'if false {', M, 'spec_rules', 'free_read'),
    ('spec-get-with-body-allowed', SC, 'if call.method != Method::Post {', 'if false {', M, 'spec_calls', 'body_belongs'),
    ('spec-body-limit-tightened', SC, 'if body.to_string().len() > MAX_TEMPLATE_BYTES {', 'if body.to_string().len() >= MAX_TEMPLATE_BYTES {', M, 'spec_calls', 'body_belongs'),
    ('spec-slot-not-allowed-here', SC, 'if let Some(slot) = used.iter().find(|s| !allow.contains(s)) {', 'if let Some(slot) = None::<&Slot> {', M, 'spec_calls', 'only_job_id'),
    ('spec-poll-never-names-the-job', SC, 'if need_job && !used.contains(&Slot::JobId) {', 'if false {', M, 'spec_calls', 'must_name_the_job'),
    ('spec-model-slot-in-path', SC, 'if slot != Slot::JobId {', 'if false {', M, 'spec_calls', 'only_job_id'),
    ('spec-prompt-never-sent', SC, 'if !sent.contains(&Slot::Prompt) {', 'if false {', M, 'spec_calls', 'prompt_must_be_sent'),
    ('spec-model-need-not-require-prompt', SC, '.position(|m| !m.requires.iter().any(|r| r == "prompt"))', '.position(|_| false)', M, 'spec_calls', 'prompt_must_be_sent'),
    ('spec-query-limit-loosened', SC, 'if call.query.len() > 8 {', 'if call.query.len() > 9 {', M, 'spec_calls', 'query_values'),
    ('template-lone-brace-allowed', ST, "} else if here.starts_with('}') {", '} else if false {', M, 'spec_template', 'braces_are_closed'),
    ('template-extra-name-known', ST, '"job_id" => Some(Self::JobId),', '"job_id" | "seed" => Some(Self::JobId),', M, 'spec_template', 'three_names'),
    ('template-key-substituted', ST, '.map(|(k, v)| Ok((k.clone(), render_value(v, values)?)))', '.map(|(k, v)| Ok((render_text(k, values)?, render_value(v, values)?)))', M, 'spec_template', 'key_is_never_substituted'),
    ('template-key-brace-allowed', ST, "if key.contains(['{', '}']) {", 'if false {', M, 'spec_template', 'keys_are_literal'),
    ('run-redirect-followed', DH, '.max_redirects(0)', '.max_redirects(5)', D, 'queue', 'redirect_on_a_read'),
    ('run-none-auth-sends-key', DH, 'SpecAuth::None => None,', 'SpecAuth::None => Some(("Authorization", format!("Bearer {}", key.expose()))),', D, 'direct', 'no_key_sends_none'),
    ('run-named-header-ignored', DH, 'SpecAuth::Header { name } => Some((name.as_str(), key.expose().to_owned())),', 'SpecAuth::Header { .. } => Some(("Authorization", format!("Bearer {}", key.expose()))),', D, 'queue', 'probe_the_submit'),
    ('run-reply-cap-unchecked', DH, 'if bytes.len() > MAX_REPLY {', 'if false {', D, 'direct', 'size_limit'),
    ('run-reply-cap-loosened', DH, 'if bytes.len() > MAX_REPLY {', 'if bytes.len() > MAX_REPLY + 1 {', D, 'direct', 'size_limit'),
    ('run-job-id-length-loosened', DF, 'id.len() <= 128', 'id.len() <= 129', D, 'queue', 'job_id_the_service'),
    ('run-job-id-space-allowed', DF, "matches!(b, b'.' | b'_' | b':' | b'-')", "matches!(b, b'.' | b'_' | b':' | b'-' | b' ')", D, 'queue', 'job_id_the_service'),
    ('run-link-may-be-loopback', DF, 'e.https && !e.is_loopback()', 'e.https', D, 'queue', 'result_link_must'),
    ('run-link-fragment-allowed', DF, "|| text.contains('#')", '|| false', D, 'queue', 'result_link_must'),
    ('run-file-kind-unchecked', DF, 'if sniff::media_type(head).map(|(kind, _)| kind) != Some(media) {', 'if false {', D, 'direct', 'not_the_declared_kind'),
    ('run-call-resent-while-running', DL, '(Some(_), _) if job.attempted => return Ok(stable(job)),', '(Some(_), _) if false => return Ok(stable(job)),', D, 'direct', 'second_poll_while'),
    ('run-full-list-drops-running-jobs', DL, '.filter(|(_, j)| j.snapshot.state.is_terminal())\n                .min_by_key', '.min_by_key', D, 'direct', 'full_list_forgets'),
    ('run-room-made-after-the-paid-call', DL, '// Make room before anything billable is sent.\n            let mut jobs = self.lock()?;\n            self.make_room(&mut jobs)?;', 'let _ = ();', D, 'direct', 'room_before_the_billable'),
    ('run-poll-budget-tightened', DL, 'if job.polls > limits.max_polls', 'if job.polls >= limits.max_polls', D, 'queue', 'poll_budget'),
    ('run-poll-budget-loosened', DL, 'if job.polls > limits.max_polls', 'if job.polls > limits.max_polls + 100', D, 'queue', 'poll_budget'),
    ('run-failed-cancel-still-cancels', DL, 'self.send(&flow::fill(call, &values)?)?;\n        }\n        let mut jobs = self.lock()?;', 'let _ = self.send(&flow::fill(call, &values)?);\n        }\n        let mut jobs = self.lock()?;', D, 'queue', 'cancel_call_the_service_refuses'),
    ('run-probe-not-sent', DL, 'self.send(&flow::fill(probe, &Values::default())?)?;', 'let _ = probe;', D, 'queue', 'failing_probe'),
    ('run-address-may-differ', DC, 'if !same_address {', 'if false {', CL, 'declarative', 'address_differs'),
]

# Mutants that cannot change behaviour because a second layer refuses the same thing.
U2_EQUIVALENT = {
    'run-none-auth-sends-key': 'a spec with auth none also drops the key when the provider is built, so no key ever reaches the HTTP layer; the test proves the end result (no key header sent)',
}

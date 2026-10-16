//! The pure parts of running a spec: filling in a call and reading a reply. No network here.

use craft_ai_core::endpoint::Endpoint;
use craft_ai_core::payload::base64_decode;
use craft_ai_core::spec::template::{Values, render_text, render_value};
use craft_ai_core::spec::{Call, Extract, ExtractAs, Method};
use craft_ai_core::{AiError, ErrorKind, Payload};
use craft_ai_media::sniff;
use serde_json::Value;

/// A call with its substitutions made, ready to send.
pub(crate) struct Filled {
    pub method: Method,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Option<Value>,
}

/// Fills in a call. The spec was checked when it was loaded, so a failure here is a bug.
pub(crate) fn fill(call: &Call, values: &Values<'_>) -> Result<Filled, AiError> {
    let internal = |why: String| {
        AiError::new(
            ErrorKind::Internal,
            format!("could not fill in a call: {why}"),
        )
    };
    Ok(Filled {
        method: call.method,
        path: render_text(&call.path, values).map_err(internal)?,
        query: call
            .query
            .iter()
            .map(|(k, v)| Ok((k.clone(), render_text(v, values)?)))
            .collect::<Result<_, String>>()
            .map_err(internal)?,
        body: call
            .body
            .as_ref()
            .map(|b| render_value(b, values))
            .transpose()
            .map_err(internal)?,
    })
}

fn provider(label: &str, what: &str) -> AiError {
    AiError::new(ErrorKind::Provider, format!("{label} {what}"))
}

/// The service's id for a job: a short plain string, or a whole number.
pub(crate) fn job_id(reply: &Value, at: &str, label: &str) -> Result<String, AiError> {
    let id = match reply.pointer(at) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) if n.is_u64() || n.is_i64() => n.to_string(),
        _ => return Err(provider(label, "did not give a job id")),
    };
    // `.` and `..` are made of allowed characters but name a folder, not a job.
    let plain = !id.is_empty()
        && !id.bytes().all(|b| b == b'.')
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    if plain {
        Ok(id)
    } else {
        Err(provider(label, "gave a job id that is not short and plain"))
    }
}

/// The status word in a reply.
pub(crate) fn status<'a>(reply: &'a Value, at: &str, label: &str) -> Result<&'a str, AiError> {
    match reply.pointer(at) {
        Some(Value::String(s)) => Ok(s),
        _ => Err(provider(label, "did not give a status")),
    }
}

/// Reads the result the spec points at, checking it is what the spec says it is.
pub(crate) fn payload(reply: &Value, ex: &Extract, label: &str) -> Result<Payload, AiError> {
    let Some(Value::String(text)) = reply.pointer(&ex.at) else {
        return Err(provider(
            label,
            "did not give a result where the spec says it would",
        ));
    };
    let media = ex.media_type.as_deref().unwrap_or("");
    match ex.kind {
        ExtractAs::Text => Ok(Payload::text(text.clone())),
        ExtractAs::Url => {
            let public_https = Endpoint::parse(text).is_ok_and(|e| e.https && !e.is_loopback());
            if !public_https || text.contains('#') {
                return Err(provider(
                    label,
                    "gave a result link that is not a public https link",
                ));
            }
            Payload::handle(text, media, None)
        }
        ExtractAs::Base64 => {
            let bytes = base64_decode(text)
                .ok_or_else(|| provider(label, "gave a result that is not valid base64"))?;
            let head = bytes.get(..sniff::HEAD_LEN).unwrap_or(&bytes);
            if sniff::media_type(head).map(|(kind, _)| kind) != Some(media) {
                return Err(provider(
                    label,
                    "gave a file that is not the kind the spec says",
                ));
            }
            Payload::inline(media, bytes)
        }
    }
}

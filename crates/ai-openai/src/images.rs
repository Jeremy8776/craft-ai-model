//! Image endpoints: `/images/generations` (JSON) and `/images/edits` (multipart).
//!
//! Pure functions, tested without a server. `image.edit` and `image.inpaint` share the edits
//! endpoint; the only difference is that inpainting requires a mask, which the model's spec
//! enforces before anything is built.
//!
//! Params that are passed through (anything else is ignored): `size`, `quality`, `background`,
//! `output_format`, `output_compression`, `style`, `moderation`.
//!
//! Inputs must be inline PNG, JPEG or WebP data. A handle (a path or URL) is refused, because this
//! adapter does not read files or fetch URLs on an app's behalf.

use craft_ai_core::job::Output;
use craft_ai_core::payload::{Limits, base64_decode};
use craft_ai_core::{AiError, ErrorKind, InputRole, Payload, Request, Usage};
use serde_json::{Map, Value, json};

use crate::catalog::needs_response_format;
use crate::chat::usage_from;

const PASS_THROUGH: &[&str] = &[
    "size",
    "quality",
    "background",
    "output_format",
    "output_compression",
    "style",
    "moderation",
];

/// A request ready to send.
#[derive(Debug)]
pub(crate) struct ImagePlan {
    pub path: &'static str,
    pub content_type: String,
    pub body: Vec<u8>,
    /// Media type to use for returned images when the bytes cannot be recognised.
    pub fallback_media: &'static str,
}

fn prompt_text(req: &Request) -> Result<String, AiError> {
    let mut found = None;
    for input in req.inputs.iter().filter(|i| i.role == InputRole::Prompt) {
        match &input.payload {
            Payload::Text { text } if !text.trim().is_empty() => found = Some(text.clone()),
            _ => return Err(AiError::invalid("the prompt must be non-empty text")),
        }
    }
    found.ok_or_else(|| AiError::invalid("an image request needs a prompt"))
}

fn fallback_media(req: &Request) -> &'static str {
    match req.params.get("output_format").and_then(Value::as_str) {
        Some("jpeg" | "jpg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
}

fn common_fields(model: &str, req: &Request, prompt: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("model".into(), json!(model));
    m.insert("prompt".into(), json!(prompt));
    if req.outputs > 1 {
        m.insert("n".into(), json!(req.outputs));
    }
    for key in PASS_THROUGH {
        if let Some(v) = req.params.get(*key) {
            m.insert((*key).to_owned(), v.clone());
        }
    }
    m
}

pub(crate) fn generate_plan(model: &str, req: &Request) -> Result<ImagePlan, AiError> {
    let prompt = prompt_text(req)?;
    let mut fields = common_fields(model, req, &prompt);
    if needs_response_format(model) {
        fields.insert("response_format".into(), json!("b64_json"));
    }
    let body = serde_json::to_vec(&Value::Object(fields))
        .map_err(|_| AiError::new(ErrorKind::Internal, "could not encode the request"))?;
    Ok(ImagePlan {
        path: "/images/generations",
        content_type: "application/json".to_owned(),
        body,
        fallback_media: fallback_media(req),
    })
}

/// What a picture is, from its first bytes. `None` for anything that is not PNG, JPEG or WebP.
pub(crate) fn sniff(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if data.get(0..4) == Some(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

fn extension(media: &str) -> &'static str {
    match media {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "png",
    }
}

/// An image to upload, already checked.
struct Upload<'a> {
    field: &'static str,
    media: &'static str,
    data: &'a [u8],
}

fn upload_for<'a>(
    field: &'static str,
    payload: &'a Payload,
    what: &str,
) -> Result<Upload<'a>, AiError> {
    let Payload::Inline { data, .. } = payload else {
        return Err(AiError::unsupported(format!(
            "the {what} must be inline image data; this adapter does not read files or URLs"
        )));
    };
    // Trust the bytes, not the label: a mislabelled file is refused here, not by the server.
    let media = sniff(data)
        .ok_or_else(|| AiError::invalid(format!("the {what} is not a PNG, JPEG or WebP image")))?;
    Ok(Upload { field, media, data })
}

pub(crate) fn edit_plan(model: &str, req: &Request) -> Result<ImagePlan, AiError> {
    let prompt = prompt_text(req)?;
    let mut images = Vec::new();
    for role in [InputRole::Source, InputRole::Reference] {
        for input in req.inputs.iter().filter(|i| i.role == role) {
            images.push(&input.payload);
        }
    }
    if images.is_empty() {
        return Err(AiError::invalid("an edit needs a source image"));
    }
    let field = if images.len() == 1 {
        "image"
    } else {
        "image[]"
    };
    let mut uploads = Vec::new();
    for p in images {
        uploads.push(upload_for(field, p, "image")?);
    }
    if let Some(mask) = req.inputs.iter().find(|i| i.role == InputRole::Mask) {
        let up = upload_for("mask", &mask.payload, "mask")?;
        if up.media != "image/png" {
            return Err(AiError::invalid("the mask must be a PNG image"));
        }
        uploads.push(up);
    }
    let mut fields = common_fields(model, req, &prompt);
    if needs_response_format(model) {
        fields.insert("response_format".into(), json!("b64_json"));
    }
    let (content_type, body) = multipart(&fields, &uploads);
    Ok(ImagePlan {
        path: "/images/edits",
        content_type,
        body,
        fallback_media: fallback_media(req),
    })
}

/// Builds a `multipart/form-data` body. The boundary is chosen so it appears nowhere in the data.
fn multipart(fields: &Map<String, Value>, files: &[Upload<'_>]) -> (String, Vec<u8>) {
    let mut salt: u64 = u64::from(std::process::id());
    let boundary = loop {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let candidate = format!("----craft-ai-{nanos:x}-{salt:x}");
        let needle = candidate.as_bytes();
        let clash = files.iter().any(|f| contains(f.data, needle))
            || fields.values().any(|v| v.to_string().contains(&candidate));
        if !clash {
            break candidate;
        }
        salt = salt.wrapping_add(1);
    };
    let mut out = Vec::new();
    for (name, value) in fields {
        let text = match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{text}\r\n"
            )
            .as_bytes(),
        );
    }
    for (i, f) in files.iter().enumerate() {
        let n = i + 1;
        out.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}{n}.{}\"\r\nContent-Type: {}\r\n\r\n",
                f.field,
                f.field.trim_end_matches("[]"),
                extension(f.media),
                f.media
            )
            .as_bytes(),
        );
        out.extend_from_slice(f.data);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), out)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Reads an images reply into outputs. Inline data is size-checked; a URL becomes a handle and is
/// never fetched here.
pub(crate) fn parse_images(
    v: &Value,
    fallback_media: &str,
) -> Result<(Vec<Output>, Option<Usage>), AiError> {
    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
        return Err(AiError::new(
            ErrorKind::Provider,
            crate::chat::error_text(e),
        ));
    }
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .filter(|d| !d.is_empty())
        .ok_or_else(|| AiError::new(ErrorKind::Provider, "the reply held no images"))?;
    let limits = Limits::default();
    let mut outputs = Vec::new();
    for (i, item) in data.iter().enumerate() {
        let label = (data.len() > 1).then(|| format!("image {}", i + 1));
        let payload = if let Some(b64) = item.get("b64_json").and_then(Value::as_str) {
            let bytes = base64_decode(b64).ok_or_else(|| {
                AiError::new(ErrorKind::Provider, "an image was not valid base64")
            })?;
            let media = sniff(&bytes).unwrap_or(fallback_media);
            Payload::inline(media, bytes)?
        } else if let Some(url) = item.get("url").and_then(Value::as_str) {
            if !url.starts_with("https://") {
                return Err(AiError::new(
                    ErrorKind::Provider,
                    "the server returned an image link that is not https",
                ));
            }
            Payload::handle(url, fallback_media, None)?
        } else {
            return Err(AiError::new(
                ErrorKind::Provider,
                "an image in the reply had no data",
            ));
        };
        payload.check(&limits).map_err(|e| {
            AiError::new(
                ErrorKind::Provider,
                format!("a returned image was refused: {e}"),
            )
        })?;
        outputs.push(Output { payload, label });
    }
    Ok((outputs, usage_from(v)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_ai_core::CapabilityId;
    use craft_ai_core::payload::base64_encode;

    const PNG: [u8; 12] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 1];

    fn png() -> Payload {
        Payload::inline("image/png", PNG.to_vec()).unwrap()
    }

    fn req(cap: &str) -> Request {
        Request::new(CapabilityId::parse(cap).unwrap())
            .with_input(InputRole::Prompt, Payload::text("a red fox"))
    }

    fn body_str(p: &ImagePlan) -> String {
        String::from_utf8_lossy(&p.body).into_owned()
    }

    #[test]
    fn generate_body_has_prompt_params_and_response_format_only_for_dall_e() {
        let r = req("image.generate")
            .with_param("size", json!("1024x1024"))
            .with_param("secret_thing", json!("x"))
            .with_outputs(2);
        let p = generate_plan("dall-e-3", &r).unwrap();
        let v: Value = serde_json::from_slice(&p.body).unwrap();
        assert_eq!(v["prompt"], "a red fox");
        assert_eq!(v["n"], 2);
        assert_eq!(v["size"], "1024x1024");
        assert_eq!(v["response_format"], "b64_json");
        assert!(v.get("secret_thing").is_none());
        let p = generate_plan("gpt-image-1", &r).unwrap();
        let v: Value = serde_json::from_slice(&p.body).unwrap();
        assert!(v.get("response_format").is_none());
        assert_eq!(p.path, "/images/generations");
    }

    #[test]
    fn prompt_is_required_and_must_be_text() {
        let no_prompt = Request::new(CapabilityId::parse("image.generate").unwrap());
        assert!(generate_plan("m", &no_prompt).is_err());
        let blank = Request::new(CapabilityId::parse("image.generate").unwrap())
            .with_input(InputRole::Prompt, Payload::text("   "));
        assert!(generate_plan("m", &blank).is_err());
    }

    #[test]
    fn edit_sends_source_and_mask_as_named_parts() {
        let r = req("image.inpaint")
            .with_input(InputRole::Source, png())
            .with_input(InputRole::Mask, png());
        let p = edit_plan("gpt-image-1", &r).unwrap();
        assert_eq!(p.path, "/images/edits");
        assert!(p.content_type.starts_with("multipart/form-data; boundary="));
        let s = body_str(&p);
        assert!(s.contains("name=\"image\"; filename=\"image1.png\""));
        assert!(s.contains("name=\"mask\"; filename=\"mask2.png\""));
        assert!(s.contains("name=\"prompt\"\r\n\r\na red fox"));
        assert!(s.contains("name=\"model\"\r\n\r\ngpt-image-1"));
        assert!(s.trim_end().ends_with("--"));
    }

    #[test]
    fn several_pictures_use_the_array_field() {
        let r = req("image.edit")
            .with_input(InputRole::Source, png())
            .with_input(InputRole::Reference, png());
        let s = body_str(&edit_plan("gpt-image-1", &r).unwrap());
        assert_eq!(s.matches("name=\"image[]\"").count(), 2);
    }

    #[test]
    fn bad_inputs_are_refused_with_a_reason() {
        let handle = Payload::handle("file:///a.png", "image/png", None).unwrap();
        let e = edit_plan(
            "m",
            &req("image.edit").with_input(InputRole::Source, handle),
        )
        .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unsupported);
        // Labelled PNG but not a picture.
        let fake = Payload::inline("image/png", b"not an image".to_vec()).unwrap();
        let e = edit_plan("m", &req("image.edit").with_input(InputRole::Source, fake)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::InvalidRequest);
        // Mask must be PNG.
        let jpeg = Payload::inline("image/jpeg", vec![0xff, 0xd8, 0xff, 0xe0]).unwrap();
        let r = req("image.inpaint")
            .with_input(InputRole::Source, png())
            .with_input(InputRole::Mask, jpeg);
        assert!(edit_plan("m", &r).is_err());
        assert!(edit_plan("m", &req("image.edit")).is_err());
    }

    #[test]
    fn boundary_never_appears_in_the_data() {
        let r = req("image.edit").with_input(InputRole::Source, png());
        let p = edit_plan("m", &r).unwrap();
        let boundary = p.content_type.rsplit('=').next().unwrap().to_owned();
        let count = body_str(&p).matches(&boundary).count();
        // One per field and file plus the closing line, never inside the picture bytes.
        assert!(count >= 4);
        assert!(!contains(&PNG, boundary.as_bytes()));
    }

    #[test]
    fn sniffing_recognises_formats_and_rejects_the_rest() {
        assert_eq!(sniff(&PNG), Some("image/png"));
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xdb]), Some("image/jpeg"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"GIF89a"), None);
        assert_eq!(sniff(&[]), None);
    }

    #[test]
    fn parses_base64_and_urls_and_labels_several() {
        let v = json!({"data":[{"b64_json": base64_encode(&PNG)},
            {"url":"https://cdn.example.com/a.png"}],
            "usage":{"input_tokens":10,"output_tokens":20}});
        let (out, usage) = parse_images(&v, "image/png").unwrap();
        assert_eq!(out.len(), 2);
        assert!(matches!(&out[0].payload, Payload::Inline { .. }));
        assert!(matches!(&out[1].payload, Payload::Handle(_)));
        assert_eq!(out[1].label.as_deref(), Some("image 2"));
        assert_eq!(usage.unwrap().input_units, Some(10));
    }

    #[test]
    fn hostile_replies_are_errors() {
        for v in [
            json!({}),
            json!({"data": []}),
            json!({"data":[{"b64_json":"!!!not base64"}]}),
            json!({"data":[{"url":"http://insecure.example.com/a.png"}]}),
            json!({"data":[{}]}),
            json!({"error":{"message":"content policy"}}),
        ] {
            assert!(parse_images(&v, "image/png").is_err(), "{v}");
        }
    }
}

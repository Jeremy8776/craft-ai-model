//! Parse one declared output node and fetch its locally saved media.
use crate::http::{Http, encode_query};
use craft_ai_core::job::{Event, Output};
use craft_ai_core::payload::Payload;
use craft_ai_core::{AiError, ErrorKind};
use craft_ai_media::sniff;
use serde_json::Value;
use std::io::Read;
use std::path::Path;

pub(crate) fn fetch(
    http: &Http,
    node: &Value,
    video: bool,
    output_dir: Option<&Path>,
    max_bytes: u64,
) -> Result<Event, AiError> {
    let keys: &[&str] = if video {
        &["videos", "gifs"]
    } else {
        &["images"]
    };
    let file = keys
        .iter()
        .find_map(|key| {
            node.get(key)
                .and_then(Value::as_array)
                .and_then(|list| list.first())
        })
        .ok_or_else(|| {
            AiError::new(
                ErrorKind::Provider,
                "workflow produced no mapped media output",
            )
        })?;
    let filename = file
        .get("filename")
        .and_then(Value::as_str)
        .ok_or_else(|| AiError::new(ErrorKind::Provider, "output has no filename"))?;
    let subfolder = file.get("subfolder").and_then(Value::as_str).unwrap_or("");
    let file_type = file.get("type").and_then(Value::as_str).unwrap_or("output");
    if file_type != "output"
        || !safe_path(filename)
        || (!subfolder.is_empty() && !safe_path(subfolder))
    {
        return Err(AiError::new(
            ErrorKind::Provider,
            "workflow output path or type is unsafe",
        ));
    }
    let media = match filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") if !video => "image/png",
        Some("jpg" | "jpeg") if !video => "image/jpeg",
        Some("webp") if !video => "image/webp",
        Some("mp4") if video => "video/mp4",
        Some("webm") if video => "video/webm",
        Some("gif") if video => "image/gif",
        _ => {
            return Err(AiError::unsupported(
                "workflow output media format is not supported",
            ));
        }
    };
    let path = format!(
        "/view?filename={}&type=output{}",
        encode_query(filename),
        if subfolder.is_empty() {
            String::new()
        } else {
            format!("&subfolder={}", encode_query(subfolder))
        }
    );
    let payload = if let Some(dir) = output_dir {
        let extension = filename
            .rsplit_once('.')
            .map(|(_, ext)| ext)
            .unwrap_or("bin");
        let mut temp = tempfile::Builder::new()
            .prefix("craft-ai-")
            .suffix(&format!(".{extension}"))
            .tempfile_in(dir)
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not create media output file"))?;
        let size = http.get_media_to(&path, temp.as_file_mut(), max_bytes)?;
        if size == 0 {
            return Err(AiError::new(
                ErrorKind::Provider,
                "workflow output is empty",
            ));
        }
        let mut head = Vec::new();
        std::fs::File::open(temp.path())
            .and_then(|f| f.take(sniff::HEAD_LEN as u64).read_to_end(&mut head))
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not check media output"))?;
        check_bytes(&head, media)?;
        temp.as_file()
            .sync_all()
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not save media output"))?;
        let uri = url::Url::from_file_path(temp.path())
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not name media output"))?
            .to_string();
        // Build the payload first: if it is refused, the temp file is still ours and is removed
        // when `temp` drops, instead of being left behind by an earlier `keep`.
        let payload = Payload::handle(&uri, media, Some(size))?;
        let _ = temp
            .keep()
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not retain media output"))?;
        payload
    } else {
        let bytes = http.get_media_limited(&path, max_bytes.min(32 * 1024 * 1024))?;
        if bytes.is_empty() {
            return Err(AiError::new(
                ErrorKind::Provider,
                "workflow output is empty",
            ));
        }
        check_bytes(&bytes, media)?;
        Payload::inline(media, bytes)?
    };
    Ok(Event::Output(Output {
        payload,
        label: None,
    }))
}

/// The output must be the media its file name says, judged by its first bytes. A server that
/// answers with an error page, a JSON body or another format is not handed on as an image.
fn check_bytes(bytes: &[u8], expected: &str) -> Result<(), AiError> {
    let head = bytes.get(..sniff::HEAD_LEN).unwrap_or(bytes);
    if sniff::media_type(head).map(|(media, _)| media) == Some(expected) {
        Ok(())
    } else {
        Err(AiError::new(
            ErrorKind::Provider,
            "workflow output is not the media its name says",
        ))
    }
}

fn safe_path(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && !s.contains("..")
        && !s.contains(['/', '\\', ':'])
        && !s.chars().any(char::is_control)
}

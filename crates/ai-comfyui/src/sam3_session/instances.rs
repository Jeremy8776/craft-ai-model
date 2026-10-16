//! Text grounding with per-instance detector metadata: masks come from `SaveImage`, boxes and
//! scores from `PreviewAny` text, which ComfyUI reports in the job history.
//!
//! ComfyUI reports nothing for an output node whose inputs are unchanged since an earlier run
//! (the history entry is simply absent), so an identical repeat would lose its boxes and scores.
//! Each request therefore prepends a unique stamp to the text via `StringConcatenate`. That
//! changes only the two text nodes' signatures, so the grounding inference stays cached, and
//! it doubles as a check that the text belongs to this request.
use super::{Prepared, Sam3Session};
use crate::sam3_detect::{Metadata, Sam3Detections};
use crate::sam3_masks::{bad, fetch};
use craft_ai_core::{AiError, ErrorKind};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A value unique to this request, safe to embed in a graph and in history text.
pub(super) fn stamp() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!(
        "craft-{nanos:x}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The text after `stamp` and a newline, or an error if the text is not from this request.
pub(super) fn unstamped<'a>(text: &'a str, stamp: &str) -> Result<&'a str, AiError> {
    text.strip_prefix(stamp)
        .and_then(|rest| rest.strip_prefix('\n'))
        .ok_or_else(|| bad("SAM 3 detection text is stale or not from this request"))
}

/// The grounding graph. With a `stamp`, the detector's boxes (output 2) and scores (output 3)
/// are also reported as stamped text by the `PreviewAny` nodes 8 and 9.
pub(super) fn grounding_graph(filename: &str, phrase: &str, stamp: Option<&str>) -> Value {
    let mut graph = json!({
        "1": {"class_type": "LoadImage", "inputs": {"image": filename}},
        "2": {"class_type": "LoadSAM3Model", "inputs": {"precision": "auto", "compile": false}},
        "3": {"class_type": "SAM3Grounding", "inputs": {
            "sam3_model_config": ["2", 0], "image": ["1", 0],
            "confidence_threshold": 0.2, "text_prompt": phrase, "max_detections": -1}},
        "4": {"class_type": "MaskToImage", "inputs": {"mask": ["3", 0]}},
        "5": {"class_type": "SaveImage", "inputs": {"images": ["4", 0], "filename_prefix": "craft-sam3-ground"}},
    });
    if let Some(stamp) = stamp {
        for (concat, preview, output) in [("6", "8", 2), ("7", "9", 3)] {
            graph[concat] = json!({"class_type": "StringConcatenate", "inputs": {
                "string_a": stamp, "string_b": ["3", output], "delimiter": "\n"}});
            graph[preview] = json!({"class_type": "PreviewAny", "inputs": {"source": [concat, 0]}});
        }
    }
    graph
}

impl Sam3Session {
    /// The prepared image and trimmed phrase, after the checks every grounding call shares.
    pub(super) fn grounding_request<'a>(
        &'a self,
        text: &'a str,
    ) -> Result<(&'a Prepared, &'a str), AiError> {
        let prepared = self
            .prepared
            .as_ref()
            .ok_or_else(|| AiError::invalid("SAM 3 image not prepared"))?;
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.len() > 200 {
            return Err(AiError::invalid(
                "SAM 3 text prompt must be 1 to 200 characters",
            ));
        }
        if !prepared.grounding {
            return Err(AiError::unsupported(
                "ComfyUI is missing the SAM3Grounding node",
            ));
        }
        Ok((prepared, trimmed))
    }

    /// Every matching instance with its mask, detector box, score and measured bounds.
    /// A nothing-matched, inconsistent or oversized result is an error, never a partial list.
    pub fn ground_instances(&self, text: &str) -> Result<Sam3Detections, AiError> {
        let (prepared, phrase) = self.grounding_request(text)?;
        if !prepared.preview_text {
            return Err(AiError::unsupported(
                "ComfyUI is missing the PreviewAny or StringConcatenate node, so SAM 3 boxes and scores are unavailable",
            ));
        }
        let stamp = stamp();
        let record = self.queue(grounding_graph(&prepared.filename, phrase, Some(&stamp)))?;
        let report = |node: &str, what: &str| {
            let text = record
                .pointer(&format!("/outputs/{node}/text/0"))
                .and_then(Value::as_str)
                .ok_or_else(|| bad(&format!("SAM 3 returned no detection {what}")))?;
            unstamped(text, &stamp)
        };
        let metadata = Metadata::parse(
            report("8", "boxes")?,
            report("9", "scores")?,
            prepared.width,
            prepared.height,
        )?;
        if metadata.is_empty() {
            return Err(AiError::new(
                ErrorKind::Provider,
                format!("Nothing matched \u{201c}{phrase}\u{201d}"),
            ));
        }
        let node = record
            .pointer("/outputs/5")
            .ok_or_else(|| bad("SAM 3 returned no selection masks"))?;
        let (width, height, masks) =
            fetch(&self.http, node, prepared.width, prepared.height)?.into_parts();
        metadata
            .attach(phrase, width, height, masks)
            .map(|found| found.stamped(self.revision()))
    }
}

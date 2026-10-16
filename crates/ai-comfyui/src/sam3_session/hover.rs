//! Hover previews and exact masks for point and box prompts on the prepared image. Previews use
//! the server's cached embedding through the hover route, without queueing a graph; commits
//! queue a graph so the saved mask is exact rather than a lossy JPEG.
use super::{Sam3Session, sam3};
use base64::Engine;
use craft_ai_core::{AiError, ErrorKind};
use serde_json::{Value, json};

impl Sam3Session {
    /// Return a JPEG overlay for a pixel inside the prepared image, without queueing a graph.
    pub fn preview(&self, x: f32, y: f32) -> Result<Vec<u8>, AiError> {
        let prepared = self.point(x, y)?;
        let response = self.http.post(
            "/sam3/interactive_segment_one",
            &json!({
                "node_id": prepared.node, "prompt_index": 0, "prompt_name": "Object selection",
                "prompt": {"positive_points": [{"x": x, "y": y}]},
            }),
        )?;
        if response.get("num_masks").and_then(Value::as_u64) != Some(1) {
            return Err(AiError::new(
                ErrorKind::Provider,
                "SAM 3 preview returned no object",
            ));
        }
        let encoded = response
            .get("overlay")
            .and_then(Value::as_str)
            .filter(|s| s.len() <= 24 * 1024 * 1024)
            .ok_or_else(|| {
                AiError::new(ErrorKind::Provider, "SAM 3 preview is missing or too large")
            })?;
        let jpeg = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| AiError::new(ErrorKind::Provider, "SAM 3 preview is not base64"))?;
        if !jpeg.starts_with(&[0xff, 0xd8]) || !jpeg.ends_with(&[0xff, 0xd9]) {
            return Err(AiError::new(
                ErrorKind::Provider,
                "SAM 3 preview is not JPEG",
            ));
        }
        // Preserve the association with the image checked before this HTTP call.
        let _ = prepared;
        Ok(jpeg)
    }

    /// Save the exact grayscale mask through the installed graph, not the lossy hover JPEG.
    pub fn commit(&self, x: f32, y: f32) -> Result<Vec<u8>, AiError> {
        let prepared = self.point(x, y)?;
        let graph = self.graph(
            prepared,
            json!([{
                "positive_points": [{"x": x, "y": y}], "session": prepared.nonce
            }]),
            true,
        );
        let record = self.queue(graph)?;
        let node = record
            .pointer("/outputs/5")
            .ok_or_else(|| AiError::new(ErrorKind::Provider, "SAM 3 returned no selection mask"))?;
        crate::sam3_masks::fetch(&self.http, node, prepared.width, prepared.height)?.union_png()
    }

    /// Return a JPEG overlay for the full prompt, without queueing a graph.
    pub fn preview_prompt(&self, prompt: &sam3::Prompt) -> Result<Vec<u8>, AiError> {
        let prepared = self.prompt_bounds(prompt)?;
        let response = self.http.post(
            "/sam3/interactive_segment_one",
            &json!({
                "node_id": prepared.node, "prompt_index": 0, "prompt_name": "Object selection",
                "prompt": {
                    "positive_points": prompt.positive_points.iter().map(|(x, y)| json!({"x": x, "y": y})).collect::<Vec<_>>(),
                    "negative_points": prompt.negative_points.iter().map(|(x, y)| json!({"x": x, "y": y})).collect::<Vec<_>>(),
                    "positive_boxes": match prompt.box_ {
                        Some([x1, y1, x2, y2]) => json!([{"x1": x1, "y1": y1, "x2": x2, "y2": y2}]),
                        None => json!([]),
                    },
                },
            }),
        )?;
        if response.get("num_masks").and_then(Value::as_u64) != Some(1) {
            return Err(AiError::new(
                ErrorKind::Provider,
                "SAM 3 preview returned no object",
            ));
        }
        let encoded = response
            .get("overlay")
            .and_then(Value::as_str)
            .filter(|s| s.len() <= 24 * 1024 * 1024)
            .ok_or_else(|| {
                AiError::new(ErrorKind::Provider, "SAM 3 preview is missing or too large")
            })?;
        let jpeg = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| AiError::new(ErrorKind::Provider, "SAM 3 preview is not base64"))?;
        if !jpeg.starts_with(&[0xff, 0xd8]) || !jpeg.ends_with(&[0xff, 0xd9]) {
            return Err(AiError::new(
                ErrorKind::Provider,
                "SAM 3 preview is not JPEG",
            ));
        }
        // Preserve the association with the image checked before this HTTP call.
        let _ = prepared;
        Ok(jpeg)
    }

    /// Save the exact grayscale mask for a full geometric prompt.
    pub fn commit_prompt(&self, prompt: &sam3::Prompt) -> Result<Vec<u8>, AiError> {
        let prepared = self.prompt_bounds(prompt)?;
        let graph = self.graph(prepared, prompt.store(prepared.nonce), true);
        self.mask_from(graph)
    }
}

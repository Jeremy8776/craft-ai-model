//! Point and box candidates through `SAM3Segmentation`, which offers several alternative masks
//! for an ambiguous prompt. Unlike hover this recomputes image features on every call, so it is
//! for a deliberate action, not for pointer movement. Scores travel as stamped `PreviewAny`
//! text for the reason given in `instances.rs`.
use super::instances::{stamp, unstamped};
use super::{Prepared, Sam3Session, sam3};
use crate::sam3_candidates::{Sam3Candidates, parse_scores};
use crate::sam3_masks::{bad, fetch};
use craft_ai_core::AiError;
use serde_json::{Value, json};

/// More points than any person places, and a bound on the graph text we send.
const MAX_POINTS: usize = 32;
/// The installed node accepts 0 to 10 refinement passes.
const MAX_REFINEMENT: u8 = 10;

fn coordinates(points: &[(f32, f32)]) -> String {
    Value::Array(
        points
            .iter()
            .map(|(x, y)| json!({"x": x, "y": y}))
            .collect(),
    )
    .to_string()
}

/// The first box as the normalised centre and size the box helper nodes take.
fn centre_size(corners: [f32; 4], width: u32, height: u32) -> [f64; 4] {
    let [ax, ay, bx, by] = corners.map(f64::from);
    let (w, h) = (f64::from(width), f64::from(height));
    // `prompt_bounds` keeps every corner inside the image, so all four values are in 0..1.
    [
        (ax + bx) / 2.0 / w,
        (ay + by) / 2.0 / h,
        (ax - bx).abs() / w,
        (ay - by).abs() / h,
    ]
}

fn candidates_graph(
    prepared: &Prepared,
    prompt: &sam3::Prompt,
    refinement: u8,
    stamp: &str,
) -> Value {
    let filename = &prepared.filename;
    let mut graph = json!({
        "1": {"class_type": "LoadImage", "inputs": {"image": filename}},
        "2": {"class_type": "LoadSAM3Model", "inputs": {"precision": "auto", "compile": false}},
        "10": {"class_type": "SAM3PointCollector", "inputs": {
            "image": ["1", 0], "points_store": "{}",
            "coordinates": coordinates(&prompt.positive_points),
            "neg_coordinates": coordinates(&prompt.negative_points)}},
        "3": {"class_type": "SAM3Segmentation", "inputs": {
            "sam3_model_config": ["2", 0], "image": ["1", 0],
            "positive_points": ["10", 0], "negative_points": ["10", 1],
            "refinement_iterations": refinement, "use_multimask": true, "output_best_mask": false}},
        "4": {"class_type": "MaskToImage", "inputs": {"mask": ["3", 0]}},
        "5": {"class_type": "SaveImage", "inputs": {"images": ["4", 0], "filename_prefix": "craft-sam3-candidates"}},
        "6": {"class_type": "StringConcatenate", "inputs": {
            "string_a": stamp, "string_b": ["3", 4], "delimiter": "\n"}},
        "8": {"class_type": "PreviewAny", "inputs": {"source": ["6", 0]}},
    });
    if let Some(corners) = prompt.box_ {
        let [center_x, center_y, width, height] =
            centre_size(corners, prepared.width, prepared.height);
        graph["11"] = json!({"class_type": "SAM3CreateBox", "inputs": {
            "center_x": center_x, "center_y": center_y,
            "width": width, "height": height, "is_positive": true}});
        graph["12"] = json!({"class_type": "SAM3CombineBoxes", "inputs": {"box_1": ["11", 0]}});
        graph["3"]["inputs"]["box"] = json!(["12", 0]);
    }
    graph
}

impl Sam3Session {
    /// Every alternative mask for a point and/or box prompt, in server order, never merged or
    /// chosen for the person. A missing node, an inconsistent or oversized result, or no object
    /// at all is an error, never a partial list. Only the first box is used by the node.
    pub fn candidates(&self, prompt: &sam3::Prompt) -> Result<Sam3Candidates, AiError> {
        self.candidates_refined(prompt, 0)
    }

    /// As `candidates`, with 0 to 10 refinement passes: each feeds the best mask back to the
    /// model for cleaner edges, at the cost of one more model call per pass.
    pub fn candidates_refined(
        &self,
        prompt: &sam3::Prompt,
        refinement_passes: u8,
    ) -> Result<Sam3Candidates, AiError> {
        let prepared = self.prompt_bounds(prompt)?;
        if refinement_passes > MAX_REFINEMENT {
            return Err(AiError::invalid(
                "SAM 3 refinement passes must be between 0 and 10",
            ));
        }
        if prompt.positive_points.len() + prompt.negative_points.len() > MAX_POINTS {
            return Err(AiError::invalid("SAM 3 prompt has too many points"));
        }
        if prompt.box_.is_some() && !prepared.boxes {
            return Err(AiError::unsupported(
                "ComfyUI is missing the SAM3CreateBox or SAM3CombineBoxes node",
            ));
        }
        if !prepared.segmentation {
            return Err(AiError::unsupported(
                "ComfyUI is missing the SAM3Segmentation or SAM3PointCollector node",
            ));
        }
        if !prepared.preview_text {
            return Err(AiError::unsupported(
                "ComfyUI is missing the PreviewAny or StringConcatenate node, so SAM 3 scores are unavailable",
            ));
        }
        let stamp = stamp();
        let record = self.queue(candidates_graph(
            prepared,
            prompt,
            refinement_passes,
            &stamp,
        ))?;
        let text = record
            .pointer("/outputs/8/text/0")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("SAM 3 returned no candidate scores"))?;
        let scores = parse_scores(unstamped(text, &stamp)?)?;
        if scores.is_empty() {
            return Err(bad("SAM 3 returned no candidates"));
        }
        let node = record
            .pointer("/outputs/5")
            .ok_or_else(|| bad("SAM 3 returned no candidate masks"))?;
        let (width, height, masks) =
            fetch(&self.http, node, prepared.width, prepared.height)?.into_parts();
        Sam3Candidates::assemble(width, height, masks, scores)
            .map(|found| found.stamped(self.revision()))
    }
}

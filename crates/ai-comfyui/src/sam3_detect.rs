//! Typed per-instance SAM 3 grounding results. Each instance keeps its mask, the detector's
//! box, its score and the bounds measured from the mask itself aligned by index. A count or
//! value that does not line up fails the whole result; nothing is guessed or dropped.
//! Detector boxes are model metadata; use mask bounds and `pick` for exact geometry.
use crate::sam3_lifecycle::Revision;
use crate::sam3_masks::{bad, union_masks};
use craft_ai_core::AiError;
use serde_json::Value;
use std::sync::OnceLock;

/// More than any plausible grounding result, and a hard cap on the text we will parse.
pub(crate) const MAX_INSTANCES: usize = 100;
pub(crate) const MAX_TEXT_BYTES: usize = 64 * 1024;
/// A pixel counts as part of an instance for picking at half coverage or more.
const PICK_COVERAGE: u8 = 128;

/// One detected instance. Debug output never contains media bytes.
pub struct Sam3Instance {
    index: usize,
    score: f32,
    detector_box: [f32; 4],
    mask_bounds: Option<[u32; 4]>,
    mask: Vec<u8>,
    width: u32,
    height: u32,
    coverage: OnceLock<Vec<u8>>,
}

impl Sam3Instance {
    /// Position in the server's score-ordered result.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Detector confidence in 0..=1.
    pub fn score(&self) -> f32 {
        self.score
    }

    /// The detector's `[x1, y1, x2, y2]` in source pixels. Model metadata, not the exact extent.
    pub fn detector_box(&self) -> [f32; 4] {
        self.detector_box
    }

    /// Exact `[x1, y1, x2, y2]` of nonzero mask pixels, `x2`/`y2` exclusive; `None` when empty.
    pub fn mask_bounds(&self) -> Option<[u32; 4]> {
        self.mask_bounds
    }

    /// The 8-bit grayscale PNG mask, same size as the source image.
    pub fn mask_png(&self) -> &[u8] {
        &self.mask
    }

    fn covers(&self, x: u32, y: u32) -> bool {
        let Some([x1, y1, x2, y2]) = self.mask_bounds else {
            return false;
        };
        if x < x1 || x >= x2 || y < y1 || y >= y2 {
            return false;
        }
        let pixels = self
            .coverage
            .get_or_init(|| decode(&self.mask, self.width, self.height).unwrap_or_default());
        let at = u64::from(y) * u64::from(self.width) + u64::from(x);
        usize::try_from(at)
            .ok()
            .and_then(|at| pixels.get(at))
            .is_some_and(|v| *v >= PICK_COVERAGE)
    }
}

impl std::fmt::Debug for Sam3Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sam3Instance")
            .field("index", &self.index)
            .field("score", &self.score)
            .field("detector_box", &self.detector_box)
            .field("mask_bounds", &self.mask_bounds)
            .finish_non_exhaustive()
    }
}

/// Every instance found for one phrase, in the server's score order.
#[derive(Debug)]
pub struct Sam3Detections {
    revision: Revision,
    phrase: String,
    width: u32,
    height: u32,
    instances: Vec<Sam3Instance>,
}

impl Sam3Detections {
    /// The prepared image these instances were found in; see `Sam3Session::is_current`.
    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub(crate) fn stamped(mut self, revision: Revision) -> Self {
        self.revision = revision;
        self
    }

    /// The text the instances were found for. All instances share this label.
    pub fn phrase(&self) -> &str {
        &self.phrase
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn instances(&self) -> &[Sam3Instance] {
        &self.instances
    }

    /// Explicit union of every instance by pixelwise maximum.
    pub fn union_png(&self) -> Result<Vec<u8>, AiError> {
        union_masks(
            self.width,
            self.height,
            self.instances.iter().map(|i| i.mask.as_slice()),
        )
    }

    /// The index of the highest scoring instance covering the pixel at half coverage or more.
    pub fn pick(&self, x: u32, y: u32) -> Option<usize> {
        self.instances
            .iter()
            .filter(|i| i.covers(x, y))
            .max_by(|a, b| a.score.total_cmp(&b.score).then(b.index.cmp(&a.index)))
            .map(|i| i.index)
    }
}

/// Boxes and scores parsed and checked against the image, before any mask is downloaded.
pub(crate) struct Metadata(Vec<([f32; 4], f32)>);

impl Metadata {
    pub(crate) fn parse(
        boxes: &str,
        scores: &str,
        width: u32,
        height: u32,
    ) -> Result<Self, AiError> {
        if boxes.len() > MAX_TEXT_BYTES || scores.len() > MAX_TEXT_BYTES {
            return Err(bad("SAM 3 detection text is too large"));
        }
        let boxes = list(boxes, "boxes")?;
        let scores = list(scores, "scores")?;
        if boxes.len() != scores.len() {
            return Err(bad("SAM 3 detection boxes and scores do not match"));
        }
        if boxes.len() > MAX_INSTANCES {
            return Err(bad("SAM 3 returned too many detections"));
        }
        let (w, h) = (width as f32, height as f32);
        let mut out = Vec::with_capacity(boxes.len());
        for (b, s) in boxes.iter().zip(&scores) {
            let corners: Vec<f32> = b
                .as_array()
                .filter(|a| a.len() == 4)
                .and_then(|a| a.iter().map(number).collect())
                .ok_or_else(|| bad("SAM 3 detection boxes are invalid"))?;
            let [x1, y1, x2, y2] = corners[..] else {
                return Err(bad("SAM 3 detection boxes are invalid"));
            };
            if x1 < 0.0 || y1 < 0.0 || x1 > x2 || y1 > y2 || x2 > w || y2 > h {
                return Err(bad("SAM 3 detection boxes are invalid"));
            }
            let score = number(s)
                .filter(|v| (0.0..=1.0).contains(v))
                .ok_or_else(|| bad("SAM 3 detection scores are invalid"))?;
            out.push(([x1, y1, x2, y2], score));
        }
        Ok(Self(out))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Align the masks with this metadata; the counts must be identical.
    pub(crate) fn attach(
        self,
        phrase: &str,
        width: u32,
        height: u32,
        masks: Vec<Vec<u8>>,
    ) -> Result<Sam3Detections, AiError> {
        if masks.len() != self.0.len() {
            return Err(bad("SAM 3 detections do not match the masks"));
        }
        let mut instances = Vec::with_capacity(masks.len());
        for (index, (mask, (detector_box, score))) in masks.into_iter().zip(self.0).enumerate() {
            let pixels = decode(&mask, width, height)?;
            instances.push(Sam3Instance {
                index,
                score,
                detector_box,
                mask_bounds: bounds(&pixels, width, height),
                mask,
                width,
                height,
                coverage: OnceLock::new(),
            });
        }
        Ok(Sam3Detections {
            revision: Revision::new(0),
            phrase: phrase.to_owned(),
            width,
            height,
            instances,
        })
    }
}

pub(crate) fn list(text: &str, what: &str) -> Result<Vec<Value>, AiError> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Array(items)) => Ok(items),
        _ => Err(bad(&format!("SAM 3 detection {what} are not a list"))),
    }
}

pub(crate) fn number(value: &Value) -> Option<f32> {
    value.as_f64().map(|v| v as f32).filter(|v| v.is_finite())
}

pub(crate) fn decode(png_bytes: &[u8], width: u32, height: u32) -> Result<Vec<u8>, AiError> {
    let mut reader = png::Decoder::new(png_bytes)
        .read_info()
        .map_err(|_| bad("SAM 3 returned an invalid mask"))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader
        .next_frame(&mut buf)
        .map_err(|_| bad("SAM 3 mask pixels are invalid"))?;
    buf.truncate(frame.buffer_size());
    let expected = u64::from(width) * u64::from(height);
    if frame.width != width || frame.height != height || buf.len() as u64 != expected {
        return Err(bad("SAM 3 mask pixels do not match the source"));
    }
    Ok(buf)
}

pub(crate) fn bounds(pixels: &[u8], width: u32, height: u32) -> Option<[u32; 4]> {
    let (mut x1, mut y1, mut x2, mut y2) = (width, height, 0, 0);
    for (i, value) in pixels.iter().enumerate() {
        if *value == 0 {
            continue;
        }
        let (x, y) = (i as u32 % width, i as u32 / width);
        x1 = x1.min(x);
        y1 = y1.min(y);
        x2 = x2.max(x + 1);
        y2 = y2.max(y + 1);
    }
    (x2 > x1 && y2 > y1).then_some([x1, y1, x2, y2])
}

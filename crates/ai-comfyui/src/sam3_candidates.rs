//! Segmentation candidates for one prompt: the alternatives SAM 3 offers, in server order, each
//! with its mask, the model's predicted quality score and the bounds measured from the mask.
//! Candidates are not labelled or ranked for the person; `highest_scoring` is only a helper.
use crate::sam3_detect::{MAX_TEXT_BYTES, bounds, decode, list, number};
use crate::sam3_lifecycle::Revision;
use crate::sam3_masks::bad;
use craft_ai_core::AiError;

/// The model offers at most this many alternatives per prompt.
pub(crate) const MAX_CANDIDATES: usize = 3;

/// One alternative mask. Debug output never contains media bytes.
pub struct Sam3Candidate {
    index: usize,
    score: f32,
    mask_bounds: Option<[u32; 4]>,
    mask: Vec<u8>,
}

impl Sam3Candidate {
    /// Position in the server's result, which is not sorted by score.
    pub fn index(&self) -> usize {
        self.index
    }

    /// The model's predicted quality for this mask. Not a probability that it is the one wanted.
    pub fn score(&self) -> f32 {
        self.score
    }

    /// Exact `[x1, y1, x2, y2]` of nonzero mask pixels, `x2`/`y2` exclusive; `None` when empty.
    pub fn mask_bounds(&self) -> Option<[u32; 4]> {
        self.mask_bounds
    }

    /// The 8-bit grayscale PNG mask, same size as the source image.
    pub fn mask_png(&self) -> &[u8] {
        &self.mask
    }
}

impl std::fmt::Debug for Sam3Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sam3Candidate")
            .field("index", &self.index)
            .field("score", &self.score)
            .field("mask_bounds", &self.mask_bounds)
            .finish_non_exhaustive()
    }
}

/// Every candidate for one prompt, in server order.
#[derive(Debug)]
pub struct Sam3Candidates {
    revision: Revision,
    width: u32,
    height: u32,
    candidates: Vec<Sam3Candidate>,
}

impl Sam3Candidates {
    /// The prepared image these candidates were computed from; see `Sam3Session::is_current`.
    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub(crate) fn stamped(mut self, revision: Revision) -> Self {
        self.revision = revision;
        self
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn candidates(&self) -> &[Sam3Candidate] {
        &self.candidates
    }

    /// Index of the candidate with the highest predicted score, earliest on a tie. The person
    /// may well want another one; applying this choice is the caller's decision.
    pub fn highest_scoring(&self) -> Option<usize> {
        self.candidates
            .iter()
            .max_by(|a, b| a.score.total_cmp(&b.score).then(b.index.cmp(&a.index)))
            .map(|c| c.index)
    }

    /// Align the masks with their scores; the counts must be identical and some mask non-empty.
    pub(crate) fn assemble(
        width: u32,
        height: u32,
        masks: Vec<Vec<u8>>,
        scores: Vec<f32>,
    ) -> Result<Self, AiError> {
        if masks.len() != scores.len() {
            return Err(bad("SAM 3 candidate scores do not match the masks"));
        }
        let mut candidates = Vec::with_capacity(masks.len());
        for (index, (mask, score)) in masks.into_iter().zip(scores).enumerate() {
            let pixels = decode(&mask, width, height)?;
            candidates.push(Sam3Candidate {
                index,
                score,
                mask_bounds: bounds(&pixels, width, height),
                mask,
            });
        }
        if candidates.iter().all(|c| c.mask_bounds.is_none()) {
            return Err(bad("SAM 3 found no object at that prompt"));
        }
        Ok(Self {
            revision: Revision::new(0),
            width,
            height,
            candidates,
        })
    }
}

/// Parse the scores text. Values must be finite numbers; the predicted quality is not clamped
/// because the model does not promise a 0 to 1 range.
pub(crate) fn parse_scores(text: &str) -> Result<Vec<f32>, AiError> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(bad("SAM 3 candidate text is too large"));
    }
    let items = list(text, "scores")?;
    if items.len() > MAX_CANDIDATES {
        return Err(bad("SAM 3 returned too many candidates"));
    }
    items
        .iter()
        .map(|v| number(v).ok_or_else(|| bad("SAM 3 candidate scores are invalid")))
        .collect()
}

//! Prepared local SAM 3 image embedding for fast hover previews and exact selection masks.
//! Each prepared image holds one of a few bounded server cache slots (see `sam3_lease`); each
//! prepare forces new image state.
use crate::http::Http;
use crate::sam3_lease::SlotLease;
use crate::sam3_lifecycle::{Revision, Sam3Canceller};

use craft_ai_core::endpoint::Endpoint;

use craft_ai_core::{AiError, ErrorKind};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod candidates;
mod hover;
mod instances;
mod queue;

const REQUIRED: &[&str] = &[
    "LoadImage",
    "LoadSAM3Model",
    "SAM3InteractiveCollector",
    "MaskToImage",
    "SaveImage",
];

mod prompt;
pub use prompt::sam3;

struct Prepared {
    /// The server cache slot's graph node id.
    node: String,
    filename: String,
    width: u32,
    height: u32,
    nonce: u128,
    /// The server has the text grounding node (checked once, during `prepare`).
    grounding: bool,
    /// The server has `PreviewAny` and `StringConcatenate`, which carry stamped boxes and scores out.
    preview_text: bool,
    /// The server has `SAM3Segmentation` and `SAM3PointCollector` (point candidates).
    segmentation: bool,
    /// The server has the box helper nodes (`SAM3CreateBox`, `SAM3CombineBoxes`).
    boxes: bool,
}

/// A single local session. Calls block and must never run on the UI thread.
/// Each prepared image holds its own bounded server cache slot, so sessions (in this or another
/// process) do not replace each other's hover cache; when all slots are taken, `prepare` is
/// refused until one session is released or dropped.
pub struct Sam3Session {
    http: Http,
    prepared: Option<Prepared>,
    /// Short name of the server address, so each server has its own pool of slots.
    server: String,
    lock_dir: PathBuf,
    lease: Option<SlotLease>,
    revision: u64,
    canceller: Sam3Canceller,
    targeted_interrupt: bool,
}

impl Sam3Session {
    pub fn new(base: &str) -> Result<Self, AiError> {
        let endpoint =
            Endpoint::parse(base).map_err(|_| AiError::invalid("invalid ComfyUI address"))?;
        if !endpoint.is_loopback() || base != endpoint.fingerprint() {
            return Err(AiError::invalid(
                "SAM 3 needs a loopback ComfyUI address without a path",
            ));
        }
        Ok(Self {
            http: Http::new(
                base.to_owned(),
                Duration::from_secs(3),
                Duration::from_secs(15),
            ),
            prepared: None,
            server: base
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect(),
            lock_dir: std::env::temp_dir().join("craft-ai-sam3"),
            lease: None,
            revision: 0,
            canceller: Sam3Canceller::default(),
            targeted_interrupt: false,
        })
    }

    /// Keep the slot locks in `dir` instead of the default folder under the system temp
    /// directory. All sessions that share a server must agree on the folder.
    pub fn with_lock_dir(mut self, dir: PathBuf) -> Self {
        self.lock_dir = dir;
        self
    }

    /// Allow cancelling to interrupt a running prompt. Only turn this on for a ComfyUI that scopes
    /// `/interrupt` to a prompt id (the installed 0.22 does; older versions interrupt whatever
    /// is running). Off, cancelling stops the wait and removes the prompt if still queued.
    pub fn with_targeted_interrupt(mut self, confirmed: bool) -> Self {
        self.targeted_interrupt = confirmed;
        self
    }

    /// A handle that cancels this session's in-flight request from another thread.
    pub fn canceller(&self) -> Sam3Canceller {
        self.canceller.clone()
    }

    /// The revision of the image this session holds (0 before the first `prepare`).
    pub fn revision(&self) -> Revision {
        Revision::new(self.revision)
    }

    /// Whether a result stamped with `revision` came from the image the session holds now.
    pub fn is_current(&self, revision: Revision) -> bool {
        self.prepared.is_some() && revision == self.revision()
    }

    /// The server cache slot this session holds, once it has prepared an image.
    pub fn slot(&self) -> Option<usize> {
        self.lease.as_ref().map(SlotLease::slot)
    }

    /// Give up the prepared image and its slot, for example when its document closes.
    /// The server's cached tensors stay until another session reuses the slot.
    pub fn release(&mut self) {
        self.revision += 1;
        self.prepared = None;
        self.lease = None;
    }

    /// Confirm that the loopback listener is ComfyUI rather than another local service.
    pub fn verify_server(&self) -> Result<(), AiError> {
        let stats = self.http.get_json("/system_stats")?;
        if stats
            .pointer("/system/comfyui_version")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(AiError::unsupported(
                "The local port is not serving ComfyUI",
            ));
        }
        Ok(())
    }

    /// Upload the PNG once and compute its image embedding as the tool activates.
    pub fn prepare(&mut self, image: &[u8]) -> Result<Revision, AiError> {
        // Anything computed before this call is now stale, whether or not the call succeeds.
        self.revision += 1;
        self.prepared = None;
        if image.len() < 8
            || image.len() > 16 * 1024 * 1024
            || !image.starts_with(b"\x89PNG\r\n\x1a\n")
        {
            return Err(AiError::invalid("SAM 3 needs a PNG image under 16 MiB"));
        }
        let reader = png::Decoder::new(image)
            .read_info()
            .map_err(|_| AiError::invalid("SAM 3 needs a valid PNG image"))?;
        let (width, height) = (reader.info().width, reader.info().height);
        if width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || u64::from(width) * u64::from(height) > 16_777_216
        {
            return Err(AiError::invalid(
                "SAM 3 source image dimensions are invalid",
            ));
        }
        let nodes = self.http.get_object_info()?;
        for class in REQUIRED {
            if nodes.get(*class).is_none() {
                return Err(AiError::unsupported(format!(
                    "ComfyUI is missing the {class} node"
                )));
            }
        }
        if self.lease.is_none() {
            self.lease = Some(SlotLease::acquire(&self.lock_dir, &self.server)?);
        }
        let node = self
            .lease
            .as_ref()
            .map(SlotLease::node_id)
            .unwrap_or_default();
        let filename = self
            .http
            .upload_image("sam3-interactive", "image/png", image)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let prepared = Prepared {
            node,
            filename,
            width,
            height,
            nonce,
            grounding: nodes.get("SAM3Grounding").is_some(),
            boxes: nodes.get("SAM3CreateBox").is_some() && nodes.get("SAM3CombineBoxes").is_some(),
            segmentation: nodes.get("SAM3Segmentation").is_some()
                && nodes.get("SAM3PointCollector").is_some(),
            preview_text: nodes.get("PreviewAny").is_some()
                && nodes.get("StringConcatenate").is_some(),
        };
        let graph = self.graph(&prepared, json!([{"session":nonce}]), false);
        self.queue(graph)?;
        self.prepared = Some(prepared);
        Ok(self.revision())
    }

    /// Text grounding: segment everything matching the phrase, reusing the prepared image.
    pub fn ground(&self, text: &str) -> Result<Vec<u8>, AiError> {
        self.ground_masks(text)?.union_png()
    }

    /// Preserve every matching instance in server order; no partial batch is returned on failure.
    pub fn ground_masks(&self, text: &str) -> Result<crate::Sam3MaskBatch, AiError> {
        let (prepared, trimmed) = self.grounding_request(text)?;
        let masks = self.masks_from(instances::grounding_graph(
            &prepared.filename,
            trimmed,
            None,
        ))?;
        if !masks.has_pixels()? {
            return Err(AiError::new(
                ErrorKind::Provider,
                format!("Nothing matched \u{201c}{trimmed}\u{201d}"),
            ));
        }
        Ok(masks)
    }

    /// Try each phrase in order and return the first that matches something, with the phrase.
    /// The model only understands concrete nouns, so a generic "subject" needs several guesses.
    pub fn ground_first(&self, texts: &[&str]) -> Result<(String, Vec<u8>), AiError> {
        let mut last = AiError::invalid("SAM 3 needs at least one phrase");
        for text in texts {
            match self.ground(text) {
                Ok(mask) => return Ok(((*text).to_owned(), mask)),
                Err(e) if e.message.starts_with("Nothing matched") => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(if texts.len() > 1 {
            AiError::new(
                ErrorKind::Provider,
                "No subject found. Type what to select, such as person or car",
            )
        } else {
            last
        })
    }

    fn mask_from(&self, graph: Value) -> Result<Vec<u8>, AiError> {
        self.masks_from(graph)?.union_png()
    }

    fn masks_from(&self, graph: Value) -> Result<crate::Sam3MaskBatch, AiError> {
        let prepared = self
            .prepared
            .as_ref()
            .ok_or_else(|| AiError::invalid("SAM 3 image not prepared"))?;
        let record = self.queue(graph)?;
        let node = record
            .pointer("/outputs/5")
            .ok_or_else(|| AiError::new(ErrorKind::Provider, "SAM 3 returned no selection mask"))?;
        crate::sam3_masks::fetch(&self.http, node, prepared.width, prepared.height)
    }

    /// Validate every point and the box against the prepared image.
    fn prompt_bounds(&self, prompt: &sam3::Prompt) -> Result<&Prepared, AiError> {
        let prepared = self
            .prepared
            .as_ref()
            .ok_or_else(|| AiError::invalid("SAM 3 image not prepared"))?;
        if prompt.is_empty() {
            return Err(AiError::invalid("SAM 3 prompt is empty"));
        }
        let inside = |x: f32, y: f32| {
            x.is_finite()
                && y.is_finite()
                && x >= 0.0
                && y >= 0.0
                && x < prepared.width as f32
                && y < prepared.height as f32
        };
        let mut all = prompt.positive_points.iter().chain(&prompt.negative_points);
        if !all.all(|(x, y)| inside(*x, *y)) {
            return Err(AiError::invalid(
                "SAM 3 prompt points must be inside the image",
            ));
        }
        if let Some([x1, y1, x2, y2]) = prompt.box_ {
            let (lo_x, hi_x) = (x1.min(x2), x1.max(x2));
            let (lo_y, hi_y) = (y1.min(y2), y1.max(y2));
            if !inside(lo_x, lo_y) || !inside(hi_x.max(lo_x), hi_y.max(lo_y)) {
                return Err(AiError::invalid(
                    "SAM 3 prompt box must be inside the image",
                ));
            }
            if hi_x - lo_x < 1.0 || hi_y - lo_y < 1.0 {
                return Err(AiError::invalid("SAM 3 prompt box is degenerate"));
            }
        }
        Ok(prepared)
    }

    fn point(&self, x: f32, y: f32) -> Result<&Prepared, AiError> {
        let p = self
            .prepared
            .as_ref()
            .ok_or_else(|| AiError::invalid("SAM 3 image not prepared"))?;
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x >= p.width as f32
            || y >= p.height as f32
        {
            return Err(AiError::invalid("SAM 3 point must be inside the image"));
        }
        Ok(p)
    }

    fn graph(&self, p: &Prepared, prompts: Value, mask: bool) -> Value {
        let mut graph = json!({
            "1":{"class_type":"LoadImage","inputs":{"image":p.filename}},
            "2":{"class_type":"LoadSAM3Model","inputs":{"precision":"auto","compile":false}},
        });
        // The collector's node id is the server's cache key, so it names this session's slot.
        graph[p.node.as_str()] = json!({"class_type":"SAM3InteractiveCollector","inputs":{
            "sam3_model_config":["2",0],"image":["1",0],
            "multi_prompts_store": prompts.to_string()
        }});
        if mask {
            graph["4"] = json!({"class_type":"MaskToImage","inputs":{"mask":[p.node,0]}});
            graph["5"] = json!({"class_type":"SaveImage","inputs":{"images":["4",0],"filename_prefix":"craft-sam3-selection"}});
        }
        graph
    }
}

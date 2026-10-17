//! SAM 3 on Candle as a [`ModelRunner`]: the whole of what an app gets.
//!
//! The request is the shared `image.segment` one: a PNG or JPEG `source` (told apart by its first
//! bytes), and either a `prompt` (a noun phrase) or `points` in params (`[{"x": 0.5, "y": 0.4,
//! "positive": true}]`, normalised to the picture). The answer is one grayscale PNG mask the size
//! of the picture as a person sees it, white where selected. A JPEG's EXIF turn is applied, so a
//! phone photo taken sideways comes back upright and the mask is upright too; a PNG is as stored.
//! `threshold` (0..1, default 0.5) sets how sure a match must be for a text prompt.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;

use craft_ai_core::{
    AiError, CapabilityId, CapabilitySpec, ErrorKind, InputRole, ModelId, ModelInfo, Output,
    Payload, Request,
};
use craft_ai_engine::{LoadedModel, ModelRunner, RunContext};
use craft_ai_features::GpuVendor;
use serde_json::Value;

use crate::image_io::{decode_picture, encode_mask};
use crate::{Click, Encoded, Sam3, WEIGHTS_FILE, is_model_dir, mask};

/// Most clicks one request may carry.
const MAX_POINTS: usize = 32;

/// The engine and model ids as the catalogue writes them (`NativeRoute`).
const ENGINE: &str = "candle";
const MODEL: &str = "sam3";

/// Every runner this crate offers, for an `EngineRegistry`.
pub fn runners() -> Vec<Arc<dyn ModelRunner>> {
    vec![Arc::new(Sam3Runner)]
}

/// SAM 3 on the Candle engine.
pub struct Sam3Runner;

impl ModelRunner for Sam3Runner {
    fn engine(&self) -> &str {
        ENGINE
    }

    fn model(&self) -> ModelInfo {
        let spec = |capability: CapabilityId| {
            CapabilitySpec::new(capability)
                .accepting(InputRole::Source, 1)
                .accepting(InputRole::Prompt, 1)
                .requiring(InputRole::Source)
        };
        let mut info = ModelInfo::new(model_id(), "SAM 3, built in");
        if let Ok(capability) = CapabilityId::parse("image.segment") {
            info = info.with(spec(capability));
        }
        info
    }

    fn accelerators(&self) -> &[GpuVendor] {
        if cfg!(target_os = "macos") {
            &[GpuVendor::Apple]
        } else {
            &[]
        }
    }

    fn load(&self, weights: &Path) -> Result<Box<dyn LoadedModel>, AiError> {
        let dir = weights.parent().filter(|_| {
            weights
                .file_name()
                .is_some_and(|n| n == std::ffi::OsStr::new(WEIGHTS_FILE))
        });
        let Some(dir) = dir.filter(|d| is_model_dir(d)) else {
            return Err(AiError::unsupported(
                "SAM 3 needs model.safetensors, vocab.json and merges.txt together in one folder. Open Set up AI to download them.",
            ));
        };
        let model = Sam3::load(dir).map_err(|e| {
            AiError::new(
                ErrorKind::Provider,
                format!("SAM 3 could not be loaded: {e}"),
            )
        })?;
        Ok(Box::new(Loaded { model, last: None }))
    }
}

#[allow(clippy::expect_used)] // `sam3` is a valid id: a literal that cannot fail
fn model_id() -> ModelId {
    ModelId::new(MODEL).expect("sam3 is a valid model id")
}

/// The model in memory, and the last picture it looked at.
struct Loaded {
    model: Sam3,
    last: Option<(u64, Encoded)>,
}

/// What to select, read from the request.
enum Prompt {
    Text(String),
    Points(Vec<Click>),
}

impl LoadedModel for Loaded {
    fn run(&mut self, request: &Request, ctx: &RunContext) -> Result<Vec<Output>, AiError> {
        let (prompt, threshold) = read_prompt(request)?;
        let Some(source) = request
            .inputs
            .iter()
            .find(|i| i.role == InputRole::Source)
            .map(|i| &i.payload)
        else {
            return Err(AiError::invalid("A picture is needed."));
        };
        let Payload::Inline { data, .. } = source else {
            return Err(AiError::unsupported(
                "SAM 3 needs the picture itself, not a link to it.",
            ));
        };
        let picture = decode_picture(data)?;
        ctx.progress(5);
        ctx.check_cancelled()?;

        let key = {
            let mut h = DefaultHasher::new();
            data.hash(&mut h);
            h.finish()
        };
        if self.last.as_ref().is_none_or(|(k, _)| *k != key) {
            self.last = None;
            ctx.notice(
                "Looking at the picture. This is the slow part and happens once per picture.",
            );
            let encoded = self
                .model
                .encode(&picture.bytes, picture.width, picture.height)
                .map_err(provider_error)?;
            self.last = Some((key, encoded));
        }
        ctx.progress(80);
        ctx.check_cancelled()?;

        let Some((_, encoded)) = self.last.as_mut() else {
            return Err(AiError::new(
                ErrorKind::Internal,
                "The picture was not kept.",
            ));
        };
        let prediction = match &prompt {
            Prompt::Points(points) => Some(
                self.model
                    .segment_clicks(encoded, points)
                    .map_err(provider_error)?,
            ),
            Prompt::Text(text) => self
                .model
                .segment_text(encoded, text, threshold)
                .map_err(provider_error)?,
        };
        ctx.progress(95);

        let (alpha, label) = match prediction {
            Some(p) => (
                mask::alpha(&p.logits, crate::MASK_SIDE, picture.width, picture.height),
                format!("confidence {:.2}", p.score),
            ),
            None => {
                ctx.notice("Nothing in the picture matched that description.");
                (
                    vec![0.0; picture.width * picture.height],
                    "no match".to_owned(),
                )
            }
        };
        let png = encode_mask(&alpha, picture.width, picture.height)?;
        Ok(vec![Output {
            payload: Payload::inline("image/png", png)?,
            label: Some(label),
        }])
    }
}

fn provider_error(e: crate::Error) -> AiError {
    AiError::new(ErrorKind::Provider, format!("SAM 3 failed: {e}"))
}

/// Reads what to select and how sure a text match must be.
fn read_prompt(request: &Request) -> Result<(Prompt, f32), AiError> {
    let threshold = match request.params.get("threshold") {
        None => 0.5,
        Some(v) => v
            .as_f64()
            .filter(|t| t.is_finite() && (0.0..=1.0).contains(t))
            .ok_or_else(|| AiError::invalid("threshold must be a number from 0 to 1"))?
            as f32,
    };
    let text = request
        .inputs
        .iter()
        .find_map(|i| match (&i.role, &i.payload) {
            (InputRole::Prompt, Payload::Text { text }) if !text.trim().is_empty() => {
                Some(text.trim().to_owned())
            }
            _ => None,
        });
    let points = match request.params.get("points") {
        None => Vec::new(),
        Some(v) => read_points(v)?,
    };
    match (text, points.is_empty()) {
        (Some(_), false) => Err(AiError::invalid(
            "Say what to select in words, or click on it, not both.",
        )),
        (Some(text), true) => Ok((Prompt::Text(text), threshold)),
        (None, false) => Ok((Prompt::Points(points), threshold)),
        (None, true) => Err(AiError::invalid(
            "Say what to select in words, or click on it.",
        )),
    }
}

fn read_points(value: &Value) -> Result<Vec<Click>, AiError> {
    let wrong = || AiError::invalid("points must be a list of {x, y, positive} inside the picture");
    let list = value.as_array().ok_or_else(wrong)?;
    if list.len() > MAX_POINTS {
        return Err(AiError::invalid(format!("at most {MAX_POINTS} points")));
    }
    list.iter()
        .map(|p| {
            let coord = |key: &str| {
                p.get(key)
                    .and_then(Value::as_f64)
                    .filter(|c| c.is_finite() && (0.0..=1.0).contains(c))
                    .map(|c| c as f32)
                    .ok_or_else(wrong)
            };
            Ok(Click {
                x: coord("x")?,
                y: coord("y")?,
                positive: p
                    .get("positive")
                    .and_then(Value::as_bool)
                    .ok_or_else(wrong)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request() -> Request {
        Request::new(CapabilityId::parse("image.segment").unwrap())
            .with_input(InputRole::Source, Payload::text("x"))
    }

    #[test]
    fn it_is_the_candle_sam3_the_catalogue_names() {
        let route = craft_ai_features::builtin()
            .into_iter()
            .find(|f| f.id == "select-subject")
            .and_then(|f| f.local)
            .and_then(|l| l.native.into_iter().find(|n| n.provider == "candle"))
            .unwrap();
        let runner = Sam3Runner;
        assert_eq!(runner.engine(), route.provider);
        assert_eq!(runner.model().id.as_str(), route.model);
        let info = runner.model();
        let spec = info
            .spec_for(&CapabilityId::parse("image.segment").unwrap())
            .unwrap();
        assert!(spec.requires.contains(&InputRole::Source));
    }

    #[test]
    fn a_word_or_clicks_are_needed_but_not_both() {
        assert!(read_prompt(&request()).is_err());
        let words = request().with_input(InputRole::Prompt, Payload::text(" the sky "));
        assert!(matches!(read_prompt(&words).unwrap(), (Prompt::Text(t), _) if t == "the sky"));
        let clicks =
            request().with_param("points", json!([{"x": 0.5, "y": 0.2, "positive": true}]));
        assert!(matches!(read_prompt(&clicks).unwrap().0, Prompt::Points(p) if p.len() == 1));
        let both = words.with_param("points", json!([{"x": 0.5, "y": 0.2, "positive": true}]));
        assert!(read_prompt(&both).is_err());
    }

    #[test]
    fn hostile_numbers_are_refused() {
        let words = request().with_input(InputRole::Prompt, Payload::text("sky"));
        for bad in [json!(1.5), json!(-0.1), json!("high")] {
            assert!(read_prompt(&words.clone().with_param("threshold", bad)).is_err());
        }
        for points in [
            json!([{"x": 1.5, "y": 0.2, "positive": true}]),
            json!([{"x": 0.5, "y": 0.2}]),
            json!("middle"),
            json!(vec![
                json!({"x": 0.1, "y": 0.1, "positive": true});
                MAX_POINTS + 1
            ]),
        ] {
            assert!(read_prompt(&request().with_param("points", points)).is_err());
        }
    }

    #[test]
    fn weights_without_their_tokenizer_files_say_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let weights = dir.path().join("model.safetensors");
        std::fs::write(&weights, b"x").unwrap();
        let e = Sam3Runner.load(&weights).err().unwrap();
        assert_eq!(e.kind, ErrorKind::Unsupported);
        assert!(e.message.contains("vocab.json") && e.message.contains("Set up AI"));
        let other = dir.path().join("sam3.safetensors");
        std::fs::write(&other, b"x").unwrap();
        assert!(Sam3Runner.load(&other).is_err());
    }
}

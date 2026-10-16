//! Which models an OpenAI-compatible server offers, and what each can do.
//!
//! `GET /models` returns only ids, so capabilities are inferred from the id. The inference is a
//! pure function so it is easy to test and to override: pass your own `Vec<ModelInfo>` in
//! [`OpenAiConfig::models`](crate::OpenAiConfig) when the guess is wrong for a server.
//!
//! Known image models get image capabilities. Ids that are plainly not chat or image models
//! (embeddings, speech, moderation, video) are dropped. Everything else is treated as a chat model
//! that can stream, call tools and return structured output. That last part is an assumption:
//! plenty of local models cannot, and the server will say so when asked.

use craft_ai_core::{CapabilityId, CapabilitySpec, Features, InputRole, ModelId, ModelInfo};

/// Substrings of ids that are not chat or image models.
const NOT_SUPPORTED: &[&str] = &[
    "embedding",
    "embed",
    "whisper",
    "tts",
    "transcribe",
    "moderation",
    "davinci",
    "babbage",
    "realtime",
    "audio",
    "sora",
    "rerank",
];

/// How many pictures one request may carry as `image[]` for the newer image models.
const GPT_IMAGE_REFERENCES: u32 = 15;

fn spec(capability: &str) -> Option<CapabilitySpec> {
    CapabilityId::parse(capability)
        .ok()
        .map(CapabilitySpec::new)
}

fn text_spec() -> Option<CapabilitySpec> {
    Some(
        spec("text.complete")?
            .accepting(InputRole::Prompt, 64)
            .requiring(InputRole::Prompt)
            .accepting(InputRole::Context, 64)
            .accepting(InputRole::System, 1)
            .with_features(Features {
                streaming: true,
                tools: true,
                structured_output: true,
            }),
    )
}

fn generate_spec(max_outputs: u32) -> Option<CapabilitySpec> {
    Some(
        spec("image.generate")?
            .accepting(InputRole::Prompt, 1)
            .requiring(InputRole::Prompt)
            .max_outputs(max_outputs),
    )
}

/// `image.edit` (mask optional) or `image.inpaint` (mask required).
fn edit_spec(capability: &str, max_outputs: u32, references: u32) -> Option<CapabilitySpec> {
    let mut s = spec(capability)?
        .accepting(InputRole::Prompt, 1)
        .requiring(InputRole::Prompt)
        .accepting(InputRole::Source, 1)
        .requiring(InputRole::Source)
        .accepting(InputRole::Mask, 1)
        .max_outputs(max_outputs);
    if references > 0 {
        s = s.accepting(InputRole::Reference, references);
    }
    if capability == "image.inpaint" {
        s = s.requiring(InputRole::Mask);
    }
    Some(s)
}

/// What an id is probably capable of, or `None` if this adapter does not serve it.
pub fn classify(id: &str) -> Option<ModelInfo> {
    let model_id = ModelId::new(id).ok()?;
    let lower = id.to_ascii_lowercase();
    let specs: Vec<CapabilitySpec> = if lower.contains("gpt-image") {
        [
            generate_spec(10),
            edit_spec("image.edit", 10, GPT_IMAGE_REFERENCES),
            edit_spec("image.inpaint", 10, GPT_IMAGE_REFERENCES),
        ]
        .into_iter()
        .flatten()
        .collect()
    } else if lower.contains("dall-e-3") {
        generate_spec(1).into_iter().collect()
    } else if lower.contains("dall-e") {
        [
            generate_spec(10),
            edit_spec("image.edit", 10, 0),
            edit_spec("image.inpaint", 10, 0),
        ]
        .into_iter()
        .flatten()
        .collect()
    } else if NOT_SUPPORTED.iter().any(|k| lower.contains(k)) {
        return None;
    } else {
        text_spec().into_iter().collect()
    };
    if specs.is_empty() {
        return None;
    }
    Some(
        specs
            .into_iter()
            .fold(ModelInfo::new(model_id, id), ModelInfo::with),
    )
}

/// True when this id is one of the original DALL-E models, which need `response_format` to return
/// image data instead of a URL.
pub(crate) fn needs_response_format(id: &str) -> bool {
    id.to_ascii_lowercase().starts_with("dall-e")
}

/// What a person can declare a model of a custom server does. Each is the plain, least
/// demanding form of the ability: no streaming, tools or structured output, one picture per
/// request, no reference images. Nothing is offered that was not declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Chat,
    ImageGenerate,
    ImageEdit,
    ImageInpaint,
}

/// A model with exactly the abilities in `kinds` (repeats ignored, listed in a fixed order), or
/// `None` if `id` cannot be a model id here or no kind was given.
pub fn declare(id: &str, kinds: &[Kind]) -> Option<ModelInfo> {
    let model_id = ModelId::new(id).ok()?;
    let mut kinds = kinds.to_vec();
    kinds.sort();
    kinds.dedup();
    let specs: Vec<CapabilitySpec> = kinds
        .iter()
        .filter_map(|kind| match kind {
            Kind::Chat => plain_text_spec(),
            Kind::ImageGenerate => generate_spec(1),
            Kind::ImageEdit => edit_spec("image.edit", 1, 0),
            Kind::ImageInpaint => edit_spec("image.inpaint", 1, 0),
        })
        .collect();
    if specs.is_empty() || specs.len() != kinds.len() {
        return None;
    }
    Some(
        specs
            .into_iter()
            .fold(ModelInfo::new(model_id, id), ModelInfo::with),
    )
}

/// What the id suggests, as a starting point for a person to confirm or change. A guess from a
/// name; it proves nothing about the server.
pub fn suggest(id: &str) -> Vec<Kind> {
    let Some(info) = classify(id) else {
        return Vec::new();
    };
    let mut kinds: Vec<Kind> = info
        .capabilities
        .iter()
        .filter_map(|s| match s.capability.as_str() {
            "text.complete" => Some(Kind::Chat),
            "image.generate" => Some(Kind::ImageGenerate),
            "image.edit" => Some(Kind::ImageEdit),
            "image.inpaint" => Some(Kind::ImageInpaint),
            _ => None,
        })
        .collect();
    kinds.sort();
    kinds
}

fn plain_text_spec() -> Option<CapabilitySpec> {
    Some(
        spec("text.complete")?
            .accepting(InputRole::Prompt, 64)
            .requiring(InputRole::Prompt)
            .accepting(InputRole::Context, 64)
            .accepting(InputRole::System, 1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(id: &str) -> Vec<String> {
        classify(id)
            .map(|m| {
                m.capabilities
                    .iter()
                    .map(|c| c.capability.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn chat_models_complete_text_with_features() {
        let m = classify("gpt-4o-mini").unwrap();
        let c = CapabilityId::parse("text.complete").unwrap();
        let s = m.spec_for(&c).unwrap();
        assert!(s.features.streaming && s.features.tools && s.features.structured_output);
        assert_eq!(caps("llama3.2:3b"), ["text.complete"]);
        assert!(s.requires.contains(&InputRole::Prompt));
    }

    #[test]
    fn image_models_get_image_capabilities_only() {
        assert_eq!(
            caps("gpt-image-1"),
            ["image.generate", "image.edit", "image.inpaint"]
        );
        assert_eq!(caps("dall-e-3"), ["image.generate"]);
        assert_eq!(caps("dall-e-2").len(), 3);
    }

    #[test]
    fn inpaint_requires_a_mask_and_edit_does_not() {
        let m = classify("gpt-image-1").unwrap();
        let edit = m
            .spec_for(&CapabilityId::parse("image.edit").unwrap())
            .unwrap();
        let inpaint = m
            .spec_for(&CapabilityId::parse("image.inpaint").unwrap())
            .unwrap();
        assert!(!edit.requires.contains(&InputRole::Mask));
        assert!(inpaint.requires.contains(&InputRole::Mask));
        assert_eq!(edit.accepts.get(&InputRole::Reference), Some(&15));
    }

    #[test]
    fn non_chat_models_are_dropped() {
        for id in [
            "text-embedding-3-small",
            "whisper-1",
            "tts-1-hd",
            "omni-moderation-latest",
            "gpt-4o-realtime-preview",
            "davinci-002",
        ] {
            assert!(classify(id).is_none(), "{id}");
        }
    }

    #[test]
    fn unusable_ids_are_dropped_not_panicked_on() {
        assert!(classify("").is_none());
        assert!(classify("has space").is_none());
    }

    #[test]
    fn response_format_is_for_original_dall_e_only() {
        assert!(needs_response_format("dall-e-3"));
        assert!(!needs_response_format("gpt-image-1"));
        assert!(!needs_response_format("flux-dev"));
    }
}

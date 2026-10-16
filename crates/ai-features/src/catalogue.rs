//! The built-in feature list.
//!
//! Local recipes are only listed where the source, size and checksum were checked against the
//! real file. A feature with no recipe says so, and the screen shows it as not installable yet.
//! Cloud options are only listed where an adapter exists in this workspace; none of them has been
//! run against its real service.

use crate::feature::{AppStatus, AppUse, Feature};
use crate::recipe::{CloudOption, Licence, LocalRecipe, ModelFile, NativeRoute, NodePack, Source};
use craft_ai_core::{CapabilityId, InputRole};

fn uses(list: &[(&str, &str)]) -> Vec<AppUse> {
    list.iter()
        .map(|(app, tool)| AppUse {
            app: (*app).to_owned(),
            tool: (*tool).to_owned(),
            status: AppStatus::Proposed,
        })
        .collect()
}

fn cloud(preset: &str, label: &str, note: &str) -> CloudOption {
    CloudOption {
        preset: preset.to_owned(),
        label: label.to_owned(),
        note: note.to_owned(),
    }
}

fn feature(id: &str, title: &str, summary: &str, capability: &str) -> Option<Feature> {
    Some(Feature {
        id: id.to_owned(),
        title: title.to_owned(),
        summary: summary.to_owned(),
        capability: CapabilityId::parse(capability).ok()?,
        inputs: Vec::new(),
        uses: Vec::new(),
        local: None,
        cloud: Vec::new(),
    })
}

fn sam3() -> LocalRecipe {
    LocalRecipe {
        label: "SAM 3".to_owned(),
        // PhotoCraft runs it today (Object Selection, `Sam3Session` through ComfyUI).
        usable: true,
        workflow: "sam3-segment".to_owned(),
        models: vec![ModelFile {
            folder: "sam3".to_owned(),
            filename: "sam3.safetensors".to_owned(),
            size_bytes: 3_449_860_632,
            sha256: "127037a7a11169c63a210b8e3e9caad24a66abdd65d3a78bbc3a7d8577d57026".to_owned(),
            source: Source::HuggingFace {
                repo: "apozz/sam3-safetensors".to_owned(),
                file: "sam3.safetensors".to_owned(),
                revision: Some("e88da2ed02aded4006f4711ee6a9c75d5aa38d0c".to_owned()),
            },
            licence: Licence {
                name: "Meta SAM License, community copy".to_owned(),
                url: Some("https://huggingface.co/facebook/sam3".to_owned()),
                accept_required: true,
                note: Some(
                    "This file is a community copy of Meta's SAM 3 weights and declares no licence of its own. Meta's release is gated behind the SAM License."
                        .to_owned(),
                ),
            },
        }],
        packs: vec![NodePack {
            label: "ComfyUI-SAM3".to_owned(),
            repo_url: "https://github.com/PozzettiAndrea/ComfyUI-SAM3".to_owned(),
            folder_names: vec!["comfyui-sam3".to_owned()],
        }],
        min_vram_mb: None,
        // LightCraft's Candle runtime reads this same file (it understands Meta's layout). Burn is
        // a listed slot: no app has a Burn adapter yet, so apps report it as unavailable.
        native: vec![
            NativeRoute {
                provider: "candle".to_owned(),
                model: "sam3".to_owned(),
                label: "SAM 3, built in (Nvidia or Apple graphics)".to_owned(),
            },
            NativeRoute {
                provider: "burn".to_owned(),
                model: "sam3".to_owned(),
                label: "SAM 3, built in (any graphics card)".to_owned(),
            },
        ],
    }
}

/// Every feature the launcher offers, in the order it shows them. A test pins the count and
/// every capability id, so the empty list this falls back to cannot ship.
pub fn builtin() -> Vec<Feature> {
    try_builtin().unwrap_or_default()
}

fn try_builtin() -> Option<Vec<Feature>> {
    let mut select = feature(
        "select-subject",
        "Select subject",
        "Pick out the main subject, or anything you describe in words, and turn it into a selection or a mask.",
        "image.segment",
    )?;
    select.uses = uses(&[
        ("PhotoCraft", "Select Subject and Object Selection"),
        ("LightCraft", "Subject, sky and people masks"),
        ("EffectCraft", "Roto and matte"),
    ]);
    select.inputs = vec![InputRole::Source, InputRole::Prompt];
    select.local = Some(sam3());

    let mut remove = feature(
        "remove-object",
        "Remove objects",
        "Mark something and the area fills in to match its surroundings.",
        "image.inpaint",
    )?;
    remove.inputs = vec![InputRole::Source, InputRole::Mask];
    remove.uses = uses(&[
        ("PhotoCraft", "Remove tool"),
        ("LightCraft", "Generative remove"),
    ]);

    let mut fill = feature(
        "generative-fill",
        "Generative fill",
        "Select an area, describe what should be there, and get new content that blends in.",
        "image.inpaint",
    )?;
    fill.inputs = vec![InputRole::Source, InputRole::Mask, InputRole::Prompt];
    fill.uses = uses(&[("PhotoCraft", "Generative Fill")]);
    fill.cloud = vec![cloud(
        "openai",
        "OpenAI image editing",
        "Your own OpenAI key, billed by OpenAI. Built and tested offline only.",
    )];

    let mut expand = feature(
        "generative-expand",
        "Generative expand",
        "Make the canvas bigger and fill the new area to match the picture.",
        "image.outpaint",
    )?;
    expand.inputs = vec![InputRole::Source, InputRole::Prompt];
    expand.uses = uses(&[("PhotoCraft", "Generative Expand")]);

    let mut extend = feature(
        "extend-sequence",
        "Extend sequence",
        "Add frames to the end of a clip so the shot carries on.",
        "video.generate",
    )?;
    extend.inputs = vec![InputRole::Source, InputRole::Prompt];
    extend.uses = uses(&[
        ("FilmCraft", "Generative video"),
        ("EffectCraft", "Generative video"),
    ]);

    let mut image = feature(
        "generate-image",
        "Generate images",
        "Make a picture from a description.",
        "image.generate",
    )?;
    image.inputs = vec![InputRole::Prompt];
    image.uses = uses(&[
        ("PhotoCraft", "Generate Image"),
        ("DesignCraft", "Image generation"),
        ("DeckCraft", "Slide images"),
    ]);
    image.cloud = vec![
        cloud("openai", "OpenAI", "Your own OpenAI key, billed by OpenAI."),
        cloud(
            "bfl",
            "Black Forest Labs FLUX",
            "Your own BFL key, billed by Black Forest Labs.",
        ),
        cloud(
            "vertex-express",
            "Google Gemini image (Nano Banana)",
            "Express-mode API key only. The key cannot be tested without a paid request.",
        ),
        cloud(
            "artcraft-omni",
            "ArtCraft",
            "Staff-enabled API key. Whether a subscription covers it is unverified.",
        ),
    ];

    let mut video = feature(
        "generate-video",
        "Generate video",
        "Make a clip from a description.",
        "video.generate",
    )?;
    video.inputs = vec![InputRole::Prompt];
    video.uses = uses(&[
        ("FilmCraft", "Generative video"),
        ("EffectCraft", "Generative video"),
    ]);
    video.cloud = vec![
        cloud(
            "bfl",
            "Black Forest Labs",
            "Your own BFL key, billed by Black Forest Labs.",
        ),
        cloud(
            "seedance",
            "Seedance (BytePlus ModelArk)",
            "Text to video with your BytePlus ModelArk key.",
        ),
        cloud(
            "artcraft-omni",
            "ArtCraft",
            "Staff-enabled API key. Whether a subscription covers it is unverified.",
        ),
    ];

    Some(vec![select, remove, fill, expand, extend, image, video])
}

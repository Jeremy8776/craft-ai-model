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

/// Meta's SAM 3 checkpoint in the Hugging Face layout, at a pinned revision, with the tokenizer
/// files the text prompt needs. All three sit in one folder, which is how the loader finds them.
fn sam3_transformers_files() -> Vec<ModelFile> {
    const REVISION: &str = "3c879f39826c281e95690f02c7821c4de09afae7";
    let file = |name: &str, size_bytes: u64, sha256: &str| {
        ModelFile {
        folder: "sam3_transformers".to_owned(),
        filename: name.to_owned(),
        size_bytes,
        sha256: sha256.to_owned(),
        source: Source::HuggingFace {
            repo: "facebook/sam3".to_owned(),
            file: name.to_owned(),
            revision: Some(REVISION.to_owned()),
        },
        licence: Licence {
            name: "Meta SAM License".to_owned(),
            url: Some("https://huggingface.co/facebook/sam3".to_owned()),
            accept_required: true,
            note: Some(
                "Meta's release is gated: downloading needs a Hugging Face account that has been granted access."
                    .to_owned(),
            ),
        },
    }
    };
    vec![
        file(
            "model.safetensors",
            3_439_938_512,
            "6d06f0a5f84e435071fe6603e61d0b4cc7b40e0d39d487cfd4d67d8cc11cc14a",
        ),
        file(
            "vocab.json",
            862_328,
            "5047b556ce86ccaf6aa22b3ffccfc52d391ea4accdab9c2f2407da5b742d4363",
        ),
        file(
            "merges.txt",
            524_619,
            "9fd691f7c8039210e0fced15865466c65820d09b63988b0174bfe25de299051a",
        ),
    ]
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
        // The built-in runtimes are ports of Hugging Face's implementation and read that
        // checkpoint layout, which is a different file from the ComfyUI one above (different
        // tensor names, plus the tokenizer files). Burn is a listed slot with no code yet, so
        // apps report it as unavailable.
        native: vec![
            NativeRoute {
                provider: "candle".to_owned(),
                model: "sam3".to_owned(),
                label: "SAM 3, built in (Apple graphics, otherwise the processor)".to_owned(),
                models: sam3_transformers_files(),
            },
            NativeRoute {
                provider: "burn".to_owned(),
                model: "sam3".to_owned(),
                // No Burn code exists yet, so no files are named for it: a route that cannot run must not
                // offer a 3.4 GB download.
                label: "SAM 3, built in (any graphics card)".to_owned(),
                models: Vec::new(),
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

    let mut background = feature(
        "remove-background",
        "Remove background",
        "Cut the subject out and leave the background transparent.",
        "image.matte",
    )?;
    background.inputs = vec![InputRole::Source];
    background.uses = uses(&[("PhotoCraft", "Remove Background")]);
    // No local recipe and no hosted route yet: no adapter here matts an image, and no model file
    // has been checked against its real source. The screen says so instead of offering a guess.

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

    Some(vec![
        select, background, remove, fill, expand, extend, image, video,
    ])
}

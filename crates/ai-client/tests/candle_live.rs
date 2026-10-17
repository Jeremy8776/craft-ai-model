//! The whole chain with the real model: the catalogue's route, the shared store, the cargo feature,
//! the engine, Meta's checkpoint. An app writes none of it.
//!
//! Skips unless `CRAFT_SAM3_WEIGHTS` names `model.safetensors` (beside `vocab.json` and
//! `merges.txt`). Run: `CRAFT_SAM3_WEIGHTS=<path> cargo test -p craft-ai-client --features candle
//! --release --test candle_live`.
#![cfg(feature = "candle")]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use craft_ai_client::{AiClient, ToolDesk};
use craft_ai_core::{
    AppId, ConsentLedger, Event, InputRole, JobState, Payload, Request, StoreFile,
};
use craft_ai_native::secrets::MemoryStore;
use craft_ai_native::shared::SharedStore;

fn picture() -> Vec<u8> {
    let (w, h) = (192u32, 128u32);
    let mut rgb = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let inside = (x as i32 - 96).pow(2) + (y as i32 - 64).pow(2) < 40 * 40;
            let v = if inside { 230 } else { 30 };
            rgb.extend_from_slice(&[v, v, v]);
        }
    }
    let mut out = Vec::new();
    let mut e = png::Encoder::new(&mut out, w, h);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    let mut writer = e.write_header().unwrap();
    writer.write_image_data(&rgb).unwrap();
    writer.finish().unwrap();
    out
}

#[test]
fn an_app_gets_a_mask_from_the_real_model_by_asking_for_select_subject() {
    let Some(weights) = std::env::var_os("CRAFT_SAM3_WEIGHTS").map(PathBuf::from) else {
        eprintln!("skipped: set CRAFT_SAM3_WEIGHTS to run the real model");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    shared.replace(&StoreFile::default()).unwrap();
    let client = |app: &str| {
        AiClient::new(
            AppId::new(app).unwrap(),
            shared.clone(),
            Arc::new(MemoryStore::new()),
        )
    };
    let feature = craft_ai_features::builtin()
        .into_iter()
        .find(|f| f.id == "select-subject")
        .unwrap();
    let role = feature.role().unwrap();

    // The launcher's part: record the route once the files are in place.
    ToolDesk::new(client("launcher"), shared.clone(), None)
        .enable_native_with(&feature, "candle", &weights)
        .unwrap();

    // An app's part: ask for the capability.
    let app = client("lightcraft");
    let request = Request::new(feature.capability.clone())
        .with_input(
            InputRole::Source,
            Payload::inline("image/png", picture()).unwrap(),
        )
        .with_input(InputRole::Prompt, Payload::text("circle"));
    let plan = app.plan(&role, request).unwrap();
    let done = app
        .start(&plan, &ConsentLedger::default())
        .unwrap()
        .wait(Duration::from_millis(100), Duration::from_secs(300))
        .unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    let masks: Vec<_> = done
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Output(o) => Some(o),
            _ => None,
        })
        .collect();
    assert_eq!(masks.len(), 1);
    let Payload::Inline { media_type, data } = &masks[0].payload else {
        panic!("expected the mask itself");
    };
    assert_eq!(media_type.as_str(), "image/png");
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let reader = decoder.read_info().unwrap();
    assert_eq!((reader.info().width, reader.info().height), (192, 128));
    // What the model picks out of a drawn disc is its own business; the content was checked on
    // real photographs (see the engine crate's `segment` example). Here the chain is the subject.
    assert!(masks[0].label.as_deref().is_some_and(|l| !l.is_empty()));
}

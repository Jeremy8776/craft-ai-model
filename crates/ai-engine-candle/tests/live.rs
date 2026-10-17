//! The real model, through the real provider and the shared runner checks.
//!
//! Needs Meta's checkpoint, so it skips (and says so) unless `CRAFT_SAM3_WEIGHTS` names the
//! `model.safetensors` that sits beside `vocab.json` and `merges.txt`. Run it in release mode:
//! `CRAFT_SAM3_WEIGHTS=<path> cargo test -p craft-ai-engine-candle --release --test live`.

use std::path::PathBuf;
use std::sync::Arc;

use craft_ai_core::{CapabilityId, ErrorKind, InputRole, Payload, Request};
use std::time::Duration;

use craft_ai_engine::testing::check_runner_paced;
use craft_ai_engine::{ModelRunner, RunContext};
use craft_ai_engine_candle::Sam3Runner;

/// A small picture with a bright disc on a dark ground.
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
fn the_real_model_passes_the_shared_runner_checks() {
    let Some(weights) = std::env::var_os("CRAFT_SAM3_WEIGHTS").map(PathBuf::from) else {
        eprintln!("skipped: set CRAFT_SAM3_WEIGHTS to run the real model");
        return;
    };
    let capability = CapabilityId::parse("image.segment").unwrap();
    let source = Payload::inline("image/png", picture()).unwrap();
    let good = Request::new(capability.clone())
        .with_input(InputRole::Source, source.clone())
        .with_input(InputRole::Prompt, Payload::text("circle"));
    // Words and no picture: refused when the job is submitted, before any work.
    let bad = Request::new(capability).with_input(InputRole::Prompt, Payload::text("circle"));
    // A real job takes about half a minute on the processor; allow it 100 s of polls.
    check_runner_paced(
        Arc::new(Sam3Runner),
        &weights,
        &good,
        &bad,
        ErrorKind::InvalidRequest,
        Duration::from_millis(100),
    )
    .unwrap();
}

/// A phone photo taken with the phone on its side: the pixels are stored 128 wide and 192 high and
/// the EXIF block says "turn 90 degrees clockwise". Upright it is the 192 by 128 picture above,
/// with the disc at (143, 64). The mask must come back upright, over the disc.
#[test]
fn a_sideways_jpeg_gets_an_upright_mask_over_the_object() {
    let Some(weights) = std::env::var_os("CRAFT_SAM3_WEIGHTS").map(PathBuf::from) else {
        eprintln!("skipped: set CRAFT_SAM3_WEIGHTS to run the real model");
        return;
    };
    let (w, h) = (128u16, 192u16);
    let mut rgb = Vec::new();
    for y in 0..i32::from(h) {
        for x in 0..i32::from(w) {
            let inside = (x - 64).pow(2) + (y - 48).pow(2) < 40 * 40;
            let v = if inside { 230 } else { 30 };
            rgb.extend_from_slice(&[v, v, v]);
        }
    }
    let mut jpeg = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut jpeg, 95);
    let mut exif = b"Exif\0\0".to_vec();
    exif.extend_from_slice(&[
        0x49, 0x49, 0x2A, 0, 8, 0, 0, 0, 1, 0, 0x12, 1, 3, 0, 1, 0, 0, 0,
    ]);
    exif.extend_from_slice(&[6, 0, 0, 0, 0, 0, 0, 0]);
    encoder.add_app_segment(1, &exif).unwrap();
    encoder
        .encode(&rgb, w, h, jpeg_encoder::ColorType::Rgb)
        .unwrap();

    let capability = CapabilityId::parse("image.segment").unwrap();
    let request = Request::new(capability)
        .with_input(
            InputRole::Source,
            Payload::inline("image/jpeg", jpeg).unwrap(),
        )
        .with_param(
            "points",
            serde_json::json!([{"x": 143.0 / 192.0, "y": 0.5, "positive": true}]),
        );
    let mut model = Sam3Runner.load(&weights).unwrap();
    let outputs = model.run(&request, &RunContext::detached()).unwrap();
    let Payload::Inline { data, .. } = &outputs[0].payload else {
        panic!("the mask should come back inline");
    };
    let mut reader = png::Decoder::new(std::io::Cursor::new(data))
        .read_info()
        .unwrap();
    let mut buffer = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buffer).unwrap();
    assert_eq!(
        (frame.width, frame.height),
        (192, 128),
        "the mask is upright"
    );
    let (mut sum_x, mut sum_y, mut count) = (0.0, 0.0, 0.0);
    for (i, v) in buffer[..frame.buffer_size()].iter().enumerate() {
        if *v > 127 {
            sum_x += (i % 192) as f64;
            sum_y += (i / 192) as f64;
            count += 1.0;
        }
    }
    assert!(count > 500.0, "the disc was not selected ({count} pixels)");
    let (cx, cy) = (sum_x / count, sum_y / count);
    assert!(
        (cx - 143.0).abs() < 15.0 && (cy - 64.0).abs() < 15.0,
        "centre ({cx}, {cy})"
    );
}

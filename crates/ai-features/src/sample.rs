//! Small requests a person can use to check that a chosen model really does a tool's job.
//!
//! Each is as cheap as the job allows: a few words and, where a picture is needed, a 64 by 64
//! one made here. A hosted service is still billed for it; the screen says so before sending.

use crate::feature::Feature;
use crate::png;
use craft_ai_core::{InputRole, Payload, Request};

/// A white square with a mid-grey disc, so "the disc" means something to a segmenter.
fn picture() -> Vec<u8> {
    png::grey(64, 64, |x, y| {
        let (dx, dy) = (i64::from(x) - 32, i64::from(y) - 32);
        if dx * dx + dy * dy <= 14 * 14 {
            90
        } else {
            255
        }
    })
}

/// White everywhere except a black square in the middle: the area to fill.
fn mask() -> Vec<u8> {
    png::grey(64, 64, |x, y| {
        if (20..44).contains(&x) && (20..44).contains(&y) {
            0
        } else {
            255
        }
    })
}

fn image(bytes: Vec<u8>) -> Option<Payload> {
    Payload::inline("image/png", bytes).ok()
}

/// The sample request for `feature`, or `None` where no test has been worked out.
pub fn sample_request(feature: &Feature) -> Option<Request> {
    let base = Request::new(feature.capability.clone());
    match feature.id.as_str() {
        "select-subject" => Some(
            base.with_input(InputRole::Source, image(picture())?)
                .with_input(InputRole::Prompt, Payload::text("the grey disc")),
        ),
        "generate-image" => Some(base.with_input(
            InputRole::Prompt,
            Payload::text("A plain red circle on a white background"),
        )),
        "generative-fill" | "remove-object" => {
            let mut request = base
                .with_input(InputRole::Source, image(picture())?)
                .with_input(InputRole::Mask, image(mask())?);
            if feature.id == "generative-fill" {
                request =
                    request.with_input(InputRole::Prompt, Payload::text("a small blue square"));
            }
            Some(request)
        }
        "generate-video" => Some(base.with_input(
            InputRole::Prompt,
            Payload::text("A red ball rolling slowly across a white floor"),
        )),
        _ => None,
    }
}

/// Whether a sample costs more than a few pennies at hosted prices, so the screen can say so.
pub fn sample_is_costly(feature: &Feature) -> bool {
    feature.capability.as_str().starts_with("video.")
}

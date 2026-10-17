//! Runs the built-in SAM 3 on one picture, the way an app would through the engine contract.
//!
//! ```text
//! cargo run --release -p craft-ai-engine-candle --example segment -- \
//!     <folder>/model.safetensors picture.jpg mask.png "the sky"
//! cargo run --release -p craft-ai-engine-candle --example segment -- \
//!     <folder>/model.safetensors picture.png mask.png @0.5,0.4
//! ```
//!
//! The last argument is a description, or `@x,y` for one positive click (0 to 1 across and down).

use std::sync::Arc;
use std::time::{Duration, Instant};

use craft_ai_core::{CapabilityId, Event, InputRole, JobState, Payload, Provider, Request};
use craft_ai_engine::{EngineProvider, ModelRunner};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [weights, picture, out, what] = args.as_slice() else {
        return Err(
            "usage: segment <model.safetensors> <picture.png|jpg> <mask.png> <words | @x,y>".into(),
        );
    };
    let runner: Arc<dyn ModelRunner> = Arc::new(craft_ai_engine_candle::Sam3Runner);
    let model = runner.model().id;
    let provider = EngineProvider::new("candle", [(runner, weights.into())]);

    let mut request = Request::new(CapabilityId::parse("image.segment")?).with_input(
        InputRole::Source,
        // The engine tells PNG from JPEG by the file's first bytes; the type is only a label.
        Payload::inline("image/png", std::fs::read(picture)?)?,
    );
    request = match what.strip_prefix('@') {
        Some(xy) => {
            let (x, y) = xy.split_once(',').ok_or("use @x,y")?;
            request.with_param(
                "points",
                json!([{"x": x.parse::<f64>()?, "y": y.parse::<f64>()?, "positive": true}]),
            )
        }
        None => request.with_input(InputRole::Prompt, Payload::text(what.clone())),
    };

    let start = Instant::now();
    let job = provider.submit(&model, &request)?;
    loop {
        let snap = provider.poll(&job)?;
        for event in snap.events {
            match event {
                Event::Notice { message } => eprintln!("[{:?}] {message}", start.elapsed()),
                Event::Output(o) => {
                    if let Payload::Inline { data, .. } = o.payload {
                        std::fs::write(out, data)?;
                        eprintln!(
                            "[{:?}] wrote {out} ({})",
                            start.elapsed(),
                            o.label.unwrap_or_default()
                        );
                    }
                }
                _ => {}
            }
        }
        match snap.state {
            JobState::Succeeded => return Ok(()),
            JobState::Failed { error } => return Err(error.message.into()),
            JobState::Cancelled => return Err("cancelled".into()),
            _ => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

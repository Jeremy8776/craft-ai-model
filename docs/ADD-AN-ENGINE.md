# Add a model to a built-in engine

A built-in engine (Candle, Burn) runs a model from a weights file on this computer. You write the
model. Job numbers, threads, polling, Cancel, progress, validation, loading the weights once,
and turning a crash into a failed job are already done in `EngineProvider`. No app changes when
you add a model, and no app changes when you add an engine.

This page is compiled as part of the crate's tests, so its code works as written.

## 1. Write the runner

A `ModelRunner` says which engine and model it is, what the model accepts, and how to load the
weights. What it loads is a `LoadedModel` that runs one request.

```rust
use std::path::Path;
use craft_ai_core::{
    AiError, CapabilityId, CapabilitySpec, InputRole, ModelId, ModelInfo, Output, Payload, Request,
};
use craft_ai_engine::{LoadedModel, ModelRunner, RunContext};

/// A toy model: answers with its prompt in capitals.
struct Shout;

impl ModelRunner for Shout {
    fn engine(&self) -> &str { "toy" }

    fn model(&self) -> ModelInfo {
        let capability = CapabilityId::parse("text.complete").unwrap();
        ModelInfo::new(ModelId::new("shout").unwrap(), "Shout").with(
            CapabilitySpec::new(capability)
                .accepting(InputRole::Prompt, 1)
                .requiring(InputRole::Prompt),
        )
    }

    fn load(&self, weights: &Path) -> Result<Box<dyn LoadedModel>, AiError> {
        // Read the file here. This is the slow part and happens once.
        std::fs::metadata(weights).map_err(|_| AiError::unsupported("That file is not readable."))?;
        Ok(Box::new(ShoutLoaded))
    }
}

struct ShoutLoaded;

impl LoadedModel for ShoutLoaded {
    fn run(&mut self, request: &Request, ctx: &RunContext) -> Result<Vec<Output>, AiError> {
        ctx.check_cancelled()?;   // between steps, so Cancel is quick
        ctx.progress(50);         // 0 to 100, any order, never goes backwards
        let text = match request.inputs.first().map(|i| &i.payload) {
            Some(Payload::Text { text }) => text.to_uppercase(),
            _ => return Err(AiError::invalid("A prompt is needed.")),
        };
        Ok(vec![Output { payload: Payload::text(text), label: None }])
    }
}
```

The request has already been checked against the spec in `model()`, so `run` only decodes.
Return errors in words a person can act on: they reach the screen.

## 2. Prove it with the shared checks

`check_runner` puts the runner behind the real provider and runs the whole shared provider
conformance suite: a job's full life, Cancel, a refused request, an unknown job, missing
weights, a second job after the first and after a Cancel. Use it in your engine crate's tests.

```rust
# use std::path::Path;
# use craft_ai_core::{AiError, CapabilityId, CapabilitySpec, ErrorKind, InputRole, ModelId, ModelInfo, Output, Payload, Request};
# use craft_ai_engine::{LoadedModel, ModelRunner, RunContext};
# struct Shout;
# impl ModelRunner for Shout {
#     fn engine(&self) -> &str { "toy" }
#     fn model(&self) -> ModelInfo {
#         let c = CapabilityId::parse("text.complete").unwrap();
#         ModelInfo::new(ModelId::new("shout").unwrap(), "Shout")
#             .with(CapabilitySpec::new(c).accepting(InputRole::Prompt, 1).requiring(InputRole::Prompt))
#     }
#     fn load(&self, _: &Path) -> Result<Box<dyn LoadedModel>, AiError> { Ok(Box::new(Loaded)) }
# }
# struct Loaded;
# impl LoadedModel for Loaded {
#     fn run(&mut self, _: &Request, _: &RunContext) -> Result<Vec<Output>, AiError> {
#         Ok(vec![Output { payload: Payload::text("HI"), label: None }])
#     }
# }
use std::sync::Arc;
use craft_ai_engine::testing::check_runner;

let dir = tempfile::tempdir().unwrap();
let weights = dir.path().join("shout.safetensors");
std::fs::write(&weights, b"weights").unwrap();

let capability = CapabilityId::parse("text.complete").unwrap();
let good = Request::new(capability.clone()).with_input(InputRole::Prompt, Payload::text("hi"));
let bad = Request::new(capability); // no prompt

check_runner(Arc::new(Shout), &weights, &good, &bad, ErrorKind::InvalidRequest).unwrap();
```

## 3. Offer it

An engine crate exposes its runners in one function:

```rust,ignore
pub fn runners() -> Vec<std::sync::Arc<dyn craft_ai_engine::ModelRunner>> {
    vec![std::sync::Arc::new(Shout)]
}
```

An engine that ships with this repository also gets a cargo feature on `craft-ai-client`
(`candle` today). An app turns the feature on and `ProviderFactory::standard()` knows the engine:
nothing else. For any other engine, the app (or the launcher) puts the runners in a registry and
gives the client the registry:

```rust,ignore
let mut registry = EngineRegistry::new();
registry.extend(my_engine::runners());
client = client.with_factory({ let mut f = ProviderFactory::standard(); f.register_engines(registry); f });
```

`crates/ai-client/tests/engines.rs` runs this path end to end with a fake runner, and
`crates/ai-client/tests/candle_live.rs` does it with the real SAM 3 (skipped without the
checkpoint): the tool
enabled from a weights file, the client planning and starting a job, the output coming back.

For the launcher to offer the model, add a `NativeRoute` (engine name and model id, matching
`engine()` and `model().id` above) to the tool's recipe in `crates/ai-features/src/catalogue.rs`.
Nothing else in any app changes.

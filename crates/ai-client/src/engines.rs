//! Turning a registry of built-in model runners into adapters the client can build.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use craft_ai_core::{AiError, Connection, Provider};
use craft_ai_engine::{EngineProvider, EngineRegistry};

use crate::factory::ProviderFactory;

impl ProviderFactory {
    /// Makes every engine in `registry` available: a saved connection whose provider is one of
    /// the registry's engines (`candle`, `burn`) is built as an [`EngineProvider`] that runs its
    /// model from the weights file recorded on the connection. The accelerators each engine
    /// declares are recorded too, so engine advice needs nothing stated by the app.
    ///
    /// This is the whole of enabling a built-in engine in an app.
    pub fn register_engines(&mut self, registry: EngineRegistry) {
        let registry = Arc::new(registry);
        for engine in registry.engines() {
            let mut vendors = Vec::new();
            for runner in registry.runners_of(&engine) {
                for vendor in runner.accelerators() {
                    if !vendors.contains(vendor) {
                        vendors.push(*vendor);
                    }
                }
            }
            let registry = registry.clone();
            let kept = Kept::default();
            self.register(&engine, move |connection, _| {
                kept.provider_for(&registry, connection)
            });
            // After registering: registering a name forgets any earlier declaration.
            self.declare_accelerators(&engine, &vendors);
        }
    }
}

/// The provider last built for each connection of one engine.
///
/// The client asks the factory for a provider on every job. A model is slow to load and a runner
/// may keep work between jobs (SAM 3 keeps the picture it has looked at), so handing back a new
/// provider each time would load the weights again for every click, and two jobs at once would
/// load two copies. One provider per connection is kept instead, and replaced when the connection
/// names different models or weight files.
#[derive(Default)]
struct Kept(Mutex<HashMap<String, (String, Arc<EngineProvider>)>>);

impl Kept {
    fn provider_for(
        &self,
        registry: &EngineRegistry,
        connection: &Connection,
    ) -> Result<Arc<dyn Provider>, AiError> {
        // What the provider was built from: change any of it and the old one no longer applies.
        let mut shape = connection.provider.to_ascii_lowercase();
        for model in &connection.models {
            shape.push_str(&format!(
                "\n{}\t{}",
                model.id,
                model.local_file.as_deref().unwrap_or_default()
            ));
        }
        let mut kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let id = connection.id.to_string();
        if let Some((same, provider)) = kept.get(&id)
            && *same == shape
        {
            return Ok(provider.clone());
        }
        let provider = Arc::new(EngineProvider::from_connection(registry, connection)?);
        kept.insert(id, (shape, provider.clone()));
        Ok(provider)
    }
}

/// The engines compiled into this build, by cargo feature. Empty when none is turned on.
pub(crate) fn built_in() -> EngineRegistry {
    #[allow(unused_mut)]
    let mut registry = EngineRegistry::new();
    #[cfg(feature = "candle")]
    registry.extend(craft_ai_engine_candle::runners());
    registry
}

//! Turning a saved [`Connection`] into a working [`Provider`].
//!
//! The launcher stores a connection with a `provider` name (`openai`, `ollama`, ...). The factory
//! maps that name to code. The built-in names all use the OpenAI-compatible adapter; an app or a
//! test can [`register`](ProviderFactory::register) more, which is how new adapters (fal,
//! ComfyUI, the ArtCraft API) plug in without the client changing.
//!
//! The key arrives here already fetched and is handed straight to the adapter. It is not stored in
//! the factory.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use craft_ai_artcraft::{ArtCraftConfig, ArtCraftProvider};
use craft_ai_bfl::{BflConfig, BflProvider};
use craft_ai_comfyui::{ComfyConfig, ComfyProvider, WorkflowLibrary};
use craft_ai_core::{AiError, Connection, Locality, Provider, SecretString};
use craft_ai_features::GpuVendor;
use craft_ai_media::{MediaStore, MediaStoreConfig, PersistingProvider};
use craft_ai_openai::{OpenAiConfig, OpenAiProvider};
use craft_ai_sam3::{Sam3Config, Sam3Provider};
use craft_ai_seedance::{SeedanceConfig, SeedanceProvider};
use craft_ai_vertex::{VertexConfig, VertexProvider};

/// Builds a provider for one connection.
pub type BuildFn =
    dyn Fn(&Connection, Option<SecretString>) -> Result<Arc<dyn Provider>, AiError> + Send + Sync;

/// Provider names that speak the OpenAI REST shape.
const OPENAI_FAMILY: &[&str] = &[
    "openai",
    "openai-compatible",
    "ollama",
    "lmstudio",
    "llamacpp",
    "vllm",
    "openrouter",
];

/// Maps provider names to adapters.
#[derive(Clone)]
pub struct ProviderFactory {
    builders: HashMap<String, Arc<BuildFn>>,
    /// The card makers an app states its build of an engine can use (see `declare_accelerators`).
    accelerators: HashMap<String, Vec<GpuVendor>>,
    /// Where the ComfyUI builder reads workflow files; the shared default when `None`.
    comfyui_library: Option<std::path::PathBuf>,
    /// Where the settings files for data-described services are read; the shared default when `None`.
    declarative_specs: Option<std::path::PathBuf>,
    /// App-owned directory for durable ComfyUI outputs. Opt-in; no implicit shared output path.
    comfyui_output_dir: Option<std::path::PathBuf>,
    /// Opt-in: copy hosted result links into an app-owned directory as jobs finish.
    media: Option<MediaStoreConfig>,
    /// Passed to every adapter: short for probing, long for real jobs.
    timeouts: Option<(Duration, Duration)>,
}

impl ProviderFactory {
    /// Only what is built in, with the ComfyUI adapter reading the shared default workflow
    /// library beside the connections store.
    pub fn standard() -> Self {
        Self::built(None, None)
    }

    /// Like [`standard`](Self::standard) but with short timeouts (5 s to connect, 20 s in all), for
    /// the "Test" button: a server that does not answer must not leave the screen waiting for the
    /// ten minutes a real image job is allowed.
    pub fn probing() -> Self {
        Self::built(
            Some((Duration::from_secs(5), Duration::from_secs(20))),
            None,
        )
    }

    /// [`standard`](Self::standard) pointed at a specific ComfyUI workflow library.
    pub fn with_comfyui_library(dir: impl Into<std::path::PathBuf>) -> Self {
        Self::built(None, Some(dir.into()))
    }

    fn built(
        timeouts: Option<(Duration, Duration)>,
        comfyui_library: Option<std::path::PathBuf>,
    ) -> Self {
        let mut f = Self {
            builders: HashMap::new(),
            accelerators: HashMap::new(),
            comfyui_library,
            declarative_specs: None,
            comfyui_output_dir: None,
            media: None,
            timeouts,
        };
        for name in OPENAI_FAMILY {
            f.register(name, move |c, s| build_openai(c, s, timeouts));
        }
        f.register("artcraft-omni", move |c, s| {
            let base = c
                .endpoint
                .as_deref()
                .ok_or_else(|| AiError::invalid("ArtCraft Omni API address is required"))?;
            let key = s.ok_or_else(|| {
                AiError::new(
                    craft_ai_core::ErrorKind::Auth,
                    "ArtCraft Omni API key is required",
                )
            })?;
            let mut cfg = ArtCraftConfig::new(base, key);
            if let Some((connect, request)) = timeouts {
                cfg.connect_timeout = connect;
                cfg.request_timeout = request;
            }
            Ok(Arc::new(ArtCraftProvider::new(cfg)?))
        });
        f.register("bfl", move |c, s| {
            let base = c
                .endpoint
                .as_deref()
                .ok_or_else(|| AiError::invalid("BFL address is required"))?;
            let key = s.ok_or_else(|| {
                AiError::new(craft_ai_core::ErrorKind::Auth, "BFL API key is required")
            })?;
            let mut cfg = BflConfig::new(base, key);
            if let Some((connect, request)) = timeouts {
                cfg.connect_timeout = connect;
                cfg.request_timeout = request;
            }
            Ok(Arc::new(BflProvider::new(cfg)?))
        });
        f.register("seedance", move |c, s| {
            let base = c
                .endpoint
                .as_deref()
                .ok_or_else(|| AiError::invalid("ModelArk address is required"))?;
            let key = s.ok_or_else(|| {
                AiError::new(
                    craft_ai_core::ErrorKind::Auth,
                    "ModelArk API key is required",
                )
            })?;
            let mut cfg = SeedanceConfig::new(base, key);
            if let Some((connect, request)) = timeouts {
                cfg.connect_timeout = connect;
                cfg.request_timeout = request;
            }
            Ok(Arc::new(SeedanceProvider::new(cfg)?))
        });
        f.register("vertex-express", move |c, s| {
            let base = c
                .endpoint
                .as_deref()
                .ok_or_else(|| AiError::invalid("Vertex address is required"))?;
            let key = s.ok_or_else(|| {
                AiError::new(
                    craft_ai_core::ErrorKind::Auth,
                    "Vertex express API key is required",
                )
            })?;
            let mut cfg = VertexConfig::new(base, key);
            if let Some((connect, request)) = timeouts {
                cfg.connect_timeout = connect;
                cfg.request_timeout = request;
            }
            Ok(Arc::new(VertexProvider::new(cfg)?))
        });
        f.register("sam3", move |c, _| {
            let base = c
                .endpoint
                .as_deref()
                .ok_or_else(|| AiError::invalid("SAM 3 bridge address is required"))?;
            let mut cfg = Sam3Config::new(base);
            if let Some((connect, request)) = timeouts {
                cfg.connect_timeout = connect;
                cfg.request_timeout = request;
            }
            Ok(Arc::new(Sam3Provider::new(cfg)?))
        });
        let comfyui_library = f.comfyui_library.clone();
        let comfy_timeouts = f.timeouts;
        f.register_comfyui(
            comfyui_library,
            f.comfyui_output_dir.clone(),
            comfy_timeouts,
        );
        f.register_declarative();
        f.register_engines(crate::engines::built_in());
        f
    }

    /// Adds or replaces the adapter for a provider name. Replacing one forgets what
    /// [`declare_accelerators`](Self::declare_accelerators) said about the old one, so declare
    /// after registering.
    pub fn register(
        &mut self,
        name: &str,
        build: impl Fn(&Connection, Option<SecretString>) -> Result<Arc<dyn Provider>, AiError>
        + Send
        + Sync
        + 'static,
    ) {
        let name = name.to_ascii_lowercase();
        // A different build of the adapter is not the one whose cards were declared.
        self.accelerators.remove(&name);
        self.builders.insert(name, Arc::new(build));
    }

    /// Removes the adapter for a provider name, and what was declared for it. An app uses this to
    /// turn off something its build would otherwise offer (a built-in engine, for example); the
    /// name is then reported as one this app cannot run.
    pub fn unregister(&mut self, name: &str) {
        let name = name.to_ascii_lowercase();
        self.builders.remove(&name);
        self.accelerators.remove(&name);
    }

    /// Points data-described services at a specific folder of settings files (one file per
    /// connection, named after the connection). Without this the shared default beside the
    /// connections store is used, so every app reads the same files.
    fn register_declarative(&mut self) {
        let dir = self.declarative_specs.clone();
        let timeouts = self.timeouts;
        self.register("declarative", move |c, s| {
            crate::declarative::build(c, s, dir.clone(), timeouts)
        });
    }

    pub fn set_declarative_specs(&mut self, dir: impl Into<std::path::PathBuf>) {
        self.declarative_specs = Some(dir.into());
        self.register_declarative();
    }

    /// Points the ComfyUI adapter at a specific workflow library. Without this it reads the
    /// shared default beside the connections store, so every app sees the same workflows.
    pub fn set_comfyui_library(&mut self, dir: impl Into<std::path::PathBuf>) {
        self.comfyui_library = Some(dir.into());
        self.register_comfyui(
            self.comfyui_library.clone(),
            self.comfyui_output_dir.clone(),
            self.timeouts,
        );
    }

    /// Opt in to streamed, bounded local media files instead of inline output bytes.
    /// The app owns the directory and must clean up assets it no longer uses.
    pub fn with_comfyui_output_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.comfyui_output_dir = Some(dir.into());
        self.register_comfyui(
            self.comfyui_library.clone(),
            self.comfyui_output_dir.clone(),
            self.timeouts,
        );
        self
    }

    /// Opt in to durable local copies of hosted results. Providers such as BFL, ArtCraft and
    /// Seedance answer with a link that expires; with this set, every provider the factory builds
    /// downloads such a link into `dir` as the job finishes and returns a `file:` handle. The
    /// app owns the directory and its cleanup. A failed copy leaves the original link and adds a
    /// notice rather than failing a job the provider already did. Errors if `dir` is not an
    /// existing absolute folder.
    pub fn with_media_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Result<Self, AiError> {
        let config = MediaStoreConfig::new(dir);
        MediaStore::new(config.clone())?;
        self.media = Some(config);
        Ok(self)
    }

    fn register_comfyui(
        &mut self,
        comfyui_library: Option<std::path::PathBuf>,
        output_dir: Option<std::path::PathBuf>,
        timeouts: Option<(Duration, Duration)>,
    ) {
        self.register("comfyui", move |c, _| {
            let base = c
                .endpoint
                .as_deref()
                .ok_or_else(|| AiError::invalid("ComfyUI address is required"))?;
            let library = match &comfyui_library {
                Some(dir) => WorkflowLibrary::at(dir)?,
                None => WorkflowLibrary::at_default_location().ok_or_else(|| {
                    AiError::invalid("could not locate the workflow library directory")
                })?,
            };
            let workflows = library.load()?;
            if workflows.is_empty() {
                return Err(AiError::unsupported(
                    "the workflow library is empty; import a workflow JSON first",
                ));
            }
            let mut cfg = ComfyConfig::new(base);
            if let Some(dir) = &output_dir {
                cfg = cfg.with_output_dir(dir);
            }
            for w in workflows {
                cfg = cfg.with_workflow(w);
            }
            if let Some((connect, request)) = timeouts {
                cfg.connect_timeout = connect;
                cfg.request_timeout = request;
            }
            Ok(Arc::new(ComfyProvider::new(cfg)?))
        });
    }

    pub fn knows(&self, name: &str) -> bool {
        self.builders.contains_key(&name.to_ascii_lowercase())
    }

    /// States which card makers this app's build of the engine `name` can use, so engine advice
    /// does not assume. Say `&[]` for a build that only runs on the processor. Call it where the
    /// adapter is registered, because that is where the build's features are known.
    pub fn declare_accelerators(&mut self, name: &str, vendors: &[GpuVendor]) {
        self.accelerators
            .insert(name.to_ascii_lowercase(), vendors.to_vec());
    }

    /// What [`declare_accelerators`](Self::declare_accelerators) was told for `name`, if anything.
    pub fn accelerators(&self, name: &str) -> Option<&[GpuVendor]> {
        self.accelerators
            .get(&name.to_ascii_lowercase())
            .map(Vec::as_slice)
    }

    pub fn build(
        &self,
        connection: &Connection,
        secret: Option<SecretString>,
    ) -> Result<Arc<dyn Provider>, AiError> {
        let build = self
            .builders
            .get(&connection.provider.to_ascii_lowercase())
            .ok_or_else(|| {
                AiError::unsupported(format!(
                    "This app can't use \"{}\" yet (connection \"{}\")",
                    connection.provider, connection.label
                ))
            })?;
        let provider = build(connection, secret)?;
        match &self.media {
            Some(config) => {
                let mut config = config.clone();
                // A server the user runs on this computer may hand back a loopback link.
                config.allow_loopback = connection.locality == Locality::Local;
                let store = Arc::new(MediaStore::new(config)?);
                Ok(Arc::new(PersistingProvider::new(provider, store)))
            }
            None => Ok(provider),
        }
    }
}

impl Default for ProviderFactory {
    fn default() -> Self {
        Self::standard()
    }
}

impl std::fmt::Debug for ProviderFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names: Vec<_> = self.builders.keys().collect();
        names.sort();
        write!(f, "ProviderFactory({names:?})")
    }
}

fn build_openai(
    connection: &Connection,
    secret: Option<SecretString>,
    timeouts: Option<(Duration, Duration)>,
) -> Result<Arc<dyn Provider>, AiError> {
    let endpoint = connection.endpoint.as_deref().ok_or_else(|| {
        AiError::invalid(format!(
            "connection \"{}\" has no address to connect to",
            connection.label
        ))
    })?;
    let mut config = OpenAiConfig::new(endpoint);
    config.api_key = secret;
    config.key_header = connection.key_header.clone();
    if let Some((connect, request)) = timeouts {
        config.connect_timeout = connect;
        config.request_timeout = request;
    }
    // The models the user saw in the launcher are the ones requests are checked against.
    if !connection.models.is_empty() {
        config = config.with_models(connection.models.clone());
    }
    Ok(Arc::new(OpenAiProvider::new(config)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(provider: &str, endpoint: Option<&str>) -> Connection {
        let mut v = serde_json::json!({
            "id": "c", "label": "Test", "provider": provider,
            "locality": "local", "auth": {"kind": "none"}, "billing": "free", "models": []
        });
        if let Some(e) = endpoint {
            v["endpoint"] = serde_json::json!(e);
        }
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn standard_knows_the_openai_family_and_bfl() {
        let f = ProviderFactory::standard();
        for n in ["openai", "OLLAMA", "lmstudio", "openrouter", "bfl"] {
            assert!(f.knows(n), "{n}");
        }
        assert!(!f.knows("fal"));
    }

    #[test]
    fn artcraft_api_key_has_a_real_adapter_but_account_access_does_not() {
        let f = ProviderFactory::standard();
        assert!(f.knows("artcraft-omni"));
        assert!(!f.knows("artcraft-account"));
    }

    #[test]
    fn unknown_provider_names_the_provider_and_connection() {
        let e = ProviderFactory::standard()
            .build(&conn("fal", Some("http://127.0.0.1:1/v1")), None)
            .err()
            .unwrap();
        assert!(e.message.contains("fal") && e.message.contains("Test"));
    }

    #[test]
    fn openai_family_needs_an_address() {
        assert!(
            ProviderFactory::standard()
                .build(&conn("openai", None), None)
                .is_err()
        );
        assert!(
            ProviderFactory::standard()
                .build(&conn("ollama", Some("http://127.0.0.1:11434/v1")), None)
                .is_ok()
        );
    }

    #[test]
    fn replacing_an_adapter_forgets_its_declared_cards_and_unregister_removes_both() {
        let mut f = ProviderFactory::standard();
        f.register("engine-x", |_, _| Err(AiError::unsupported("stub")));
        f.declare_accelerators("engine-x", &[GpuVendor::Nvidia]);
        assert_eq!(f.accelerators("ENGINE-X"), Some(&[GpuVendor::Nvidia][..]));
        // Someone else's build of the same name: the old statement no longer applies.
        f.register("engine-x", |_, _| Err(AiError::unsupported("other")));
        assert_eq!(f.accelerators("engine-x"), None);
        f.declare_accelerators("engine-x", &[]);
        f.unregister("Engine-X");
        assert!(!f.knows("engine-x") && f.accelerators("engine-x").is_none());
        f.unregister("never-there"); // not an error
    }

    #[test]
    fn register_replaces_and_debug_lists_names_only() {
        let mut f = ProviderFactory::standard();
        f.register("fal", |_, _| Err(AiError::unsupported("stub")));
        assert!(f.knows("fal"));
        assert!(format!("{f:?}").contains("fal"));
    }
}

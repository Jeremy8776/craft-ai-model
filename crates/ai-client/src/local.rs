//! A tool's local model becomes something an app can read.
//!
//! The catalogue says a tool has a local recipe (SAM 3 for Select subject), and the launcher shows
//! it Ready once the files and node pack are in place. Until now nothing in the shared store
//! represented that, so no tool could be bound to it and no app could read a choice. This writes
//! the missing piece: a local connection that offers the recipe's model for the tool, and binds
//! the tool to it unless a person has already chosen something else.

use std::path::{Path, PathBuf};

use craft_ai_core::endpoint::Endpoint;
use craft_ai_core::role::ResolveError;
use craft_ai_core::{
    AiError, Auth, Billing, Binding, CapabilitySpec, Connection, ConnectionId, Locality, ModelId,
    ModelInfo, Role,
};
use craft_ai_features::Feature;

use crate::client::AiClient;
use crate::tools::ToolDesk;

/// The exact local connection and model chosen for a tool, without any key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalRoute {
    pub connection: ConnectionId,
    pub model: ModelId,
    pub provider: String,
    pub endpoint: Option<String>,
    /// The weights file of a model an app runs itself, as recorded by whoever registered it.
    pub file: Option<String>,
}

/// What [`ToolDesk::enable_local`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalEnabled {
    pub connection: ConnectionId,
    pub model: ModelId,
    /// The tool was bound to it now. False when a choice was already made, which is left alone.
    pub bound: bool,
}

impl ToolDesk {
    /// Registers the local model of `feature`, served by the ComfyUI at `endpoint` (a loopback
    /// address), and binds the tool to it if nothing is chosen yet.
    pub fn enable_local(&self, feature: &Feature, endpoint: &str) -> Result<LocalEnabled, AiError> {
        let recipe = feature.local.as_ref().filter(|r| r.usable).ok_or_else(|| {
            AiError::unsupported(format!(
                "{} has no local model that can run yet",
                feature.title
            ))
        })?;
        let role = feature
            .role()
            .ok_or_else(|| AiError::invalid("the tool id is not valid"))?;
        let address = local_address(endpoint)?;
        let id = ConnectionId::new(format!("local-{}", feature.id))
            .map_err(|e| AiError::invalid(e.to_string()))?;
        let model_id =
            ModelId::new(&recipe.workflow).map_err(|e| AiError::invalid(e.to_string()))?;
        let model = tool_model(feature, model_id.clone(), &recipe.label);
        let label = format!("{} on this computer", recipe.label);
        let connection = local_connection(id, label, "comfyui", Some(address), model);
        self.register(connection, role.id, model_id)
    }

    /// Saves `connection` (replacing one with the same id) and binds `role` to it unless a choice
    /// is already made. A choice already made, even one that no longer works, is never replaced.
    fn register(
        &self,
        connection: Connection,
        role: craft_ai_core::RoleId,
        model: ModelId,
    ) -> Result<LocalEnabled, AiError> {
        let id = connection.id.clone();
        let binding = Binding {
            role,
            connection: id.clone(),
            model: model.clone(),
        };
        let mut bound = false;
        self.shared.update(|store| {
            match store.connections.iter_mut().find(|c| c.id == id) {
                Some(existing) => *existing = connection,
                None => store.connections.push(connection),
            }
            if !store.defaults.iter().any(|b| b.role == binding.role) {
                store.defaults.push(binding);
                bound = true;
            }
        })?;
        Ok(LocalEnabled {
            connection: id,
            model,
            bound,
        })
    }
}

impl ToolDesk {
    /// Registers the first built-in runtime of `feature` (its recipe's [`NativeRoute`](craft_ai_features::NativeRoute))
    /// reading the weights at `weights`, and binds the tool to it if nothing is chosen yet.
    pub fn enable_native(
        &self,
        feature: &Feature,
        weights: &Path,
    ) -> Result<LocalEnabled, AiError> {
        let first = feature
            .local
            .as_ref()
            .and_then(|r| r.native.first())
            .map(|n| n.provider.clone())
            .ok_or_else(|| {
                AiError::unsupported(format!("{} has no built-in runtime", feature.title))
            })?;
        self.enable_native_with(feature, &first, weights)
    }

    /// Registers the built-in runtime named `provider` (`candle`, `burn`) from the feature's recipe.
    /// Only a runtime the recipe lists is accepted, matched exactly. Every runtime reads the same
    /// weights file and gets its own connection; the tool is bound only if nothing is chosen yet.
    pub fn enable_native_with(
        &self,
        feature: &Feature,
        provider: &str,
        weights: &Path,
    ) -> Result<LocalEnabled, AiError> {
        let native = feature
            .local
            .as_ref()
            .filter(|r| r.usable)
            .and_then(|r| r.native.iter().find(|n| n.provider == provider))
            .ok_or_else(|| {
                AiError::unsupported(format!(
                    "{} has no built-in {provider} runtime",
                    feature.title
                ))
            })?;
        let role = feature
            .role()
            .ok_or_else(|| AiError::invalid("the tool id is not valid"))?;
        let file = weights_file(weights)?;
        let id = ConnectionId::new(format!("local-{}-{}", feature.id, native.provider))
            .map_err(|e| AiError::invalid(e.to_string()))?;
        let model_id = ModelId::new(&native.model).map_err(|e| AiError::invalid(e.to_string()))?;
        let mut model = tool_model(feature, model_id.clone(), &native.label);
        model.local_file = Some(file);
        let connection = local_connection(id, native.label.clone(), &native.provider, None, model);
        self.register(connection, role.id, model_id)
    }
}

impl AiClient {
    /// The weights file for a tool whose chosen model an app runs itself with `provider`.
    ///
    /// `None` only when nothing is bound. A choice run by another adapter, one with no file
    /// recorded, or a file that is gone is an error, never a cue to look elsewhere.
    pub fn local_file_for(&self, role: &Role, provider: &str) -> Result<Option<PathBuf>, AiError> {
        let Some(route) = self.local_route_for(role)? else {
            return Ok(None);
        };
        if route.provider != provider {
            return Err(AiError::unsupported(
                "The model you chose can't be run by this app yet. Choose another in Set up AI.",
            ));
        }
        let file = route.file.ok_or_else(|| {
            AiError::invalid("The model you chose isn't fully set up. Open Set up AI to fix it.")
        })?;
        let path = PathBuf::from(file);
        if !path.is_file() {
            return Err(AiError::unsupported(
                "The file for the model you chose is no longer there. Open Set up AI to fix it.",
            ));
        }
        Ok(Some(path))
    }

    /// The selected local inference route, with the exact adapter name and model. An unbound
    /// tool returns `None`; a broken or remote choice is an error, never a cue to fall back.
    pub fn local_route_for(&self, role: &Role) -> Result<Option<LocalRoute>, AiError> {
        let store = self.shared.load()?;
        let resolved = match store.resolve(self.app(), role) {
            Ok(route) => route,
            Err(ResolveError::Unbound(_)) => return Ok(None),
            Err(error) => return Err(AiError::unsupported(error.to_string())),
        };
        if resolved.connection.locality != Locality::Local {
            return Err(AiError::unsupported(
                "The model you chose runs online, not on this computer.",
            ));
        }
        Ok(Some(LocalRoute {
            connection: resolved.connection.id.clone(),
            model: resolved.model.id.clone(),
            provider: resolved.connection.provider.clone(),
            endpoint: resolved.connection.endpoint.clone(),
            file: resolved.model.local_file.clone(),
        }))
    }

    /// Address for a local ComfyUI session (PhotoCraft's hover path). Only `None` when nothing
    /// is bound. A chosen native, remote or broken route is an error, never a signal to run the
    /// old hard-coded ComfyUI address. The exact model is in [`local_route_for`](Self::local_route_for).
    pub fn endpoint_for(&self, role: &Role) -> Result<Option<String>, AiError> {
        let Some(route) = self.local_route_for(role)? else {
            return Ok(None);
        };
        if route.provider != "comfyui" {
            return Err(AiError::unsupported(
                "The model you chose can't be run by this app yet. Choose another in Set up AI.",
            ));
        }
        route.endpoint.map(Some).ok_or_else(|| {
            AiError::invalid("The model you chose isn't fully set up. Open Set up AI to fix it.")
        })
    }
}

/// The model as the tool sees it: what it takes is what the tool sends, as the catalogue declares it.
fn tool_model(feature: &Feature, id: ModelId, label: &str) -> ModelInfo {
    let mut spec = CapabilitySpec::new(feature.capability.clone());
    for input in &feature.inputs {
        spec = spec.accepting(input.clone(), 1);
    }
    ModelInfo::new(id, label).with(spec)
}

/// A connection to a model on this computer: free, and no key.
fn local_connection(
    id: ConnectionId,
    label: String,
    provider: &str,
    endpoint: Option<String>,
    model: ModelInfo,
) -> Connection {
    Connection {
        id,
        label,
        provider: provider.to_owned(),
        endpoint,
        locality: Locality::Local,
        auth: Auth::None,
        billing: Billing::Free,
        models: vec![model],
        enabled: true,
        key_header: None,
    }
}

/// An absolute path to an existing `.safetensors` file, as text.
fn weights_file(path: &Path) -> Result<String, AiError> {
    let text = path
        .to_str()
        .filter(|t| !t.contains('\0'))
        .ok_or_else(|| AiError::invalid("the weights path is not valid text"))?;
    let safetensors = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("safetensors"));
    if !path.is_absolute() || !safetensors || !path.is_file() {
        return Err(AiError::invalid(
            "the weights must be an existing .safetensors file, given by its full path",
        ));
    }
    Ok(text.to_owned())
}

/// A loopback address with nothing after the port, or an error. A local model is only ever
/// reached on this computer.
fn local_address(input: &str) -> Result<String, AiError> {
    let trimmed = input.trim_end_matches('/');
    let endpoint = Endpoint::parse(trimmed)
        .map_err(|_| AiError::invalid("the local address is not a valid address"))?;
    if !endpoint.is_loopback() || trimmed.contains(['?', '#']) {
        return Err(AiError::invalid(
            "a local model must be reached on this computer, without a query or fragment",
        ));
    }
    Ok(trimmed.to_owned())
}

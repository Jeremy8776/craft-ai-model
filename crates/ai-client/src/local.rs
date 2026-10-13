//! A tool's local model becomes something an app can read (Phase 2e, vertical slice).
//!
//! The catalogue says a tool has a local recipe (SAM 3 for Select subject), and the launcher shows
//! it Ready once the files and node pack are in place. Until now nothing in the shared store
//! represented that, so no tool could be bound to it and no app could read a choice. This writes
//! the missing piece: a local connection that offers the recipe's model for the tool, and binds
//! the tool to it unless a person has already chosen something else.

use craft_ai_features::Feature;
use craft_ai_model::endpoint::Endpoint;
use craft_ai_model::role::ResolveError;
use craft_ai_model::{
    AiError, Auth, Billing, Binding, CapabilitySpec, Connection, ConnectionId, Locality, ModelId,
    ModelInfo, Role,
};

use crate::client::AiClient;
use crate::tools::ToolDesk;

/// The exact local connection and model chosen for a tool, without any key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalRoute {
    pub connection: ConnectionId,
    pub model: ModelId,
    pub provider: String,
    pub endpoint: Option<String>,
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
        // What the model takes is what the tool sends, as the catalogue declares it.
        let mut spec = CapabilitySpec::new(feature.capability.clone());
        for input in &feature.inputs {
            spec = spec.accepting(input.clone(), 1);
        }
        let connection = Connection {
            id: id.clone(),
            label: format!("{} on this computer", recipe.label),
            provider: "comfyui".to_owned(),
            endpoint: Some(address),
            locality: Locality::Local,
            auth: Auth::None,
            billing: Billing::Free,
            models: vec![ModelInfo::new(model_id.clone(), recipe.label.clone()).with(spec)],
            enabled: true,
            key_header: None,
        };
        let binding = Binding {
            role: role.id,
            connection: id.clone(),
            model: model_id.clone(),
        };
        let mut bound = false;
        self.shared.update(|store| {
            match store.connections.iter_mut().find(|c| c.id == id) {
                Some(existing) => *existing = connection,
                None => store.connections.push(connection),
            }
            // A choice already made, even one that no longer works, is never replaced.
            if !store.defaults.iter().any(|b| b.role == binding.role) {
                store.defaults.push(binding);
                bound = true;
            }
        })?;
        Ok(LocalEnabled {
            connection: id,
            model: model_id,
            bound,
        })
    }
}

impl AiClient {
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
                "the chosen model is remote; use the client job and consent flow",
            ));
        }
        Ok(Some(LocalRoute {
            connection: resolved.connection.id.clone(),
            model: resolved.model.id.clone(),
            provider: resolved.connection.provider.clone(),
            endpoint: resolved.connection.endpoint.clone(),
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
            return Err(AiError::unsupported(format!(
                "the selected {} model cannot run through comfyui",
                route.provider
            )));
        }
        route
            .endpoint
            .map(Some)
            .ok_or_else(|| AiError::invalid("the selected comfyui connection has no address"))
    }
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

//! A workflow library: imported API-format graphs kept as JSON files one per workflow.
//!
//! The launcher (or an app's Preferences) copies or writes a workflow file into the library
//! directory; every ComfyUI connection built from the same directory sees the same workflows.
//! Nothing is inferred from a file name beyond a derived id; the graph, the prompt binding and
//! the output node are stored inside each file and re-validated on every load.
//!
//! File shape (all fields except `graph` optional; see [`WorkflowSpec::import`]):
//!
//! ```json
//! {
//!   "id": "kite-image",
//!   "capability": "image.generate",
//!   "graph": { "1": { "class_type": "CLIPTextEncode", "inputs": { "text": "..." } } },
//!   "prompt": { "node": "1", "class_type": "CLIPTextEncode", "input": "text" },
//!   "output_node": "2"
//! }
//! ```
//!
//! A bare API-format graph (the object ComfyUI's "Save (API Format)" produces) is also
//! accepted; then the id comes from the file name and the binding and output node are
//! inferred. A UI-format export (with `nodes` and `links`) is refused with a message that
//! says which export to use instead.

// The per-OS library location, supplied by the native crate on non-wasm builds so this crate
// stays free of a hard dependency on it.
#[cfg(not(target_arch = "wasm32"))]
mod native_paths {
    pub use craft_ai_native::paths;
}

use crate::workflow::{Binding, Workflow};
use craft_ai_core::{AiError, ErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Largest workflow file accepted, in bytes. A graph is JSON text; a few MiB is generous.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Extension used for every stored workflow.
const EXTENSION: &str = "json";

/// What a stored workflow adds around the bare graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowSpec {
    /// Defaults to the file name's stem, then to `workflow`.
    #[serde(default)]
    pub id: String,
    /// Defaults to `image.generate`.
    #[serde(default = "default_capability")]
    pub capability: String,
    pub graph: Value,
    /// When absent, the only node with a plain string `text` input is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<Binding>,
    /// Required for image.inpaint; maps the uploaded source filename to a LoadImage node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Binding>,
    /// Required for image.inpaint; maps the uploaded PNG filename to a LoadImageMask node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Binding>,
    /// When absent, the last node that is not linked to by any other is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_node: Option<String>,
}

fn default_capability() -> String {
    "image.generate".to_owned()
}

impl WorkflowSpec {
    /// Validates everything and builds the checked [`Workflow`].
    pub fn to_workflow(&self) -> Result<Workflow, AiError> {
        let binding = match &self.prompt {
            Some(p) => p.clone(),
            None => infer_binding(&self.graph)?,
        };
        let output = match &self.output_node {
            Some(n) => n.clone(),
            None => infer_output_node(&self.graph)?,
        };
        if self.capability == "image.inpaint" {
            return Workflow::import_inpaint(
                if self.id.trim().is_empty() {
                    "workflow"
                } else {
                    self.id.trim()
                },
                self.graph.clone(),
                binding,
                &output,
                self.source
                    .clone()
                    .ok_or_else(|| AiError::invalid("inpainting needs a source binding"))?,
                self.mask
                    .clone()
                    .ok_or_else(|| AiError::invalid("inpainting needs a mask binding"))?,
            );
        }
        if self.source.is_some() || self.mask.is_some() {
            return Err(AiError::invalid(
                "source and mask bindings need image.inpaint",
            ));
        }
        Workflow::import(
            if self.id.trim().is_empty() {
                "workflow"
            } else {
                self.id.trim()
            },
            &self.capability,
            self.graph.clone(),
            binding,
            &output,
        )
    }

    /// Parses a stored file. `id` overrides the spec's `id` when the file has none
    /// (it is the stem the library saved it under).
    pub fn parse(bytes: &[u8], id: Option<&str>) -> Result<Self, AiError> {
        let value: Value = serde_json::from_slice(bytes).map_err(|_| {
            AiError::new(ErrorKind::InvalidRequest, "workflow file is not valid JSON")
        })?;
        // A UI-format export is not an API graph; ComfyUI itself would reject it. Detect it
        // before anything else: it has a top-level `nodes` array and no `graph` field.
        if value
            .get("nodes")
            .and_then(Value::as_array)
            .is_some_and(|n| !n.is_empty())
        {
            return Err(AiError::new(
                ErrorKind::InvalidRequest,
                "this is a UI-format export; use Save (API Format) instead",
            ));
        }
        // A bare API-format graph (the node map ComfyUI's "Save (API Format)" produces) is
        // also accepted: an object whose keys are node ids.
        let is_bare_graph = value
            .as_object()
            .is_some_and(|o| !o.is_empty() && o.values().all(|n| n.get("class_type").is_some()));
        let mut spec: WorkflowSpec = if is_bare_graph {
            Self {
                id: String::new(),
                capability: default_capability(),
                graph: value,
                prompt: None,
                source: None,
                mask: None,
                output_node: None,
            }
        } else {
            serde_json::from_value(value).map_err(|_| {
                AiError::new(ErrorKind::InvalidRequest, "workflow file is not valid JSON")
            })?
        };
        if spec.id.trim().is_empty() {
            spec.id = id.unwrap_or("workflow").to_owned();
        }
        spec.to_workflow()?;
        Ok(spec)
    }

    /// Builds a spec from explicit parts. `None` binding or output node defers to inference.
    pub fn import(
        id: &str,
        capability: &str,
        graph: Value,
        prompt: Option<Binding>,
        output_node: Option<String>,
    ) -> Result<Self, AiError> {
        let spec = Self {
            id: id.to_owned(),
            capability: capability.to_owned(),
            graph,
            prompt,
            source: None,
            mask: None,
            output_node,
        };
        spec.to_workflow()?;
        Ok(spec)
    }
}

/// The single string `text` input of the only node that has one.
fn infer_binding(graph: &Value) -> Result<Binding, AiError> {
    let Some(nodes) = graph.as_object() else {
        return Err(AiError::new(
            ErrorKind::InvalidRequest,
            "API workflow must be a node map",
        ));
    };
    let mut found: Option<(String, String)> = None;
    for (id, node) in nodes {
        let class = node
            .get("class_type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(inputs) = node.get("inputs").and_then(Value::as_object) else {
            continue;
        };
        for (field, value) in inputs {
            if field == "text" && value.as_str().is_some() {
                if found.is_some() {
                    return Err(AiError::new(
                        ErrorKind::InvalidRequest,
                        "several nodes have a text input; name the prompt node in the file",
                    ));
                }
                found = Some((id.clone(), field.clone()));
                let _ = class;
            }
        }
    }
    let Some((node, input)) = found else {
        return Err(AiError::new(
            ErrorKind::InvalidRequest,
            "no node has a text input to map the prompt to",
        ));
    };
    let class_type = graph
        .get(&node)
        .and_then(|n| n.get("class_type"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Ok(Binding::new(node, class_type, input))
}

/// A node no other node consumes: a sink. The last one in file order wins, matching how
/// ComfyUI orders its output nodes.
fn infer_output_node(graph: &Value) -> Result<String, AiError> {
    let Some(nodes) = graph.as_object() else {
        return Err(AiError::new(
            ErrorKind::InvalidRequest,
            "API workflow must be a node map",
        ));
    };
    let consumed: Vec<String> = nodes
        .values()
        .filter_map(|n| n.get("inputs").and_then(Value::as_object))
        .flat_map(|inputs| inputs.values())
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let mut sink: Option<&String> = None;
    for id in nodes.keys() {
        if !consumed.iter().any(|c| c == id) {
            sink = Some(id);
        }
    }
    sink.map(|s| s.to_owned()).ok_or_else(|| {
        AiError::new(
            ErrorKind::InvalidRequest,
            "no output node found; name it in the file",
        )
    })
}

/// A directory of workflow files.
#[derive(Clone, Debug)]
pub struct WorkflowLibrary {
    dir: PathBuf,
}

impl WorkflowLibrary {
    /// The library at `dir`, created on demand when something is saved.
    pub fn at(dir: impl Into<PathBuf>) -> Result<Self, AiError> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).map_err(|_| {
            AiError::new(
                ErrorKind::Internal,
                "could not create the workflow directory",
            )
        })?;
        Ok(Self { dir })
    }

    /// The default library directory, beside the shared store (`workflows/` in the AI dir).
    /// `None` when the OS reports no config directory. Native builds only.
    pub fn at_default_location() -> Option<Self> {
        // The path comes from the native crate; this one stays I/O-light and wasm-safe
        // apart from this cfg'd block.
        #[cfg(not(target_arch = "wasm32"))]
        {
            native_paths::paths::workflow_dir().map(Self::new_unchecked)
        }
        #[cfg(target_arch = "wasm32")]
        {
            None
        }
    }

    fn new_unchecked(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Reads every workflow file. A damaged or invalid file is skipped, not fatal: one bad
    /// file must not hide the rest of the library.
    pub fn load(&self) -> Result<Vec<Workflow>, AiError> {
        self.load_specs()
            .map(|specs| specs.iter().filter_map(|s| s.to_workflow().ok()).collect())
    }

    /// Like [`load`](Self::load) but returns the stored specs, for editors and listings.
    pub fn load_specs(&self) -> Result<Vec<WorkflowSpec>, AiError> {
        let mut specs = Vec::new();
        let entries = fs::read_dir(&self.dir).map_err(|_| {
            AiError::new(ErrorKind::Internal, "could not read the workflow directory")
        })?;
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == EXTENSION))
            .collect();
        paths.sort();
        for path in paths {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("workflow");
            if let Ok(bytes) = read_bounded(&path)
                && let Ok(spec) = WorkflowSpec::parse(&bytes, Some(stem))
            {
                specs.push(spec);
            }
        }
        Ok(specs)
    }

    /// Imports a workflow file from anywhere on disk into the library and returns the id it
    /// was stored under. A spec that already names an id replaces the file of that id.
    pub fn import_file(&self, source: &Path) -> Result<String, AiError> {
        let bytes = read_bounded(source)?;
        let stem = source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("workflow");
        let spec = WorkflowSpec::parse(&bytes, Some(stem))?;
        let id = craft_ai_core::ModelId::new(&spec.id)
            .map_err(|_| AiError::new(ErrorKind::InvalidRequest, "workflow id is not usable"))?
            .as_str()
            .to_owned();
        let mut stored = spec;
        stored.id = id.clone();
        self.save(&stored)?;
        Ok(id)
    }

    /// Writes one workflow file. The id becomes the file name.
    pub fn save(&self, spec: &WorkflowSpec) -> Result<(), AiError> {
        let id = craft_ai_core::ModelId::new(spec.id.trim())
            .map_err(|_| AiError::new(ErrorKind::InvalidRequest, "workflow id is not usable"))?
            .as_str()
            .to_owned();
        let json = serde_json::to_vec(spec)
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not encode the workflow"))?;
        let path = self.dir.join(format!("{id}.{EXTENSION}"));
        fs::write(&path, &json)
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not write the workflow file"))
    }

    /// Deletes one workflow file. Missing is fine.
    pub fn remove(&self, id: &str) -> Result<(), AiError> {
        let id = craft_ai_core::ModelId::new(id)
            .map_err(|_| AiError::new(ErrorKind::InvalidRequest, "workflow id is not usable"))?
            .as_str()
            .to_owned();
        let _ = fs::remove_file(self.dir.join(format!("{id}.{EXTENSION}")));
        Ok(())
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, AiError> {
    let file = fs::File::open(path).map_err(|_| {
        AiError::new(
            ErrorKind::InvalidRequest,
            "could not open the workflow file",
        )
    })?;
    let size = file.metadata().map(|m| m.len()).map_err(|_| {
        AiError::new(
            ErrorKind::InvalidRequest,
            "could not read the workflow file",
        )
    })?;
    if size > MAX_FILE_BYTES {
        return Err(AiError::new(
            ErrorKind::InvalidRequest,
            "workflow file is too large",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AiError::new(ErrorKind::Network, "workflow file broke during reading"))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(AiError::new(
            ErrorKind::InvalidRequest,
            "workflow file is too large",
        ));
    }
    Ok(bytes)
}

//! Imported API-format workflows and explicit, checked request-to-node mappings.
//! No node/model discovery is inferred from a workflow's name. Only declared fields change.
use craft_ai_core::capability::CapabilityId;
use craft_ai_core::model::{CapabilitySpec, InputRole, ModelInfo};
use craft_ai_core::payload::{Limits, Payload};
use craft_ai_core::{AiError, ErrorKind, ModelId, Request};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_GRAPH_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Binding {
    pub node: String,
    pub class_type: String,
    pub input: String,
}

impl Binding {
    pub fn new(
        node: impl Into<String>,
        class_type: impl Into<String>,
        input: impl Into<String>,
    ) -> Self {
        Self {
            node: node.into(),
            class_type: class_type.into(),
            input: input.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Workflow {
    id: ModelId,
    capability: CapabilityId,
    graph: Value,
    prompt: Binding,
    output_node: String,
    source: Option<Binding>,
    mask: Option<Binding>,
}

impl Workflow {
    /// Imports an API-format graph, not the browser's UI-format workflow export.
    pub fn import(
        id: &str,
        capability: &str,
        graph: Value,
        prompt: Binding,
        output_node: &str,
    ) -> Result<Self, AiError> {
        let id = ModelId::new(id).map_err(|_| AiError::invalid("invalid workflow id"))?;
        let capability = CapabilityId::parse(capability)
            .map_err(|_| AiError::invalid("invalid workflow capability"))?;
        if !matches!(
            capability.as_str(),
            "image.generate" | "video.generate" | "image.inpaint"
        ) {
            return Err(AiError::unsupported(
                "only image.generate, video.generate and image.inpaint workflows are mapped",
            ));
        }
        if serde_json::to_vec(&graph).map_or(true, |v| v.len() > MAX_GRAPH_BYTES) {
            return Err(AiError::invalid("workflow graph is too large"));
        }
        let nodes = graph
            .as_object()
            .ok_or_else(|| AiError::invalid("API workflow must be a node map"))?;
        if nodes.is_empty()
            || nodes.len() > 2048
            || !nodes
                .keys()
                .all(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(AiError::invalid(
                "workflow node ids must be decimal numbers",
            ));
        }
        for node in nodes.values() {
            if node.get("class_type").and_then(Value::as_str).is_none()
                || node.get("inputs").and_then(Value::as_object).is_none()
            {
                return Err(AiError::invalid(
                    "workflow nodes need class_type and inputs",
                ));
            }
        }
        let mapped = nodes
            .get(&prompt.node)
            .ok_or_else(|| AiError::invalid("prompt node is missing"))?;
        if mapped.get("class_type").and_then(Value::as_str) != Some(&prompt.class_type) {
            return Err(AiError::invalid("prompt node class has changed"));
        }
        if !mapped
            .get("inputs")
            .and_then(Value::as_object)
            .and_then(|inputs| inputs.get(&prompt.input))
            .is_some_and(Value::is_string)
        {
            return Err(AiError::invalid(
                "prompt input must be a text field, not a linked node",
            ));
        }
        if !nodes.contains_key(output_node) {
            return Err(AiError::invalid("output node is missing"));
        }
        if capability.as_str() == "image.inpaint" {
            return Err(AiError::invalid(
                "inpainting needs explicit source and mask bindings",
            ));
        }
        Ok(Self {
            id,
            capability,
            graph,
            prompt,
            output_node: output_node.to_owned(),
            source: None,
            mask: None,
        })
    }

    /// Import an inpainting graph only with both image-file bindings explicitly mapped.
    pub fn import_inpaint(
        id: &str,
        graph: Value,
        prompt: Binding,
        output_node: &str,
        source: Binding,
        mask: Binding,
    ) -> Result<Self, AiError> {
        let mut workflow = Self::import(id, "image.generate", graph, prompt, output_node)?;
        workflow.check_media_binding(&source, "source")?;
        workflow.check_media_binding(&mask, "mask")?;
        if source.node == mask.node && source.input == mask.input {
            return Err(AiError::invalid(
                "source and mask must map to different fields",
            ));
        }
        for (binding, role) in [
            (&workflow.prompt, "prompt"),
            (&source, "source"),
            (&mask, "mask"),
        ] {
            if !workflow.feeds_output(&binding.node) {
                return Err(AiError::invalid(format!(
                    "{role} node is not connected to the output"
                )));
            }
        }
        workflow.capability = CapabilityId::parse("image.inpaint")
            .map_err(|_| AiError::invalid("invalid inpaint capability"))?;
        workflow.source = Some(source);
        workflow.mask = Some(mask);
        Ok(workflow)
    }

    fn feeds_output(&self, target: &str) -> bool {
        let mut pending = vec![self.output_node.as_str()];
        let mut visited = std::collections::HashSet::new();
        while let Some(node_id) = pending.pop() {
            if node_id == target {
                return true;
            }
            if !visited.insert(node_id) {
                continue;
            }
            if let Some(inputs) = self
                .graph
                .get(node_id)
                .and_then(|n| n.get("inputs"))
                .and_then(Value::as_object)
            {
                for value in inputs.values() {
                    if let Some(link) = value.as_array()
                        && link.len() == 2
                        && link[1].is_number()
                        && let Some(parent) = link[0].as_str()
                        && self.graph.get(parent).is_some()
                    {
                        pending.push(parent);
                    }
                }
            }
        }
        false
    }

    fn check_media_binding(&self, binding: &Binding, role: &str) -> Result<(), AiError> {
        if binding.input != "image"
            || (role == "source" && binding.class_type != "LoadImage")
            || (role == "mask" && binding.class_type != "LoadImageMask")
        {
            return Err(AiError::unsupported(format!(
                "{role} must bind to the image field of a ComfyUI image loader"
            )));
        }
        let Some(node) = self.graph.get(&binding.node) else {
            return Err(AiError::invalid(format!("{role} node is missing")));
        };
        if node.get("class_type").and_then(Value::as_str) != Some(binding.class_type.as_str()) {
            return Err(AiError::invalid(format!("{role} node class has changed")));
        }
        if !node
            .get("inputs")
            .and_then(|v| v.get(&binding.input))
            .is_some_and(Value::is_string)
        {
            return Err(AiError::invalid(format!(
                "{role} input must be a filename field"
            )));
        }
        if binding.node == self.prompt.node && binding.input == self.prompt.input {
            return Err(AiError::invalid(format!(
                "{role} cannot replace the prompt field"
            )));
        }
        Ok(())
    }

    /// Checks installed node classes and static selector values from `/object_info`.
    /// This is a preflight, not a guarantee that execution will succeed.
    pub fn validate_installation(&self, info: &Value) -> Result<(), AiError> {
        let classes = info.as_object().ok_or_else(|| {
            AiError::new(
                ErrorKind::Provider,
                "ComfyUI returned invalid node metadata",
            )
        })?;
        for (node_id, node) in self
            .graph
            .as_object()
            .into_iter()
            .flat_map(|nodes| nodes.iter())
        {
            let class = node
                .get("class_type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let definition = classes.get(class).ok_or_else(|| {
                AiError::unsupported(format!("ComfyUI has no installed node class {class}"))
            })?;
            let inputs = definition
                .get("input")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    AiError::new(
                        ErrorKind::Provider,
                        "ComfyUI returned incomplete node metadata",
                    )
                })?;
            if let Some(fields) = node.get("inputs").and_then(Value::as_object) {
                for (field, value) in fields {
                    let Some(value) = value.as_str() else {
                        continue;
                    };
                    let schema = ["required", "optional"]
                        .iter()
                        .filter_map(|kind| inputs.get(*kind).and_then(Value::as_object))
                        .find_map(|schema| schema.get(field));
                    let dynamic = self
                        .source
                        .iter()
                        .chain(self.mask.iter())
                        .any(|binding| &binding.node == node_id && binding.input == *field);
                    if dynamic && schema.is_none() {
                        return Err(AiError::unsupported(format!(
                            "ComfyUI has no mapped image input for {class}.{field}"
                        )));
                    }
                    let choices = schema
                        .and_then(|schema| schema.get(0))
                        .and_then(Value::as_array);
                    if !dynamic
                        && let Some(choices) = choices
                        && !choices.iter().any(|choice| choice.as_str() == Some(value))
                    {
                        return Err(AiError::unsupported(format!(
                            "ComfyUI does not have the selected value for {class}.{field}"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn info(&self) -> ModelInfo {
        let mut spec = CapabilitySpec::new(self.capability.clone())
            .accepting(InputRole::Prompt, 1)
            .requiring(InputRole::Prompt);
        if self.capability.as_str() == "image.inpaint" {
            spec = spec
                .accepting(InputRole::Source, 1)
                .requiring(InputRole::Source)
                .accepting(InputRole::Mask, 1)
                .requiring(InputRole::Mask);
        }
        ModelInfo::new(self.id.clone(), self.id.as_str()).with(spec)
    }

    pub fn id(&self) -> &ModelId {
        &self.id
    }
    pub fn output_node(&self) -> &str {
        &self.output_node
    }
    pub fn capability(&self) -> &CapabilityId {
        &self.capability
    }

    pub fn validate_request(&self, request: &Request) -> Result<(), AiError> {
        request.validate(
            self.info()
                .spec_for(&self.capability)
                .ok_or_else(|| AiError::invalid("workflow has no capability"))?,
            &Limits::default(),
        )?;
        if !request.params.is_empty() {
            return Err(AiError::unsupported(
                "this workflow has no mapped parameters",
            ));
        }
        let prompt = request.inputs.iter().find(|i| i.role == InputRole::Prompt);
        if !matches!(prompt.map(|i| &i.payload), Some(Payload::Text { .. })) {
            return Err(AiError::invalid("prompt must be text"));
        }
        for role in [InputRole::Source, InputRole::Mask] {
            if let Some(input) = request.inputs.iter().find(|i| i.role == role) {
                match &input.payload {
                    Payload::Inline { media_type, data }
                        if media_type.as_str() == "image/png"
                            && data.starts_with(b"\x89PNG\r\n\x1a\n") => {}
                    Payload::Inline { media_type, data }
                        if role == InputRole::Source
                            && media_type.as_str() == "image/jpeg"
                            && data.starts_with(b"\xff\xd8\xff") => {}
                    // Only the declared type is known without reading the file; its bytes are
                    // checked when it is uploaded.
                    Payload::Handle(h)
                        if h.media_type.as_str() == "image/png"
                            || (role == InputRole::Source
                                && h.media_type.as_str() == "image/jpeg") => {}
                    _ => {
                        return Err(AiError::invalid(format!(
                            "{role} needs a PNG image (source also accepts JPEG)"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn render(&self, request: &Request) -> Result<Value, AiError> {
        if self.capability.as_str() == "image.inpaint" {
            return Err(AiError::unsupported(
                "inpainting needs uploaded source and mask files",
            ));
        }
        self.render_mapped(request, None)
    }

    /// Substitute only server-returned upload filenames, never caller-supplied paths.
    pub fn render_with_uploads(
        &self,
        request: &Request,
        source: &str,
        mask: &str,
    ) -> Result<Value, AiError> {
        if self.capability.as_str() != "image.inpaint" {
            return Err(AiError::unsupported(
                "this workflow has no source and mask mappings",
            ));
        }
        for name in [source, mask] {
            if !crate::http::safe_upload_name(name) {
                return Err(AiError::invalid("uploaded filename is not safe"));
            }
        }
        self.render_mapped(request, Some((source, mask)))
    }

    fn render_mapped(
        &self,
        request: &Request,
        uploads: Option<(&str, &str)>,
    ) -> Result<Value, AiError> {
        self.validate_request(request)?;
        let Some(Payload::Text { text }) = request
            .inputs
            .iter()
            .find(|i| i.role == InputRole::Prompt)
            .map(|i| &i.payload)
        else {
            return Err(AiError::invalid("prompt must be text"));
        };
        let mut graph = self.graph.clone();
        let field = graph
            .get_mut(&self.prompt.node)
            .and_then(|v| v.get_mut("inputs"))
            .and_then(|v| v.get_mut(&self.prompt.input))
            .ok_or_else(|| AiError::invalid("mapped prompt field is missing"))?;
        *field = Value::String(text.clone());
        if let Some((source, mask)) = uploads {
            for (binding, filename) in [(self.source.as_ref(), source), (self.mask.as_ref(), mask)]
            {
                let binding = binding.ok_or_else(|| AiError::invalid("missing inpaint binding"))?;
                let field = graph
                    .get_mut(&binding.node)
                    .and_then(|v| v.get_mut("inputs"))
                    .and_then(|v| v.get_mut(&binding.input))
                    .ok_or_else(|| AiError::invalid("mapped image field is missing"))?;
                *field = Value::String(filename.to_owned());
            }
        }
        Ok(graph)
    }
}

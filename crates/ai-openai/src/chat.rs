//! Chat completions: building the request body and reading the reply, whole or streamed.
//!
//! Everything here is pure (no I/O) so it is tested without a server.
//!
//! Inputs map to messages in the order given, except that `system` inputs come first: `system`
//! becomes a system message, `prompt` and `context` become user messages. Only text inputs are sent; a non-text payload is refused rather
//! than silently dropped.
//!
//! Params that are passed through (anything else is ignored): `temperature`, `top_p`,
//! `max_tokens`, `seed`, `stop`, `frequency_penalty`, `presence_penalty`, `tools`, `tool_choice`
//! and `response_format`. Giving `response_format` makes the request *structured*: the reply is
//! checked to be JSON and arrives as one `application/json` output instead of text deltas.

use craft_ai_core::job::{Event, Output};
use craft_ai_core::{AiError, ErrorKind, InputRole, Payload, Request, Usage};
use serde_json::{Map, Value, json};

/// Largest single SSE line accepted, in bytes.
pub(crate) const MAX_SSE_LINE: usize = 1024 * 1024;
/// Most parallel tool calls tracked in one reply.
const MAX_TOOL_CALLS: usize = 64;

const PASS_THROUGH: &[&str] = &[
    "temperature",
    "top_p",
    "max_tokens",
    "seed",
    "stop",
    "frequency_penalty",
    "presence_penalty",
    "tools",
    "tool_choice",
    "response_format",
];

/// A request ready to send.
#[derive(Debug)]
pub(crate) struct ChatPlan {
    pub body: Value,
    pub stream: bool,
    pub structured: bool,
}

pub(crate) fn plan(model: &str, req: &Request) -> Result<ChatPlan, AiError> {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for input in &req.inputs {
        let role = if input.role == InputRole::System {
            "system"
        } else if input.role == InputRole::Prompt || input.role == InputRole::Context {
            "user"
        } else {
            return Err(AiError::unsupported(format!(
                "text models do not take a {} input",
                input.role
            )));
        };
        let Payload::Text { text } = &input.payload else {
            return Err(AiError::unsupported(
                "this adapter sends text inputs to text models only",
            ));
        };
        let list = if role == "system" {
            &mut system
        } else {
            &mut messages
        };
        list.push(json!({"role": role, "content": text}));
    }
    // System messages go first whatever order they were given in: some local chat templates
    // reject one that follows a user message.
    system.append(&mut messages);
    let messages = system;
    if messages.is_empty() {
        return Err(AiError::invalid("a text request needs a prompt"));
    }
    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    body.insert("messages".into(), Value::Array(messages));
    for key in PASS_THROUGH {
        if let Some(v) = req.params.get(*key) {
            body.insert((*key).to_owned(), v.clone());
        }
    }
    if req.stream {
        body.insert("stream".into(), json!(true));
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
    let structured = req.params.contains_key("response_format");
    Ok(ChatPlan {
        body: Value::Object(body),
        stream: req.stream,
        structured,
    })
}

/// What a finished reply held.
#[derive(Debug, Default)]
pub(crate) struct Completion {
    pub text: String,
    pub tool_calls: Vec<(String, String)>,
    pub usage: Option<Usage>,
}

pub(crate) fn usage_from(v: &Value) -> Option<Usage> {
    let u = v.get("usage").filter(|u| u.is_object())?;
    let n = |keys: &[&str]| keys.iter().find_map(|k| u.get(*k).and_then(Value::as_u64));
    // OpenRouter reports what a request cost, in credits (US dollars), as `usage.cost`. Whether the
    // number can be believed depends on who sent it, so the caller keeps it only for OpenRouter
    // (see `Http::trust_cost`); here it is read as a plain number.
    let cost = u
        .get("cost")
        .and_then(Value::as_f64)
        .filter(|c| c.is_finite() && (0.0..1.0e9).contains(c))
        .map(dollars_to_micros);
    Some(Usage {
        input_units: n(&["prompt_tokens", "input_tokens"]),
        output_units: n(&["completion_tokens", "output_tokens"]),
        cost_micros: cost,
        currency: cost.map(|_| "USD".to_owned()),
    })
}

/// Dollars to millionths, rounded. The caller has checked the range.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn dollars_to_micros(dollars: f64) -> u64 {
    (dollars * 1_000_000.0).round() as u64
}

/// Reads a non-streamed `chat.completion` object.
pub(crate) fn parse_completion(v: &Value) -> Result<Completion, AiError> {
    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
        return Err(AiError::new(ErrorKind::Provider, error_text(e)));
    }
    let message = v
        .pointer("/choices/0/message")
        .ok_or_else(|| AiError::new(ErrorKind::Provider, "the reply had no choices"))?;
    let text = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut tool_calls = Vec::new();
    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_TOOL_CALLS)
    {
        let name = call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let args = call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}");
        tool_calls.push((name.to_owned(), args.to_owned()));
    }
    Ok(Completion {
        text,
        tool_calls,
        usage: usage_from(v),
    })
}

pub(crate) fn error_text(e: &Value) -> String {
    let msg = e
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| e.as_str())
        .unwrap_or("the server reported an error");
    msg.chars().take(300).collect()
}

/// Turns a finished reply into events: structured replies become one JSON output.
pub(crate) fn events_for(c: &Completion, structured: bool) -> Result<Vec<Event>, AiError> {
    let mut events = Vec::new();
    if structured {
        let parsed: Value = serde_json::from_str(c.text.trim()).map_err(|_| {
            AiError::new(
                ErrorKind::Provider,
                "the model was asked for structured output but did not return valid JSON",
            )
        })?;
        let bytes = serde_json::to_vec(&parsed)
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not encode the JSON reply"))?;
        events.push(Event::Output(Output {
            payload: Payload::inline("application/json", bytes)?,
            label: None,
        }));
    } else if !c.text.is_empty() {
        events.push(Event::TextDelta {
            text: c.text.clone(),
        });
    }
    for (name, arguments) in &c.tool_calls {
        events.push(Event::ToolCall {
            name: name.clone(),
            arguments: arguments.clone(),
        });
    }
    Ok(events)
}

/// Reads a server-sent-events stream one line at a time.
#[derive(Debug, Default)]
pub(crate) struct SseState {
    /// All text so far (needed for the structured check at the end).
    pub text: String,
    tools: Vec<(String, String)>,
    pub usage: Option<Usage>,
    /// `[DONE]` was seen.
    pub done: bool,
    /// A `finish_reason` was seen.
    pub finished: bool,
}

impl SseState {
    /// Feeds one line. Returns the new text, if the line carried any.
    pub fn line(&mut self, raw: &str) -> Result<Option<String>, AiError> {
        let line = raw.trim_end_matches(['\r', '\n']);
        // Blank lines end an event; comments start with a colon; other fields are not used.
        let Some(data) = line.strip_prefix("data:") else {
            return Ok(None);
        };
        let data = data.trim_start();
        if data == "[DONE]" {
            self.done = true;
            return Ok(None);
        }
        let v: Value = serde_json::from_str(data).map_err(|_| {
            AiError::new(ErrorKind::Provider, "the stream held an unreadable event")
        })?;
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            return Err(AiError::new(ErrorKind::Provider, error_text(e)));
        }
        if let Some(u) = usage_from(&v) {
            self.usage = Some(u);
        }
        let Some(choice) = v.pointer("/choices/0") else {
            return Ok(None);
        };
        if choice.get("finish_reason").is_some_and(|f| !f.is_null()) {
            self.finished = true;
        }
        if let Some(calls) = choice
            .pointer("/delta/tool_calls")
            .and_then(Value::as_array)
        {
            for call in calls {
                self.tool_delta(call);
            }
        }
        let delta = choice
            .pointer("/delta/content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        Ok(delta.map(|d| {
            self.text.push_str(d);
            d.to_owned()
        }))
    }

    fn tool_delta(&mut self, call: &Value) {
        let index = call
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|i| usize::try_from(i).ok())
            .unwrap_or(0);
        if index >= MAX_TOOL_CALLS {
            return;
        }
        if self.tools.len() <= index {
            self.tools.resize(index + 1, (String::new(), String::new()));
        }
        if let Some(slot) = self.tools.get_mut(index) {
            if let Some(n) = call.pointer("/function/name").and_then(Value::as_str) {
                slot.0.push_str(n);
            }
            if let Some(a) = call.pointer("/function/arguments").and_then(Value::as_str) {
                slot.1.push_str(a);
            }
        }
    }

    /// The finished reply, once the stream has ended.
    pub fn into_completion(self) -> Completion {
        let tool_calls = self
            .tools
            .into_iter()
            .filter(|(name, _)| !name.is_empty())
            .map(|(n, a)| (n, if a.is_empty() { "{}".to_owned() } else { a }))
            .collect();
        Completion {
            text: self.text,
            tool_calls,
            usage: self.usage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_ai_core::CapabilityId;

    fn req() -> Request {
        Request::new(CapabilityId::parse("text.complete").unwrap())
    }

    fn text(s: &str) -> Payload {
        Payload::text(s)
    }

    #[test]
    fn inputs_become_messages_in_order() {
        let r = req()
            .with_input(InputRole::System, text("be brief"))
            .with_input(InputRole::Prompt, text("hi"))
            .with_input(InputRole::Context, text("more"));
        let p = plan("m", &r).unwrap();
        let roles: Vec<_> = p.body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(roles, ["system", "user", "user"]);
        assert!(p.body.get("stream").is_none());
        assert!(!p.structured);
    }

    #[test]
    fn only_listed_params_pass_through() {
        let r = req()
            .with_input(InputRole::Prompt, text("hi"))
            .with_param("temperature", json!(0.2))
            .with_param("max_tokens", json!(50))
            .with_param("response_format", json!({"type": "json_object"}))
            .with_param("api_key", json!("sneaky"))
            .with_param("base_url", json!("http://elsewhere"));
        let p = plan("m", &r).unwrap();
        assert_eq!(p.body["temperature"], 0.2);
        assert!(p.structured);
        assert!(p.body.get("api_key").is_none());
        assert!(p.body.get("base_url").is_none());
    }

    #[test]
    fn openrouter_cost_is_read_as_exact_millionths_of_a_dollar() {
        let usage = |cost: serde_json::Value| {
            usage_from(&json!({"usage":{"prompt_tokens":1,"cost":cost}})).unwrap()
        };
        let u = usage(json!(0.00014));
        assert_eq!(
            (u.cost_micros, u.currency.as_deref()),
            (Some(140), Some("USD"))
        );
        assert_eq!(usage(json!(0.95)).cost_micros, Some(950_000));
        assert_eq!(usage(json!(0)).cost_micros, Some(0));
        // Anything that is not a sane amount of money is not a cost.
        for bad in [
            json!(-1.0),
            json!("0.5"),
            json!(null),
            json!(1.0e12),
            json!(true),
        ] {
            let u = usage(bad.clone());
            assert_eq!((u.cost_micros, u.currency), (None, None), "{bad}");
        }
        assert!(
            usage_from(&json!({"usage":{"prompt_tokens":1}}))
                .unwrap()
                .cost_micros
                .is_none()
        );
    }

    #[test]
    fn streaming_asks_for_usage() {
        let p = plan(
            "m",
            &req().with_input(InputRole::Prompt, text("x")).streaming(),
        )
        .unwrap();
        assert_eq!(p.body["stream"], true);
        assert_eq!(p.body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn non_text_and_odd_roles_are_refused_not_dropped() {
        let img = Payload::inline("image/png", vec![1]).unwrap();
        let e = plan("m", &req().with_input(InputRole::Prompt, img)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unsupported);
        let e = plan("m", &req().with_input(InputRole::Mask, text("x"))).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unsupported);
        assert_eq!(
            plan("m", &req()).unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
    }

    #[test]
    fn parses_a_completion_with_tools_and_usage() {
        let v = json!({"choices":[{"message":{"content":"hello","tool_calls":[
            {"function":{"name":"get_time","arguments":"{\"tz\":\"UTC\"}"}}]}}],
            "usage":{"prompt_tokens":3,"completion_tokens":5}});
        let c = parse_completion(&v).unwrap();
        assert_eq!(c.text, "hello");
        assert_eq!(
            c.tool_calls,
            [("get_time".into(), "{\"tz\":\"UTC\"}".into())]
        );
        assert_eq!(c.usage.unwrap().output_units, Some(5));
    }

    #[test]
    fn null_content_with_tool_call_is_fine_and_garbage_is_an_error() {
        let v = json!({"choices":[{"message":{"content":null,"tool_calls":[
            {"function":{"name":"f","arguments":"{}"}}]}}]});
        assert!(parse_completion(&v).unwrap().text.is_empty());
        assert!(parse_completion(&json!({})).is_err());
        assert!(parse_completion(&json!({"error":{"message":"nope"}})).is_err());
    }

    #[test]
    fn structured_replies_become_a_json_output_and_bad_json_is_refused() {
        let ok = Completion {
            text: " {\"a\": 1} ".into(),
            ..Completion::default()
        };
        let events = events_for(&ok, true).unwrap();
        assert!(matches!(&events[0], Event::Output(_)));
        let bad = Completion {
            text: "sure! here it is".into(),
            ..Completion::default()
        };
        assert_eq!(
            events_for(&bad, true).unwrap_err().kind,
            ErrorKind::Provider
        );
        let plain = events_for(&bad, false).unwrap();
        assert!(matches!(&plain[0], Event::TextDelta { .. }));
    }

    #[test]
    fn sse_accumulates_text_tools_and_usage() {
        let mut s = SseState::default();
        let lines = [
            ": keep-alive",
            "",
            "event: message",
            r#"data: {"choices":[{"delta":{"content":"Hel"}}]}"#,
            r#"data:{"choices":[{"delta":{"content":"lo"}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"f","arguments":"{\"a\""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":1}"}}]},"finish_reason":"tool_calls"}]}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":2,"completion_tokens":4}}"#,
            "data: [DONE]",
        ];
        let mut deltas = Vec::new();
        for l in lines {
            if let Some(d) = s.line(l).unwrap() {
                deltas.push(d);
            }
        }
        assert_eq!(deltas, ["Hel", "lo"]);
        assert!(s.done && s.finished);
        let c = s.into_completion();
        assert_eq!(c.text, "Hello");
        assert_eq!(c.tool_calls, [("f".into(), "{\"a\":1}".into())]);
        assert_eq!(c.usage.unwrap().input_units, Some(2));
    }

    #[test]
    fn sse_errors_and_garbage_are_errors_not_panics() {
        let mut s = SseState::default();
        assert!(s.line("data: {not json").is_err());
        assert!(
            s.line(r#"data: {"error":{"message":"overloaded"}}"#)
                .is_err()
        );
        // Hostile indices are ignored rather than allocated.
        s.line(r#"data: {"choices":[{"delta":{"tool_calls":[{"index":4000000000,"function":{"name":"x"}}]}}]}"#)
            .unwrap();
        assert!(s.into_completion().tool_calls.is_empty());
    }
}

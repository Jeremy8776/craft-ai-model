//! The OpenAI-compatible provider against a scripted localhost server, driven through the shared
//! conformance checks. No real network and no real key are used.

mod support;

use craft_ai_core::conformance::{
    run_cancel_paced, run_lifecycle_paced, run_rejects, run_unknown_job,
};
use craft_ai_core::job::{Event, JobState};
use craft_ai_core::payload::base64_encode;
use craft_ai_core::{
    AiError, CapabilityId, ErrorKind, InputRole, ModelId, Payload, Provider, Request, SecretString,
};
use craft_ai_openai::{OpenAiConfig, OpenAiProvider};
use serde_json::json;
use support::{Reply, Server, Step, sse};

const KEY: &str = "sk-test-needle-123";

const PNG: [u8; 12] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 1];

/// The provider works on its own threads, so give it a moment between polls.
fn pause() {
    std::thread::sleep(std::time::Duration::from_millis(5));
}

fn lifecycle(
    p: &dyn Provider,
    m: &ModelId,
    r: &Request,
) -> Result<craft_ai_core::conformance::Completed, String> {
    run_lifecycle_paced(p, m, r, &pause)
}

fn cancel(p: &dyn Provider, m: &ModelId, r: &Request) -> Result<(), String> {
    run_cancel_paced(p, m, r, &pause)
}

fn cap(s: &str) -> CapabilityId {
    CapabilityId::parse(s).unwrap()
}

fn model(s: &str) -> ModelId {
    ModelId::new(s).unwrap()
}

fn provider(url: &str) -> OpenAiProvider {
    OpenAiProvider::new(OpenAiConfig::new(url).with_key(SecretString::new(KEY))).unwrap()
}

fn text_req() -> Request {
    Request::new(cap("text.complete")).with_input(InputRole::Prompt, Payload::text("hi"))
}

fn image_req() -> Request {
    Request::new(cap("image.generate")).with_input(InputRole::Prompt, Payload::text("a fox"))
}

fn png() -> Payload {
    Payload::inline("image/png", PNG.to_vec()).unwrap()
}

fn completion(text: &str) -> String {
    json!({"choices":[{"message":{"content":text}}],
           "usage":{"prompt_tokens":3,"completion_tokens":2}})
    .to_string()
}

fn chunk(delta: serde_json::Value) -> Step {
    sse(&json!({"choices":[{"delta": delta}]}).to_string())
}

fn texts(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::TextDelta { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn failed(state: &JobState) -> &AiError {
    match state {
        JobState::Failed { error } => error,
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn models_are_listed_classified_and_sorted_with_the_key_sent() {
    let server = Server::start(|_| {
        Reply::Json(
            200,
            json!({"data":[{"id":"gpt-image-1"},{"id":"text-embedding-3-small"},{"id":"gpt-4o"},
                           {"id":"whisper-1"},{"id":"llama3"},{"id":"gpt-4o"}]})
            .to_string(),
        )
    });
    let models = provider(&server.url).models().unwrap();
    let ids: Vec<_> = models.iter().map(|m| m.id.to_string()).collect();
    assert_eq!(ids, ["gpt-4o", "gpt-image-1", "llama3"]);
    let req = &server.recorded()[0];
    assert_eq!(req.path, "/v1/models");
    assert_eq!(req.header("authorization"), Some(&*format!("Bearer {KEY}")));
}

#[test]
fn no_key_means_no_authorization_header() {
    let server = Server::start(|_| Reply::Json(200, json!({"data":[]}).to_string()));
    let p = OpenAiProvider::new(OpenAiConfig::new(&server.url)).unwrap();
    p.models().unwrap();
    assert!(server.recorded()[0].header("authorization").is_none());
}

#[test]
fn text_reply_passes_the_lifecycle_and_sends_the_right_body() {
    let server = Server::start(|_| Reply::Json(200, completion("Hello there")));
    let req = text_req()
        .with_input(InputRole::System, Payload::text("be brief"))
        .with_param("temperature", json!(0.1));
    let done = lifecycle(&provider(&server.url), &model("gpt-4o"), &req).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(texts(&done.events), ["Hello there"]);
    let sent = &server.recorded()[0];
    assert_eq!(sent.path, "/v1/chat/completions");
    let body = sent.body_json();
    assert_eq!(body["model"], "gpt-4o");
    // The system message is hoisted ahead of the prompt even though it was given after it.
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"], "hi");
    assert_eq!(body["temperature"], 0.1);
    assert!(body.get("stream").is_none());
}

#[test]
fn streamed_text_arrives_as_deltas_and_passes_the_lifecycle() {
    let server = Server::start(|_| {
        Reply::Sse(vec![
            chunk(json!({"content": "Hel"})),
            Step::Wait(30),
            chunk(json!({"content": "lo"})),
            Step::Wait(30),
            chunk(json!({"content": "!"})),
            sse(&json!({"choices":[{"delta":{},"finish_reason":"stop"}]}).to_string()),
            sse(
                &json!({"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":3}})
                    .to_string(),
            ),
            Step::Send("data: [DONE]\n\n".into()),
        ])
    });
    let done = lifecycle(
        &provider(&server.url),
        &model("gpt-4o"),
        &text_req().streaming(),
    )
    .unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(texts(&done.events).concat(), "Hello!");
    let body = server.recorded()[0].body_json();
    assert_eq!(body["stream"], true);
}

#[test]
fn streamed_tool_call_arrives_whole() {
    let server = Server::start(|_| {
        Reply::Sse(vec![
            chunk(
                json!({"tool_calls":[{"index":0,"function":{"name":"get_time","arguments":"{\"tz\""}}]}),
            ),
            chunk(json!({"tool_calls":[{"index":0,"function":{"arguments":":\"UTC\"}"}}]})),
            Step::Send("data: [DONE]\n\n".into()),
        ])
    });
    let req = text_req().streaming().with_param(
        "tools",
        json!([{"type":"function","function":{"name":"get_time"}}]),
    );
    let done = lifecycle(&provider(&server.url), &model("gpt-4o"), &req).unwrap();
    let calls: Vec<_> = done
        .events
        .iter()
        .filter_map(|e| match e {
            Event::ToolCall { name, arguments } => Some((name.clone(), arguments.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        [("get_time".to_owned(), "{\"tz\":\"UTC\"}".to_owned())]
    );
    assert!(server.recorded()[0].body_json()["tools"].is_array());
}

#[test]
fn structured_output_is_one_json_output_and_prose_is_a_failure() {
    let good = Server::start(|_| Reply::Json(200, completion("{\"answer\": 42}")));
    let req = text_req().with_param("response_format", json!({"type": "json_object"}));
    let done = lifecycle(&provider(&good.url), &model("gpt-4o"), &req).unwrap();
    let out = done
        .events
        .iter()
        .find_map(|e| match e {
            Event::Output(o) => Some(o),
            _ => None,
        })
        .unwrap();
    assert!(
        matches!(&out.payload, Payload::Inline { media_type, .. } if media_type.as_str() == "application/json")
    );

    let prose = Server::start(|_| Reply::Json(200, completion("Sure, here you go!")));
    let done = lifecycle(&provider(&prose.url), &model("gpt-4o"), &req).unwrap();
    assert_eq!(failed(&done.state).kind, ErrorKind::Provider);
}

#[test]
fn image_generation_returns_a_recognised_picture() {
    let b64 = base64_encode(&PNG);
    let server = Server::start(move |_| {
        Reply::Json(
            200,
            json!({"data":[{"b64_json": b64}],"usage":{"input_tokens":5,"output_tokens":9}})
                .to_string(),
        )
    });
    let req = image_req().with_param("size", json!("1024x1024"));
    let done = lifecycle(&provider(&server.url), &model("gpt-image-1"), &req).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    let Some(Event::Output(o)) = done.events.first() else {
        panic!("no output: {:?}", done.events)
    };
    assert!(matches!(&o.payload, Payload::Inline { media_type, data }
        if media_type.as_str() == "image/png" && data == &PNG));
    let sent = &server.recorded()[0];
    assert_eq!(sent.path, "/v1/images/generations");
    assert_eq!(sent.body_json()["size"], "1024x1024");
}

#[test]
fn inpainting_uploads_source_and_mask_as_multipart() {
    let b64 = base64_encode(&PNG);
    let server =
        Server::start(move |_| Reply::Json(200, json!({"data":[{"b64_json": b64}]}).to_string()));
    let req = Request::new(cap("image.inpaint"))
        .with_input(InputRole::Prompt, Payload::text("add a hat"))
        .with_input(InputRole::Source, png())
        .with_input(InputRole::Mask, png());
    let done = lifecycle(&provider(&server.url), &model("gpt-image-1"), &req).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    let sent = &server.recorded()[0];
    assert_eq!(sent.path, "/v1/images/edits");
    assert!(
        sent.header("content-type")
            .unwrap()
            .starts_with("multipart/form-data; boundary=")
    );
    let text = sent.body_text();
    assert!(text.contains("name=\"image\""));
    assert!(text.contains("name=\"mask\""));
    assert!(text.contains("add a hat"));
    assert!(sent.body.windows(PNG.len()).any(|w| w == PNG));
}

/// Status, extra headers, body, and the error kind expected.
type Case = (u16, Vec<(&'static str, String)>, String, ErrorKind);

#[test]
fn http_failures_map_to_kinds_and_never_echo_the_key() {
    let cases: Vec<Case> = vec![
        (
            401,
            vec![],
            json!({"error":{"message":format!("Incorrect API key provided: {KEY}")}}).to_string(),
            ErrorKind::Auth,
        ),
        (
            429,
            vec![("Retry-After", "3".into())],
            json!({"error":{"message":"slow down","code":"rate_limit_exceeded"}}).to_string(),
            ErrorKind::RateLimited,
        ),
        (
            429,
            vec![],
            json!({"error":{"message":"pay","code":"insufficient_quota"}}).to_string(),
            ErrorKind::Quota,
        ),
        (
            400,
            vec![],
            json!({"error":{"message":"bad size"}}).to_string(),
            ErrorKind::InvalidRequest,
        ),
        (
            500,
            vec![],
            "<html>oops</html>".to_owned(),
            ErrorKind::Provider,
        ),
    ];
    for (status, headers, body, kind) in cases {
        let server = Server::start(move |_| Reply::JsonWith(status, headers.clone(), body.clone()));
        let done = lifecycle(&provider(&server.url), &model("gpt-4o"), &text_req()).unwrap();
        let err = failed(&done.state);
        assert_eq!(err.kind, kind, "status {status}");
        assert!(
            !format!("{err} {err:?}").contains("needle"),
            "status {status}: {err}"
        );
        if status == 429 && kind == ErrorKind::RateLimited {
            assert_eq!(err.retry_after_secs, Some(3));
        }
    }
}

#[test]
fn unreachable_server_is_a_network_error() {
    let done = lifecycle(
        &provider(&Server::dead_url()),
        &model("gpt-4o"),
        &text_req(),
    )
    .unwrap();
    assert_eq!(failed(&done.state).kind, ErrorKind::Network);
}

#[test]
fn a_truncated_stream_is_a_failure_not_a_silent_success() {
    let server = Server::start(|_| Reply::Sse(vec![chunk(json!({"content": "half a sen"}))]));
    let done = lifecycle(
        &provider(&server.url),
        &model("gpt-4o"),
        &text_req().streaming(),
    )
    .unwrap();
    assert_eq!(failed(&done.state).kind, ErrorKind::Network);
}

#[test]
fn requests_the_model_cannot_take_are_refused_before_any_traffic() {
    let server = Server::start(|_| Reply::Json(500, "{}".into()));
    let p = provider(&server.url);
    // A text request to an image model, and an image request to a text model.
    run_rejects(
        &p,
        &model("gpt-image-1"),
        &text_req(),
        ErrorKind::Unsupported,
    )
    .unwrap();
    run_rejects(&p, &model("gpt-4o"), &image_req(), ErrorKind::Unsupported).unwrap();
    // A model this adapter does not serve at all.
    run_rejects(&p, &model("whisper-1"), &text_req(), ErrorKind::Unsupported).unwrap();
    // A mask on plain generation, and a missing source on an edit.
    let masked = image_req().with_input(InputRole::Mask, png());
    run_rejects(&p, &model("gpt-image-1"), &masked, ErrorKind::Unsupported).unwrap();
    let no_source = Request::new(cap("image.inpaint"))
        .with_input(InputRole::Prompt, Payload::text("x"))
        .with_input(InputRole::Mask, png());
    assert!(p.submit(&model("gpt-image-1"), &no_source).is_err());
    // Too many outputs for dall-e-3, and streaming an image.
    assert!(
        p.submit(&model("dall-e-3"), &image_req().with_outputs(2))
            .is_err()
    );
    assert!(
        p.submit(&model("gpt-image-1"), &image_req().streaming())
            .is_err()
    );
    assert_eq!(
        server.count(),
        0,
        "nothing may be sent for a refused request"
    );
}

#[test]
fn unknown_jobs_are_errors() {
    let p = provider(&Server::dead_url());
    run_unknown_job(&p).unwrap();
}

fn slow_stream_server() -> Server {
    let slow: Vec<Step> = (0..100)
        .flat_map(|_| [chunk(json!({"content": "x"})), Step::Wait(100)])
        .collect();
    let slow = std::sync::Mutex::new(Some(slow));
    Server::start(move |_| Reply::Sse(slow.lock().unwrap().take().unwrap_or_default()))
}

#[test]
fn cancel_passes_the_conformance_check_on_a_slow_stream() {
    let server = slow_stream_server();
    cancel(
        &provider(&server.url),
        &model("gpt-4o"),
        &text_req().streaming(),
    )
    .unwrap();
}

#[test]
fn cancelling_mid_stream_closes_the_connection() {
    let server = slow_stream_server();
    let p = provider(&server.url);
    let job = p.submit(&model("gpt-4o"), &text_req().streaming()).unwrap();
    // Wait until text is really flowing, so the cancel lands in the middle of the stream.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let snap = p.poll(&job).unwrap();
        if !texts(&snap.events).is_empty() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "no text arrived");
        pause();
    }
    p.cancel(&job).unwrap();
    assert_eq!(p.poll(&job).unwrap().state, JobState::Cancelled);
    // The worker notices on its next line and drops the connection; the stream would otherwise
    // run for ten seconds, so the server seeing the break within five proves it stopped early.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !server
        .client_dropped
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "server never saw the client leave"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn redirects_are_not_followed_so_the_key_cannot_be_forwarded() {
    let elsewhere = Server::start(|_| Reply::Json(200, completion("stolen")));
    let target = format!("{}/chat/completions", elsewhere.url);
    let server = Server::start(move |_| {
        Reply::JsonWith(307, vec![("Location", target.clone())], String::new())
    });
    let done = lifecycle(&provider(&server.url), &model("gpt-4o"), &text_req()).unwrap();
    assert!(matches!(done.state, JobState::Failed { .. }));
    assert_eq!(
        elsewhere.count(),
        0,
        "the redirect target must never be contacted"
    );
}

#[test]
fn a_redirected_model_list_is_not_followed_either() {
    let elsewhere =
        Server::start(|_| Reply::Json(200, json!({"data":[{"id":"gpt-4o"}]}).to_string()));
    let target = format!("{}/models", elsewhere.url);
    let server = Server::start(move |_| {
        Reply::JsonWith(302, vec![("Location", target.clone())], String::new())
    });
    assert!(provider(&server.url).models().is_err());
    assert_eq!(
        elsewhere.count(),
        0,
        "the redirect target must never be contacted"
    );
}

#[test]
fn only_so_many_jobs_run_at_once() {
    let slow: Vec<Step> = vec![Step::Wait(1500), Step::Send("data: [DONE]\n\n".into())];
    let slow = std::sync::Mutex::new(Some(slow));
    let server =
        Server::start(move |_| Reply::Sse(slow.lock().unwrap().take().unwrap_or_default()));
    let mut cfg = OpenAiConfig::new(&server.url).with_key(SecretString::new(KEY));
    cfg.max_active_jobs = 1;
    let p = OpenAiProvider::new(cfg).unwrap();
    let first = p.submit(&model("gpt-4o"), &text_req().streaming()).unwrap();
    let second = p.submit(&model("gpt-4o"), &text_req().streaming());
    assert_eq!(second.unwrap_err().kind, ErrorKind::RateLimited);
    p.cancel(&first).unwrap();
    // A slot frees once the first job is terminal.
    assert!(p.submit(&model("gpt-4o"), &text_req().streaming()).is_ok());
}

/// Submits one text job and polls it to the end, returning the usage the final snapshot carried.
fn usage_of(server_url: &str) -> Option<craft_ai_core::Usage> {
    let p = provider(server_url);
    let id = p.submit(&model("gpt-4o"), &text_req()).unwrap();
    for _ in 0..400 {
        let snap = p.poll(&id).unwrap();
        if snap.state.is_terminal() {
            assert_eq!(snap.state, JobState::Succeeded);
            return snap.usage;
        }
        pause();
    }
    panic!("the job never finished");
}

#[test]
fn a_cost_field_from_an_unknown_server_is_not_shown_as_money() {
    let reply = json!({"choices":[{"message":{"content":"hi"}}],
                       "usage":{"prompt_tokens":3,"completion_tokens":2,"cost":0.00042}})
    .to_string();
    let server = Server::start(move |_| Reply::Json(200, reply.clone()));
    let usage = usage_of(&server.url).unwrap();
    assert_eq!(usage.input_units, Some(3), "tokens are still reported");
    assert_eq!(usage.cost_micros, None);
    assert_eq!(usage.currency, None);
}

#[test]
fn prices_in_a_model_list_are_ignored_unless_the_server_is_openrouter() {
    let list = r#"{"object":"list","data":[{"id":"gpt-4o","created":1700000000,
                   "pricing":{"prompt":"0.000005","completion":"0.000015"}}]}"#;
    let server = Server::start(move |_| Reply::Json(200, list.to_owned()));
    let models = provider(&server.url).models().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(
        (models[0].pricing.clone(), models[0].released),
        (None, None)
    );
}

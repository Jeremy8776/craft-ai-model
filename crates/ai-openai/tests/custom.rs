//! What a custom (unlisted, OpenAI-shaped) server needs from the adapter: a named key header
//! that still never follows a redirect, a raw model list, and models declared by a person with
//! only the abilities they were declared with. Scripted localhost servers only.

mod support;

use craft_ai_core::conformance::run_lifecycle_paced;
use craft_ai_core::job::JobState;
use craft_ai_core::{
    CapabilityId, ErrorKind, InputRole, ModelId, Payload, Provider, Request, SecretString,
};
use craft_ai_openai::catalog::{Kind, declare, suggest};
use craft_ai_openai::probe::{MAX_LISTED, list_models};
use craft_ai_openai::{OpenAiConfig, OpenAiProvider};
use serde_json::json;
use support::{Reply, Server};

const KEY: &str = "gw-key-7f3a";

fn config(url: &str) -> OpenAiConfig {
    OpenAiConfig::new(url).with_key(SecretString::new(KEY))
}

fn ok_models() -> Reply {
    Reply::Json(200, json!({"data": [{"id": "alpha"}]}).to_string())
}

fn pause() {
    std::thread::sleep(std::time::Duration::from_millis(5));
}

#[test]
fn the_key_goes_as_a_bearer_token_unless_a_header_is_named() {
    let server = Server::start(|_| ok_models());
    OpenAiProvider::new(config(&server.url))
        .unwrap()
        .models()
        .unwrap();
    let seen = &server.recorded()[0];
    assert_eq!(
        seen.header("authorization"),
        Some(&*format!("Bearer {KEY}"))
    );
    assert_eq!(seen.header("x-api-key"), None);
}

#[test]
fn a_named_header_carries_the_bare_key_and_no_authorization_is_sent() {
    let server = Server::start(|_| ok_models());
    OpenAiProvider::new(config(&server.url).with_key_header("x-api-key"))
        .unwrap()
        .models()
        .unwrap();
    let seen = &server.recorded()[0];
    assert_eq!(seen.header("x-api-key"), Some(KEY));
    assert_eq!(seen.header("authorization"), None);
}

#[test]
fn a_named_header_is_also_used_for_jobs() {
    let server = Server::start(|r| {
        if r.path.ends_with("/chat/completions") {
            Reply::Json(
                200,
                json!({"choices":[{"message":{"content":"hello"}}]}).to_string(),
            )
        } else {
            ok_models()
        }
    });
    let model = declare("alpha", &[Kind::Chat]).unwrap();
    let provider = OpenAiProvider::new(
        config(&server.url)
            .with_key_header("api-key")
            .with_models(vec![model]),
    )
    .unwrap();
    let request = Request::new(CapabilityId::parse("text.complete").unwrap())
        .with_input(InputRole::Prompt, Payload::text("hi"));
    let done =
        run_lifecycle_paced(&provider, &ModelId::new("alpha").unwrap(), &request, &pause).unwrap();
    assert!(
        matches!(done.state, JobState::Succeeded),
        "{:?}",
        done.state
    );
    let post = server
        .recorded()
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap();
    assert_eq!(post.header("api-key"), Some(KEY));
    assert_eq!(post.header("authorization"), None);
}

#[test]
fn a_redirect_never_carries_a_named_key_header_elsewhere() {
    let elsewhere = Server::start(|_| ok_models());
    let target = format!("{}/models", elsewhere.url);
    let server = Server::start(move |_| {
        Reply::JsonWith(307, vec![("Location", target.clone())], String::new())
    });
    let e = list_models(&config(&server.url).with_key_header("x-api-key")).unwrap_err();
    assert!(!e.message.contains(KEY));
    assert_eq!(
        elsewhere.count(),
        0,
        "the redirect target must not be contacted"
    );
    assert_eq!(server.count(), 1);
}

#[test]
fn an_error_that_echoes_a_named_key_is_redacted() {
    let server = Server::start(|r| {
        let echoed = r.header("x-api-key").unwrap_or_default().to_owned();
        Reply::Json(
            401,
            json!({"error": {"message": format!("bad key {echoed}")}}).to_string(),
        )
    });
    let e = list_models(&config(&server.url).with_key_header("x-api-key")).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Auth);
    assert!(!e.message.contains(KEY), "{}", e.message);
}

fn entries(n: usize) -> String {
    let data: Vec<_> = (0..n).map(|i| json!({"id": format!("m{i:05}")})).collect();
    json!({"data": data}).to_string()
}

#[test]
fn the_raw_list_keeps_every_id_without_guessing_which_are_chat_models() {
    let body = json!({"data": [
        {"id": "zeta"}, {"id": "text-embedding-9"}, {"id": "zeta"},
        {"nope": 1}, {"id": "has space"}, {"id": 7}
    ]});
    let server = Server::start(move |r| {
        // An unusual base path: only this one answers.
        if r.path == "/gw/ai/v7/models" {
            Reply::Json(200, body.to_string())
        } else {
            Reply::Json(404, "{}".into())
        }
    });
    let base = server.url.replace("/v1", "/gw/ai/v7");
    let listing = list_models(&config(&base)).unwrap();
    assert_eq!(listing.ids, ["text-embedding-9", "zeta"]);
    assert_eq!(listing.skipped, 3);
    assert!(!listing.truncated);
}

#[test]
fn a_huge_list_is_cut_and_says_so() {
    let body = entries(MAX_LISTED + 1);
    let server = Server::start(move |_| Reply::Json(200, body.clone()));
    let listing = list_models(&config(&server.url)).unwrap();
    assert_eq!(listing.ids.len(), MAX_LISTED);
    assert!(listing.truncated);
    let body = entries(MAX_LISTED);
    let server = Server::start(move |_| Reply::Json(200, body.clone()));
    let listing = list_models(&config(&server.url)).unwrap();
    assert_eq!(listing.ids.len(), MAX_LISTED);
    assert!(!listing.truncated);
}

#[test]
fn a_reply_without_a_data_list_is_an_error_the_person_can_read() {
    for body in [
        json!({"models": [{"id": "a"}]}),
        json!([{"id": "a"}]),
        json!({}),
    ] {
        let server = Server::start(move |_| Reply::Json(200, body.to_string()));
        let e = list_models(&config(&server.url)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Provider);
        assert!(e.message.contains("model list"), "{}", e.message);
    }
}

#[test]
fn a_declared_chat_model_does_what_was_declared_and_nothing_more() {
    let info = declare("alpha", &[Kind::Chat]).unwrap();
    assert_eq!(info.capabilities.len(), 1);
    let spec = &info.capabilities[0];
    assert_eq!(spec.capability.as_str(), "text.complete");
    assert!(spec.requires.contains(&InputRole::Prompt));
    let f = spec.features;
    assert!(!f.streaming && !f.tools && !f.structured_output, "{f:?}");

    let server = Server::start(|_| ok_models());
    let provider = OpenAiProvider::new(config(&server.url).with_models(vec![info])).unwrap();
    let streamed = Request::new(CapabilityId::parse("text.complete").unwrap())
        .with_input(InputRole::Prompt, Payload::text("hi"))
        .streaming();
    assert!(
        provider
            .submit(&ModelId::new("alpha").unwrap(), &streamed)
            .is_err()
    );
    let image = Request::new(CapabilityId::parse("image.generate").unwrap())
        .with_input(InputRole::Prompt, Payload::text("a fox"));
    assert!(
        provider
            .submit(&ModelId::new("alpha").unwrap(), &image)
            .is_err()
    );
    assert_eq!(server.count(), 0, "refused before any traffic");
}

#[test]
fn image_kinds_map_to_their_capabilities_with_one_output() {
    let info = declare(
        "pix",
        &[
            Kind::ImageInpaint,
            Kind::ImageGenerate,
            Kind::ImageEdit,
            Kind::ImageGenerate,
        ],
    )
    .unwrap();
    let names: Vec<_> = info
        .capabilities
        .iter()
        .map(|c| c.capability.to_string())
        .collect();
    assert_eq!(names, ["image.generate", "image.edit", "image.inpaint"]);
    for spec in &info.capabilities {
        assert_eq!(spec.max_outputs, 1, "{}", spec.capability);
        assert!(spec.requires.contains(&InputRole::Prompt));
    }
    let inpaint = &info.capabilities[2];
    assert!(inpaint.requires.contains(&InputRole::Mask));
    assert!(!info.capabilities[1].requires.contains(&InputRole::Mask));
}

#[test]
fn nothing_is_declared_without_a_kind_or_with_an_unusable_id() {
    assert!(declare("alpha", &[]).is_none());
    assert!(declare("has space", &[Kind::Chat]).is_none());
    assert!(declare("", &[Kind::Chat]).is_none());
}

#[test]
fn suggestions_from_a_name_are_only_suggestions() {
    assert_eq!(suggest("llama3.2:3b"), [Kind::Chat]);
    assert_eq!(
        suggest("gpt-image-1"),
        [Kind::ImageGenerate, Kind::ImageEdit, Kind::ImageInpaint]
    );
    assert_eq!(suggest("dall-e-3"), [Kind::ImageGenerate]);
    assert!(suggest("text-embedding-3-small").is_empty());
}

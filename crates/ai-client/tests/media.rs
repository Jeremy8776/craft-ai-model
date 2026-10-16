//! `ProviderFactory::with_media_dir`: hosted result links become local files, per connection.

#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use std::sync::Arc;

use craft_ai_client::ProviderFactory;
use craft_ai_core::{
    AiError, Connection, Event, JobId, JobSnapshot, JobState, ModelId, ModelInfo, Output, Payload,
    Provider, Request,
};
use serde_json::json;
use support::{Server, http_bytes};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR-fake-body";

/// Finishes at once with one output that links to `link`.
struct Hosted {
    link: String,
}

impl Provider for Hosted {
    fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(Vec::new())
    }
    fn submit(&self, _: &ModelId, _: &Request) -> Result<JobId, AiError> {
        JobId::new("job-1").map_err(|e| AiError::invalid(e.to_string()))
    }
    fn poll(&self, _: &JobId) -> Result<JobSnapshot, AiError> {
        let mut snap = JobSnapshot::new(JobState::Succeeded);
        snap.events.push(Event::Output(Output {
            payload: Payload::handle(&self.link, "image/png", None)?,
            label: None,
        }));
        Ok(snap)
    }
    fn cancel(&self, _: &JobId) -> Result<(), AiError> {
        Ok(())
    }
}

fn connection(locality: &str) -> Connection {
    serde_json::from_value(json!({
        "id": "hosted", "label": "Hosted", "provider": "hosted-test",
        "endpoint": "http://127.0.0.1:9", "locality": locality,
        "auth": {"kind": "none"}, "billing": "free", "models": []
    }))
    .unwrap()
}

fn factory(link: &str) -> ProviderFactory {
    let mut f = ProviderFactory::standard();
    let link = link.to_owned();
    f.register("hosted-test", move |_, _| {
        Ok(Arc::new(Hosted { link: link.clone() }))
    });
    f
}

fn first_uri(provider: &dyn Provider) -> String {
    let snap = provider.poll(&JobId::new("job-1").unwrap()).unwrap();
    match &snap.outputs().next().unwrap().payload {
        Payload::Handle(h) => h.uri.clone(),
        other => panic!("not a handle: {other:?}"),
    }
}

#[test]
fn without_the_option_links_are_returned_unchanged() {
    let server = Server::start(|_| http_bytes(200, &[], PNG));
    let link = format!("{}/r.png", server.url.trim_end_matches("/v1"));
    let provider = factory(&link).build(&connection("local"), None).unwrap();
    assert_eq!(first_uri(provider.as_ref()), link);
    assert_eq!(server.count(), 0);
}

#[test]
fn a_local_connection_gets_its_loopback_result_saved() {
    let server = Server::start(|_| http_bytes(200, &[], PNG));
    let dir = tempfile::tempdir().unwrap();
    let link = format!("{}/r.png", server.url.trim_end_matches("/v1"));
    let provider = factory(&link)
        .with_media_dir(dir.path())
        .unwrap()
        .build(&connection("local"), None)
        .unwrap();
    let uri = first_uri(provider.as_ref());
    assert!(uri.starts_with("file:"), "{uri}");
    let path = url::Url::parse(&uri).unwrap().to_file_path().unwrap();
    assert_eq!(std::fs::read(path).unwrap(), PNG);
}

#[test]
fn a_remote_connection_may_not_make_us_fetch_from_this_computer() {
    let server = Server::start(|_| http_bytes(200, &[], PNG));
    let dir = tempfile::tempdir().unwrap();
    let link = format!("{}/r.png", server.url.trim_end_matches("/v1"));
    let provider = factory(&link)
        .with_media_dir(dir.path())
        .unwrap()
        .build(&connection("remote"), None)
        .unwrap();
    let snap = provider.poll(&JobId::new("job-1").unwrap()).unwrap();
    assert_eq!(server.count(), 0);
    assert!(matches!(&snap.outputs().next().unwrap().payload, Payload::Handle(h) if h.uri == link));
    assert!(
        snap.events
            .iter()
            .any(|e| matches!(e, Event::Notice { message } if message.contains("No local copy")))
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn a_bad_media_directory_is_refused_up_front() {
    assert!(
        ProviderFactory::standard()
            .with_media_dir("relative/dir")
            .is_err()
    );
}

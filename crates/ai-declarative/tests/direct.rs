//! The request/response flow and the rules every declarative route keeps: one billable call at
//! most once, no redirects, no key or reply text in errors, bounded replies and job lists.

mod fixtures;

use craft_ai_core::conformance::run_lifecycle_paced;
use craft_ai_core::{ErrorKind, Event, JobState, Payload, Provider, SecretString};
use craft_ai_declarative::{DeclarativeConfig, DeclarativeProvider};
use fixtures::support::{Reply, Server, http_bytes};
use fixtures::{KEY, b64, direct_json, image_request, model, png_bytes, queue_json, spec};
use serde_json::{Value, json};

fn direct(server: &Server, edit: impl FnOnce(&mut Value)) -> DeclarativeProvider {
    let mut v = direct_json(&server.url);
    edit(&mut v);
    DeclarativeProvider::new(DeclarativeConfig::new(
        spec(&v),
        Some(SecretString::new(KEY)),
    ))
    .unwrap()
}

fn png_server() -> Server {
    let body = json!({"data": [{"b64_json": b64(&png_bytes())}]}).to_string();
    Server::start(move |_| Reply::Json(200, body.clone()))
}

fn failure(p: &DeclarativeProvider, id: &craft_ai_core::JobId) -> String {
    match p.poll(id).unwrap().state {
        JobState::Failed { error } => error.message,
        other => panic!("expected a failed job, got {other:?}"),
    }
}

#[test]
fn one_call_on_the_first_poll_returns_the_decoded_file() {
    let server = png_server();
    let p = direct(&server, |_| {});
    let done = run_lifecycle_paced(
        &p,
        &model(),
        &image_request("a \"red\" door\n{prompt}"),
        &|| {},
    )
    .unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    let Some(Event::Output(out)) = done.events.iter().find(|e| matches!(e, Event::Output(_)))
    else {
        panic!("no output: {:?}", done.events);
    };
    assert!(
        matches!(&out.payload, Payload::Inline { media_type, data } if media_type.as_str() == "image/png" && *data == png_bytes())
    );
    let sent = server.recorded();
    assert_eq!(sent.len(), 1, "polling again sends nothing");
    assert_eq!(
        sent[0].header("authorization"),
        Some(&*format!("Bearer {KEY}"))
    );
    assert_eq!(
        sent[0].body_json(),
        json!({"model": "img-1", "prompt": "a \"red\" door\n{prompt}"}),
        "the prompt stays one value: it adds no field and is not expanded again"
    );
}

#[test]
fn a_failed_call_ends_the_job_and_is_never_sent_again() {
    let server = Server::start(|_| Reply::Json(500, "{}".into()));
    let p = direct(&server, |_| {});
    let id = p.submit(&model(), &image_request("x")).unwrap();
    assert_eq!(server.count(), 0, "nothing is sent at submit");
    assert!(failure(&p, &id).contains("500"));
    for _ in 0..3 {
        assert!(p.poll(&id).unwrap().state.is_terminal());
    }
    assert_eq!(server.count(), 1, "a billable call is attempted once");
}

#[test]
fn a_file_that_is_not_the_declared_kind_fails_the_job() {
    for bytes in [
        b"<html>not an image</html>".to_vec(),
        b"\xFF\xD8\xFFjpeg".to_vec(),
    ] {
        let body = json!({"data": [{"b64_json": b64(&bytes)}]}).to_string();
        let server = Server::start(move |_| Reply::Json(200, body.clone()));
        let p = direct(&server, |_| {});
        let id = p.submit(&model(), &image_request("x")).unwrap();
        assert!(failure(&p, &id).contains("not the kind"));
    }
    let server = Server::start(|_| Reply::Json(200, r#"{"data":[{"b64_json":"!!!"}]}"#.into()));
    let p = direct(&server, |_| {});
    let id = p.submit(&model(), &image_request("x")).unwrap();
    assert!(failure(&p, &id).contains("base64"));
}

#[test]
fn cancelling_before_the_call_sends_nothing() {
    let server = png_server();
    let p = direct(&server, |_| {});
    let id = p.submit(&model(), &image_request("x")).unwrap();
    p.cancel(&id).unwrap();
    p.cancel(&id).unwrap();
    assert_eq!(p.poll(&id).unwrap().state, JobState::Cancelled);
    assert_eq!(server.count(), 0);
}

#[test]
fn a_second_poll_while_the_call_is_in_flight_does_not_send_it_again() {
    let server = Server::start(|_| {
        std::thread::sleep(std::time::Duration::from_millis(400));
        Reply::Json(200, r#"{"choices":[{"text":"hello"}]}"#.into())
    });
    let p = std::sync::Arc::new(direct(&server, |v| {
        v["flow"]["result"] = json!({"at": "/choices/0/text", "as": "text"});
    }));
    let id = p.submit(&model(), &image_request("x")).unwrap();
    let first = {
        let (p, id) = (p.clone(), id.clone());
        std::thread::spawn(move || p.poll(&id).unwrap())
    };
    std::thread::sleep(std::time::Duration::from_millis(100));
    let during = p.poll(&id).unwrap();
    assert!(!during.state.is_terminal(), "the call is still running");
    assert_eq!(first.join().unwrap().state, JobState::Succeeded);
    assert_eq!(server.count(), 1, "the call was sent once");
}

#[test]
fn a_text_result_is_returned_as_text() {
    let server = Server::start(|_| Reply::Json(200, r#"{"choices":[{"text":"hello"}]}"#.into()));
    let p = direct(&server, |v| {
        v["flow"]["result"] = json!({"at": "/choices/0/text", "as": "text"});
    });
    let done = run_lifecycle_paced(&p, &model(), &image_request("x"), &|| {}).unwrap();
    assert!(
        done.events
            .iter()
            .any(|e| matches!(e, Event::Output(o) if o.payload == Payload::text("hello")))
    );
}

#[test]
fn a_redirect_is_never_followed_and_the_key_goes_nowhere_else() {
    let elsewhere = Server::start(|_| Reply::Json(200, r#"{"data":[]}"#.into()));
    let target = elsewhere.url.clone();
    let server = Server::start(move |_| {
        Reply::JsonWith(302, vec![("Location", target.clone())], "{}".into())
    });
    let p = direct(&server, |_| {});
    let id = p.submit(&model(), &image_request("x")).unwrap();
    let message = failure(&p, &id);
    assert!(!message.contains(KEY), "{message}");
    assert_eq!(server.count(), 1);
    assert_eq!(elsewhere.count(), 0, "nothing was sent to the Location");
}

#[test]
fn errors_never_carry_the_key_or_the_services_own_words() {
    let server =
        Server::start(|_| Reply::Json(401, format!(r#"{{"error":"bad key {KEY} sk-other"}}"#)));
    let p = direct(&server, |_| {});
    let id = p.submit(&model(), &image_request("x")).unwrap();
    let message = failure(&p, &id);
    assert!(message.contains("401"), "{message}");
    assert!(
        !message.contains(KEY) && !message.contains("sk-other"),
        "{message}"
    );
    assert!(!format!("{p:?}").contains(KEY));
    let config = DeclarativeConfig::new(
        spec(&direct_json("https://x.example.com")),
        Some(SecretString::new(KEY)),
    );
    assert!(!format!("{config:?}").contains(KEY));
}

#[test]
fn a_reply_over_the_size_limit_is_refused() {
    let big = vec![b' '; 32 * 1024 * 1024 + 1];
    let server =
        Server::start(move |_| http_bytes(200, &[("Content-Type", "application/json")], &big));
    let p = direct(&server, |_| {});
    let id = p.submit(&model(), &image_request("x")).unwrap();
    assert!(failure(&p, &id).contains("too large"));
}

#[test]
fn a_service_with_no_key_sends_none_and_one_with_a_key_insists_on_it() {
    let server = png_server();
    let mut v = direct_json(&server.url);
    v["auth"] = json!({"kind": "none"});
    let p = DeclarativeProvider::new(DeclarativeConfig::new(
        spec(&v),
        Some(SecretString::new(KEY)),
    ))
    .unwrap();
    let id = p.submit(&model(), &image_request("x")).unwrap();
    assert_eq!(p.poll(&id).unwrap().state, JobState::Succeeded);
    let sent = server.recorded();
    assert!(
        sent[0].header("authorization").is_none()
            && sent[0].headers.iter().all(|(_, v)| !v.contains(KEY))
    );

    let e = DeclarativeProvider::new(DeclarativeConfig::new(
        spec(&direct_json(&server.url)),
        None,
    ))
    .unwrap_err();
    assert!(
        e.message.contains("Plain Images needs a key"),
        "{}",
        e.message
    );
    for bad in ["two\nlines", "has space", ""] {
        let cfg = DeclarativeConfig::new(
            spec(&direct_json(&server.url)),
            Some(SecretString::new(bad)),
        );
        assert_eq!(
            DeclarativeProvider::new(cfg).unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
    }
}

#[test]
fn a_full_list_forgets_finished_jobs_and_refuses_when_all_are_running() {
    let server = png_server();
    let p = direct(&server, |_| {});
    let first = p.submit(&model(), &image_request("x")).unwrap();
    assert_eq!(p.poll(&first).unwrap().state, JobState::Succeeded);
    let mut ids = vec![];
    for _ in 0..127 {
        ids.push(p.submit(&model(), &image_request("x")).unwrap());
    }
    // 128 tracked, one finished: the next submit makes room by forgetting the finished one.
    ids.push(p.submit(&model(), &image_request("x")).unwrap());
    assert!(
        p.poll(&first).is_err(),
        "the oldest finished job was forgotten"
    );
    let before = server.count();
    let e = p.submit(&model(), &image_request("x")).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Quota);
    assert_eq!(server.count(), before);
}

#[test]
fn a_queue_spec_makes_room_before_the_billable_call() {
    let server = Server::start(|r| match r.method.as_str() {
        "POST" => Reply::Json(200, r#"{"id":"run-1"}"#.into()),
        _ => Reply::Json(200, r#"{"status":"Working"}"#.into()),
    });
    let p = DeclarativeProvider::new(DeclarativeConfig::new(
        spec(&queue_json(&server.url)),
        Some(SecretString::new(KEY)),
    ))
    .unwrap();
    for _ in 0..128 {
        p.submit(&model(), &image_request("x")).unwrap();
    }
    let before = server.count();
    assert_eq!(
        p.submit(&model(), &image_request("x")).unwrap_err().kind,
        ErrorKind::Quota
    );
    assert_eq!(server.count(), before, "the paid submit was never sent");
}

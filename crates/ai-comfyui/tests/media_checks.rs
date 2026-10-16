//! p4/p10: media going to and coming from ComfyUI is checked by its bytes, not by a name or a
//! label. Outputs must be the media their file name says; source and mask images may arrive inline
//! or as a local file handle, are bounded, and must be the PNG or JPEG they are declared to be
//! before a single byte is sent. Scripted localhost servers and temporary folders only.
#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use craft_ai_comfyui::{Binding, ComfyConfig, ComfyProvider, Workflow};
use craft_ai_core::capability::CapabilityId;
use craft_ai_core::model::InputRole;
use craft_ai_core::payload::Payload;
use craft_ai_core::{ErrorKind, JobState, ModelId, Provider, Request};
use serde_json::json;
use support::{Reply, Server, http_bytes};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nimage body";
const JPEG: &[u8] = b"\xff\xd8\xff\xe0\x00\x10JFIF image body";
const MP4: &[u8] = b"\0\0\0\x18ftypisom video body";

// ---- outputs ----

fn generate_workflow(cap: &str) -> Workflow {
    Workflow::import(
        "local",
        cap,
        json!({
            "1":{"class_type":"CLIPTextEncode","inputs":{"text":"old"}},
            "2":{"class_type":"SaveOutput","inputs":{"data":["1",0]}}
        }),
        Binding::new("1", "CLIPTextEncode", "text"),
        "2",
    )
    .unwrap()
}

/// A server whose finished job names `filename` and answers `/view` with `body`.
fn output_server(filename: &'static str, key: &'static str, body: Vec<u8>) -> Server {
    Server::start(move |r| {
        if r.path == "/object_info" {
            Reply::Json(
                200,
                json!({
                    "CLIPTextEncode":{"input":{"required":{"text":["STRING",{}]}}},
                    "SaveOutput":{"input":{"required":{"data":["IMAGE",{}]}}}
                })
                .to_string(),
            )
        } else if r.path == "/prompt" {
            Reply::Json(200, r#"{"prompt_id":"job-1"}"#.into())
        } else if r.path == "/history/job-1" {
            Reply::Json(
                200,
                json!({"job-1":{"status":{"completed":true},"outputs":{"2":{
                    key:[{"filename":filename,"type":"output"}]}}}})
                .to_string(),
            )
        } else if r.path.starts_with("/view?") {
            http_bytes(200, &[], &body)
        } else {
            Reply::Json(404, "{}".into())
        }
    })
}

fn finish(server: &Server, config: ComfyConfig, cap: &str) -> craft_ai_core::job::JobSnapshot {
    let provider = ComfyProvider::new(config.with_workflow(generate_workflow(cap))).unwrap();
    let _ = server;
    let request = Request::new(CapabilityId::parse(cap).unwrap())
        .with_input(InputRole::Prompt, Payload::text("a lighthouse"));
    let id = provider
        .submit(&ModelId::new("local").unwrap(), &request)
        .unwrap();
    provider.poll(&id).unwrap()
}

fn base(server: &Server) -> ComfyConfig {
    ComfyConfig::new(server.url.trim_end_matches("/v1"))
}

#[test]
fn an_output_that_is_not_the_media_its_name_claims_is_refused_and_leaves_no_file() {
    let html = b"<!DOCTYPE html><html>expired</html>".to_vec();
    let json_error = br#"{"error":"out of memory"}"#.to_vec();
    let cases: Vec<(&str, &str, &str, Vec<u8>)> = vec![
        ("an error page named .png", "a.png", "images", html),
        ("a json body named .mp4", "a.mp4", "videos", json_error),
        ("jpeg bytes named .png", "a.png", "images", JPEG.to_vec()),
        ("png bytes named .mp4", "a.mp4", "videos", PNG.to_vec()),
        ("nothing at all", "a.png", "images", Vec::new()),
    ];
    for (why, name, key, body) in cases {
        let cap = if key == "videos" {
            "video.generate"
        } else {
            "image.generate"
        };
        // Inline, and to disk: neither may hand the bytes over, and disk must be left empty.
        let server = output_server(name, key, body.clone());
        let inline = finish(&server, base(&server), cap);
        assert!(matches!(inline.state, JobState::Failed { .. }), "{why}");
        assert_eq!(inline.outputs().count(), 0, "{why}");

        let dir = tempfile::tempdir().unwrap();
        let server = output_server(name, key, body);
        let saved = finish(&server, base(&server).with_output_dir(dir.path()), cap);
        assert!(matches!(saved.state, JobState::Failed { .. }), "{why}");
        assert_eq!(saved.outputs().count(), 0, "{why}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "{why}");
    }
}

#[test]
fn real_media_is_accepted_inline_and_on_disk() {
    for (name, key, cap, body, media) in [
        ("a.png", "images", "image.generate", PNG, "image/png"),
        ("a.jpg", "images", "image.generate", JPEG, "image/jpeg"),
        ("a.mp4", "videos", "video.generate", MP4, "video/mp4"),
    ] {
        let server = output_server(name, key, body.to_vec());
        let inline = finish(&server, base(&server), cap);
        assert_eq!(inline.state, JobState::Succeeded, "{name}");
        assert!(
            matches!(&inline.outputs().next().unwrap().payload,
            Payload::Inline { media_type, data }
                if media_type.as_str() == media && data == body),
            "{name}"
        );

        let dir = tempfile::tempdir().unwrap();
        let server = output_server(name, key, body.to_vec());
        let saved = finish(&server, base(&server).with_output_dir(dir.path()), cap);
        assert_eq!(saved.state, JobState::Succeeded, "{name}");
        let Payload::Handle(handle) = &saved.outputs().next().unwrap().payload else {
            panic!("{name}: expected a file handle");
        };
        assert_eq!(handle.media_type.as_str(), media);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

// ---- inputs ----

fn inpaint_workflow() -> Workflow {
    Workflow::import_inpaint(
        "repair",
        json!({
            "1":{"class_type":"CLIPTextEncode","inputs":{"text":"old"}},
            "2":{"class_type":"LoadImage","inputs":{"image":"old-source.png"}},
            "3":{"class_type":"LoadImageMask","inputs":{"image":"old-mask.png","channel":"red"}},
            "4":{"class_type":"SaveOutput","inputs":{"data":["5",0]}},
            "5":{"class_type":"Inpaint","inputs":{"image":["2",0],"mask":["3",0],"prompt":["1",0]}}
        }),
        Binding::new("1", "CLIPTextEncode", "text"),
        "4",
        Binding::new("2", "LoadImage", "image"),
        Binding::new("3", "LoadImageMask", "image"),
    )
    .unwrap()
}

fn inpaint_server() -> Server {
    Server::start(|r| {
        match r.path.as_str() {
        "/object_info" => Reply::Json(
            200,
            json!({
                "CLIPTextEncode":{"input":{"required":{"text":["STRING",{}]}}},
                "LoadImage":{"input":{"required":{"image":[[],{"image_upload":true}]}}},
                "LoadImageMask":{"input":{"required":{"image":[[],{"image_upload":true}],"channel":[["red","alpha"],{}]}}},
                "Inpaint":{"input":{"required":{"image":["IMAGE",{}],"mask":["MASK",{}],"prompt":["CONDITIONING",{}]}}},
                "SaveOutput":{"input":{"required":{"data":["IMAGE",{}]}}}
            })
            .to_string(),
        ),
        "/upload/image" => {
            let name = if r.body_text().contains("craft-source-") {
                "returned-source.png"
            } else {
                "returned-mask.png"
            };
            Reply::Json(
                200,
                json!({"name":name,"subfolder":"","type":"input"}).to_string(),
            )
        }
        "/prompt" => Reply::Json(200, r#"{"prompt_id":"inpaint-job"}"#.into()),
        _ => Reply::Json(404, "{}".into()),
    }
    })
}

fn uploads(server: &Server) -> usize {
    server
        .recorded()
        .iter()
        .filter(|r| r.path == "/upload/image")
        .count()
}

fn queued(server: &Server) -> bool {
    server.recorded().iter().any(|r| r.path == "/prompt")
}

fn inpaint(source: Payload, mask: Payload) -> Request {
    Request::new(CapabilityId::parse("image.inpaint").unwrap())
        .with_input(InputRole::Prompt, Payload::text("a lighthouse"))
        .with_input(InputRole::Source, source)
        .with_input(InputRole::Mask, mask)
}

fn provider(server: &Server, config: impl FnOnce(ComfyConfig) -> ComfyConfig) -> ComfyProvider {
    ComfyProvider::new(config(base(server)).with_workflow(inpaint_workflow())).unwrap()
}

fn submit(
    p: &ComfyProvider,
    request: &Request,
) -> Result<craft_ai_core::JobId, craft_ai_core::AiError> {
    p.submit(&ModelId::new("repair").unwrap(), request)
}

fn file_handle(path: &std::path::Path, media: &str) -> Payload {
    let uri = url::Url::from_file_path(path).unwrap().to_string();
    Payload::handle(&uri, media, None).unwrap()
}

fn write(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn a_local_file_handle_is_read_and_uploaded_like_inline_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let source = write(&dir, "source.jpg", JPEG);
    let mask = write(&dir, "mask.png", PNG);
    let server = inpaint_server();
    let p = provider(&server, |c| c);
    let request = inpaint(
        file_handle(&source, "image/jpeg"),
        file_handle(&mask, "image/png"),
    );
    submit(&p, &request).unwrap();

    let sent: Vec<_> = server
        .recorded()
        .into_iter()
        .filter(|r| r.path == "/upload/image")
        .collect();
    assert_eq!(sent.len(), 2);
    let has = |body: &[u8], needle: &[u8]| body.windows(needle.len()).any(|w| w == needle);
    assert!(has(&sent[0].body, JPEG), "the source bytes were sent");
    assert!(has(&sent[0].body, b"image/jpeg"));
    assert!(has(&sent[1].body, PNG), "the mask bytes were sent");
    let graph = &server
        .recorded()
        .into_iter()
        .find(|r| r.path == "/prompt")
        .unwrap()
        .body_json()["prompt"];
    assert_eq!(graph["2"]["inputs"]["image"], "returned-source.png");
    assert_eq!(graph["3"]["inputs"]["image"], "returned-mask.png");
}

#[test]
fn an_input_that_is_not_the_image_it_claims_is_never_uploaded() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = write(&dir, "notes.png", b"the password is hunter2");
    let wrong_kind = write(&dir, "photo.png", JPEG);
    let empty = write(&dir, "empty.png", b"");
    let cases: Vec<(&str, Payload)> = vec![
        (
            "a text file declared as png",
            file_handle(&secrets, "image/png"),
        ),
        (
            "jpeg bytes declared as png",
            file_handle(&wrong_kind, "image/png"),
        ),
        ("an empty file", file_handle(&empty, "image/png")),
        (
            "inline text declared as png",
            Payload::inline("image/png", b"the password is hunter2".to_vec()).unwrap(),
        ),
        (
            "inline html declared as jpeg",
            Payload::inline("image/jpeg", b"<html></html>".to_vec()).unwrap(),
        ),
        (
            "a format ComfyUI would receive mislabelled",
            Payload::inline("image/webp", b"RIFF\0\0\0\0WEBPVP8 ".to_vec()).unwrap(),
        ),
    ];
    for (why, bad) in cases {
        for swap in [false, true] {
            let server = inpaint_server();
            let p = provider(&server, |c| c);
            let ok = Payload::inline("image/png", PNG.to_vec()).unwrap();
            let request = if swap {
                inpaint(ok, bad.clone())
            } else {
                inpaint(bad.clone(), ok)
            };
            let error = submit(&p, &request).unwrap_err();
            assert!(
                matches!(
                    error.kind,
                    ErrorKind::InvalidRequest | ErrorKind::Unsupported
                ),
                "{why}: {error:?}"
            );
            assert!(!queued(&server), "{why}");
            assert!(
                !format!("{error:?}").contains("hunter2"),
                "{why}: the file content is not repeated"
            );
            assert_eq!(
                uploads(&server),
                0,
                "{why}: nothing is sent if either input is bad"
            );
        }
    }
}

#[test]
fn an_input_over_the_limit_is_refused_without_being_sent() {
    let dir = tempfile::tempdir().unwrap();
    let mut big = PNG.to_vec();
    big.resize(5000, 7);
    let file = write(&dir, "big.png", &big);
    let good = Payload::inline("image/png", PNG.to_vec()).unwrap();
    for (why, over) in [
        ("file", file_handle(&file, "image/png")),
        ("inline", Payload::inline("image/png", big.clone()).unwrap()),
    ] {
        let server = inpaint_server();
        let p = provider(&server, |c| c.with_max_upload_bytes(4096));
        let error = submit(&p, &inpaint(over, good.clone())).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidRequest, "{why}");
        assert_eq!(uploads(&server), 0, "{why}");
        assert!(!queued(&server), "{why}");
    }
    // The same file passes under a roomier limit, so the limit is what stopped it.
    let server = inpaint_server();
    let p = provider(&server, |c| c.with_max_upload_bytes(8192));
    submit(&p, &inpaint(file_handle(&file, "image/png"), good)).unwrap();
}

#[test]
fn only_plain_local_files_are_read() {
    let dir = tempfile::tempdir().unwrap();
    let good = Payload::inline("image/png", PNG.to_vec()).unwrap();
    let folder = dir.path().join("folder.png");
    std::fs::create_dir(&folder).unwrap();
    // Each case names the kind and the words of the refusal, so a check that went missing cannot
    // hide behind another check that happens to refuse the same input.
    let cases: Vec<(&str, Payload, ErrorKind, &str)> = vec![
        (
            "a web address",
            Payload::handle("https://example.invalid/a.png", "image/png", None).unwrap(),
            ErrorKind::Unsupported,
            "local files only",
        ),
        (
            "a file on another machine",
            Payload::handle("file://otherhost/share/a.png", "image/png", None).unwrap(),
            ErrorKind::InvalidRequest,
            "on this machine",
        ),
        (
            "a folder",
            file_handle(&folder, "image/png"),
            ErrorKind::InvalidRequest,
            "could not be read",
        ),
        (
            "a file that is not there",
            file_handle(&dir.path().join("gone.png"), "image/png"),
            ErrorKind::InvalidRequest,
            "could not be read",
        ),
        (
            "an address that is not a file path",
            Payload::handle("not a uri at all", "image/png", None).unwrap(),
            ErrorKind::InvalidRequest,
            "could not be read",
        ),
    ];
    for (why, bad, kind, words) in cases {
        let server = inpaint_server();
        let p = provider(&server, |c| c);
        let error = submit(&p, &inpaint(bad, good.clone())).unwrap_err();
        assert_eq!(error.kind, kind, "{why}: {error:?}");
        assert!(error.message.contains(words), "{why}: {error:?}");
        assert_eq!(uploads(&server), 0, "{why}");
        assert!(!queued(&server), "{why}");
        let shown = format!("{error:?}");
        assert!(
            !shown.contains(&dir.path().to_string_lossy().to_string()),
            "{why}: the error does not show where the file was"
        );
    }
}

#[test]
fn a_mask_must_be_a_png_even_when_it_is_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let jpeg = write(&dir, "mask.jpg", JPEG);
    let server = inpaint_server();
    let p = provider(&server, |c| c);
    let source = Payload::inline("image/png", PNG.to_vec()).unwrap();
    let error = submit(&p, &inpaint(source, file_handle(&jpeg, "image/jpeg"))).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidRequest);
    assert!(error.message.contains("needs a PNG"), "{error:?}");
    assert_eq!(uploads(&server), 0);
}

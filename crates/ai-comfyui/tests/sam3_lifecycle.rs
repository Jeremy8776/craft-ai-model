//! Session lifecycle: results are stamped with the revision of the image they were computed from
//! so an app can drop a stale one, and cancelling a request touches only that request's prompt.
mod sam3_support;
use craft_ai_comfyui::{Sam3Session, sam3};
use craft_ai_core::ErrorKind;
use sam3_support::{Reply, Server, mask, server_nodes, server_with, sized_mask, two_masks};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const NODES: &[&str] = &[
    "PreviewAny",
    "StringConcatenate",
    "SAM3Segmentation",
    "SAM3PointCollector",
];

fn text(value: &str) -> Value {
    json!({"text": [format!("{{nonce}}\n{value}")]})
}

fn candidate_server() -> Server {
    server_nodes(
        json!([{"filename": "a.png", "type": "output", "subfolder": ""}]),
        vec![("a.png", mask(&[255, 0]))],
        json!({"8": text("[0.7]")}),
        NODES,
    )
}

fn session(server: &Server, dir: &tempfile::TempDir) -> Sam3Session {
    Sam3Session::new(server.url.trim_end_matches("/v1"))
        .unwrap()
        .with_lock_dir(dir.path().to_path_buf())
}

fn click() -> sam3::Prompt {
    sam3::Prompt::point(0.5, 0.5)
}

#[test]
fn nothing_is_current_before_an_image_is_prepared() {
    let dir = tempfile::tempdir().unwrap();
    let s = session(&candidate_server(), &dir);
    assert_eq!(s.revision().get(), 0);
    assert!(!s.is_current(s.revision()));
}

#[test]
fn each_prepare_numbers_the_image_and_results_carry_the_number() {
    let dir = tempfile::tempdir().unwrap();
    let server = candidate_server();
    let mut s = session(&server, &dir);
    let first = s.prepare(&mask(&[255, 255])).unwrap();
    assert_eq!(s.revision(), first);
    assert!(s.is_current(first));
    let from_first = s.candidates(&click()).unwrap();
    assert_eq!(from_first.revision(), first);
    let second = s.prepare(&mask(&[255, 255])).unwrap();
    assert!(second > first);
    assert!(!s.is_current(first), "the old image is no longer current");
    assert!(s.is_current(second));
    assert_eq!(s.candidates(&click()).unwrap().revision(), second);
    assert_eq!(
        from_first.revision(),
        first,
        "an old result keeps its own number"
    );
}

#[test]
fn grounded_instances_carry_the_revision_too() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_with(
        two_masks(),
        vec![
            ("left.png", mask(&[255, 0])),
            ("right.png", mask(&[0, 255])),
        ],
        json!({"8": text("[[0.0,0.0,1.0,1.0],[1.0,0.0,2.0,1.0]]"), "9": text("[0.9,0.5]")}),
        true,
    );
    let mut s = session(&server, &dir);
    let rev = s.prepare(&mask(&[255, 255])).unwrap();
    assert_eq!(s.ground_instances("eye").unwrap().revision(), rev);
}

#[test]
fn a_failed_prepare_or_a_release_makes_every_older_result_stale() {
    let dir = tempfile::tempdir().unwrap();
    let server = candidate_server();
    let mut s = session(&server, &dir);
    let first = s.prepare(&mask(&[255, 255])).unwrap();
    assert!(s.prepare(b"not a png").is_err());
    assert!(!s.is_current(first));
    assert!(
        !s.is_current(s.revision()),
        "an unprepared session has nothing current"
    );
    let second = s.prepare(&mask(&[255, 255])).unwrap();
    assert!(second > first);
    s.release();
    assert!(!s.is_current(second));
    assert!(s.revision() > second);
}

/// A ComfyUI whose first prompt (the prepare) completes and whose later prompts never do.
fn stuck_server(queue_status: u16) -> Server {
    let prompts = AtomicUsize::new(0);
    Server::start(move |r| match r.path.as_str() {
        "/object_info" => {
            let mut nodes = json!({"LoadImage":{},"LoadSAM3Model":{},"SAM3InteractiveCollector":{},
                "MaskToImage":{},"SaveImage":{}});
            for name in NODES {
                nodes[*name] = json!({});
            }
            Reply::Json(200, nodes.to_string())
        }
        "/upload/image" => Reply::Json(
            200,
            json!({"name":"source.png","type":"input","subfolder":""}).to_string(),
        ),
        "/prompt" => {
            let id = if prompts.fetch_add(1, Ordering::SeqCst) == 0 {
                "prep-1"
            } else {
                "stuck-1"
            };
            Reply::Json(200, json!({"prompt_id": id}).to_string())
        }
        "/history/prep-1" => Reply::Json(
            200,
            json!({"prep-1":{"status":{"completed":true},"outputs":{}}}).to_string(),
        ),
        "/history/stuck-1" => Reply::Json(200, "{}".into()),
        "/queue" | "/interrupt" => Reply::Json(queue_status, String::new()),
        _ => Reply::Json(404, "{}".into()),
    })
}

fn posted(server: &Server, path: &str) -> Vec<Value> {
    server
        .recorded()
        .into_iter()
        .filter(|r| r.path == path)
        .map(|r| r.body_json())
        .collect()
}

/// Run `candidates` on a thread, cancel it after a moment, and return the error and how long
/// the cancellation took to be honoured.
fn cancel_a_running_request(s: &Sam3Session) -> (craft_ai_core::AiError, Duration) {
    let canceller = s.canceller();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| s.candidates(&click()));
        std::thread::sleep(Duration::from_millis(400));
        let asked = Instant::now();
        canceller.cancel();
        let err = worker.join().unwrap().unwrap_err();
        (err, asked.elapsed())
    })
}

#[test]
fn cancelling_stops_the_wait_and_removes_only_its_own_pending_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let server = stuck_server(200);
    let mut s = session(&server, &dir);
    s.prepare(&sized_mask(2, 1, &[255, 255])).unwrap();
    let (err, took) = cancel_a_running_request(&s);
    assert_eq!(err.kind, ErrorKind::Cancelled);
    assert!(took < Duration::from_secs(3), "{took:?}");
    assert_eq!(
        posted(&server, "/queue"),
        vec![json!({"delete": ["stuck-1"]})]
    );
    assert!(
        posted(&server, "/interrupt").is_empty(),
        "no interrupt unless the server is confirmed to scope it to a prompt"
    );
}

#[test]
fn with_a_confirmed_server_the_interrupt_names_only_its_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let server = stuck_server(200);
    let mut s = session(&server, &dir).with_targeted_interrupt(true);
    s.prepare(&sized_mask(2, 1, &[255, 255])).unwrap();
    let (err, _) = cancel_a_running_request(&s);
    assert_eq!(err.kind, ErrorKind::Cancelled);
    assert_eq!(
        posted(&server, "/interrupt"),
        vec![json!({"prompt_id": "stuck-1"})]
    );
    let queue = posted(&server, "/queue");
    assert!(queue.iter().all(|b| b.get("clear").is_none()), "{queue:?}");
}

#[test]
fn a_failed_cleanup_never_hides_the_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let server = stuck_server(500);
    let mut s = session(&server, &dir).with_targeted_interrupt(true);
    s.prepare(&sized_mask(2, 1, &[255, 255])).unwrap();
    let (err, _) = cancel_a_running_request(&s);
    assert_eq!(err.kind, ErrorKind::Cancelled);
}

#[test]
fn cancelling_while_idle_does_not_cancel_later_work() {
    let dir = tempfile::tempdir().unwrap();
    let server = candidate_server();
    let mut s = session(&server, &dir);
    s.prepare(&mask(&[255, 255])).unwrap();
    s.canceller().cancel();
    assert!(s.candidates(&click()).is_ok());
}

#[test]
fn one_sessions_canceller_does_not_cancel_another_sessions_request() {
    let dir = tempfile::tempdir().unwrap();
    let (one, two) = (stuck_server(200), stuck_server(200));
    let mut a = session(&one, &dir);
    let mut b = session(&two, &dir);
    a.prepare(&sized_mask(2, 1, &[255, 255])).unwrap();
    b.prepare(&sized_mask(2, 1, &[255, 255])).unwrap();
    let (cancel_a, cancel_b) = (a.canceller(), b.canceller());
    std::thread::scope(|scope| {
        let ra = scope.spawn(|| a.candidates(&click()));
        let rb = scope.spawn(|| b.candidates(&click()));
        std::thread::sleep(Duration::from_millis(400));
        cancel_a.cancel();
        let a_err = ra.join().unwrap().unwrap_err();
        assert_eq!(a_err.kind, ErrorKind::Cancelled);
        std::thread::sleep(Duration::from_millis(400));
        assert!(!rb.is_finished(), "B was not cancelled by A's canceller");
        assert!(
            posted(&two, "/queue").is_empty(),
            "B's prompt was left alone"
        );
        cancel_b.cancel();
        assert_eq!(rb.join().unwrap().unwrap_err().kind, ErrorKind::Cancelled);
    });
}

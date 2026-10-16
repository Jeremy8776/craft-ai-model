//! Live check of bounded session ownership and scoped cancellation on the installed ComfyUI.
//! Run through `run_sessions_live.py`. It uses the hover cache slots `craft_ai_sam3_0..3`, never
//! the older shared `craft_ai_sam3` id, so a running PhotoCraft build is not disturbed.
use craft_ai_comfyui::{Sam3Session, sam3};
use craft_ai_core::ErrorKind;
use std::time::{Duration, Instant};

fn source() -> Vec<u8> {
    let image = std::env::var("CRAFT_TEST_SAM3_IMAGE").expect("set the public test image path");
    std::fs::read(image).unwrap()
}

/// The same photograph mirrored left to right: a genuinely different prepared image.
fn mirrored(png_bytes: &[u8]) -> Vec<u8> {
    let mut reader = png::Decoder::new(png_bytes).read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut pixels).unwrap();
    let (w, h) = (frame.width as usize, frame.height as usize);
    let bytes_per_pixel = frame.buffer_size() / (w * h);
    let mut flipped = vec![0u8; frame.buffer_size()];
    for y in 0..h {
        for x in 0..w {
            let from = (y * w + x) * bytes_per_pixel;
            let to = (y * w + (w - 1 - x)) * bytes_per_pixel;
            flipped[to..to + bytes_per_pixel]
                .copy_from_slice(&pixels[from..from + bytes_per_pixel]);
        }
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, frame.width, frame.height);
    encoder.set_color(frame.color_type);
    encoder.set_depth(frame.bit_depth);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&flipped)
        .unwrap();
    out
}

fn open(url: &str) -> Sam3Session {
    let s = Sam3Session::new(url).unwrap();
    s.verify_server().unwrap();
    s
}

#[test]
#[ignore = "requires the existing headless server and public-domain astronaut fixture"]
fn two_sessions_with_different_images_do_not_replace_each_others_hover() {
    let url = std::env::var("CRAFT_TEST_COMFYUI_URL").expect("set the test URL");
    let (original, flipped) = (source(), mirrored(&source()));
    let mut a = open(&url);
    let rev_a = a.prepare(&original).unwrap();
    // The face centre in the original; in the mirrored image the same pixel is background-ish.
    let before = a.preview(220.0, 110.0).unwrap();
    let mut b = open(&url);
    let rev_b = b.prepare(&flipped).unwrap();
    assert_ne!(a.slot(), b.slot(), "two images, two slots");
    // A's hover is still about A's image after B prepared; with one shared slot it would show B.
    let after = a.preview(220.0, 110.0).unwrap();
    assert_eq!(before, after, "A's hover changed after B prepared");
    let from_b = b.preview(220.0, 110.0).unwrap();
    assert_ne!(after, from_b, "B's hover must reflect B's own image");
    let exact_a = a.commit(220.0, 110.0).unwrap();
    let exact_b = b.commit(220.0, 110.0).unwrap();
    assert_ne!(exact_a, exact_b);
    // Committing runs the collector again for the same slot, so A's hover is still its own.
    assert_eq!(a.preview(220.0, 110.0).unwrap(), before);
    assert!(a.is_current(rev_a) && b.is_current(rev_b));
    println!(
        "LIVE SESSIONS: slots {:?}/{:?}, A hover stable ({} bytes), B differs ({} bytes)",
        a.slot(),
        b.slot(),
        before.len(),
        from_b.len()
    );
    // Releasing B frees its slot for the next image, and A is untouched.
    let b_slot = b.slot();
    b.release();
    assert!(!b.is_current(rev_b));
    let mut c = open(&url);
    c.prepare(&original).unwrap();
    assert_eq!(c.slot(), b_slot);
    assert_eq!(a.preview(220.0, 110.0).unwrap(), before);
}

#[test]
#[ignore = "requires the existing headless server and public-domain astronaut fixture"]
fn cancelling_one_request_leaves_a_concurrent_unrelated_job_running() {
    let url = std::env::var("CRAFT_TEST_COMFYUI_URL").expect("set the test URL");
    let image = source();
    let mut keeper = open(&url);
    keeper.prepare(&image).unwrap();
    let mut victim = open(&url).with_targeted_interrupt(true);
    victim.prepare(&image).unwrap();
    let click = sam3::Prompt::point(220.0, 110.0);
    let cancel = victim.canceller();
    let (kept, cancelled, took) = std::thread::scope(|scope| {
        // The unrelated job is a deliberately heavy request: ten refinement passes.
        let keep = scope.spawn(|| keeper.candidates_refined(&click, 10));
        std::thread::sleep(Duration::from_millis(100));
        let gone = scope.spawn(|| victim.candidates_refined(&click, 10));
        std::thread::sleep(Duration::from_millis(150));
        let asked = Instant::now();
        cancel.cancel();
        let cancelled = gone.join().unwrap();
        (keep.join().unwrap(), cancelled, asked.elapsed())
    });
    let err = cancelled.expect_err("the cancelled request must not return a result");
    assert_eq!(err.kind, ErrorKind::Cancelled);
    assert!(took < Duration::from_secs(5), "{took:?}");
    let kept = kept.expect("the unrelated job must finish untouched");
    assert_eq!(kept.candidates().len(), 3);
    // The cancelled session is still usable afterwards.
    assert!(victim.candidates(&click).is_ok());
    println!("LIVE CANCEL: cancelled in {took:?}, unrelated job finished with 3 candidates");
}

#[test]
#[ignore = "requires the existing headless server and public-domain astronaut fixture"]
fn every_slot_can_be_held_on_the_real_server_and_a_fifth_image_is_refused() {
    let url = std::env::var("CRAFT_TEST_COMFYUI_URL").expect("set the test URL");
    let image = source();
    let mut held = Vec::new();
    for _ in 0..4 {
        let mut s = open(&url);
        s.prepare(&image).unwrap();
        held.push(s);
    }
    let mut slots: Vec<_> = held.iter().map(|s| s.slot().unwrap()).collect();
    slots.sort_unstable();
    assert_eq!(slots, vec![0, 1, 2, 3]);
    let mut fifth = open(&url);
    let err = fifth.prepare(&image).unwrap_err();
    assert!(err.message.contains("slots"), "{}", err.message);
    // Every held slot still serves its own hover on the real server.
    for s in &held {
        assert!(s.preview(220.0, 110.0).unwrap().starts_with(&[0xff, 0xd8]));
    }
    println!("LIVE SLOTS: all four held, fifth refused: {}", err.message);
    // Dropping them frees the locks (the server keeps its cached entries).
    drop(held);
    let mut again = open(&url);
    again.prepare(&image).unwrap();
    assert_eq!(again.slot(), Some(0));
}

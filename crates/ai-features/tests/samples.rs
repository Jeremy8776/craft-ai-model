use craft_ai_core::{InputRole, Payload};
use craft_ai_features::{builtin, sample_is_costly, sample_request};

fn decode(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).unwrap();
    (info.width, info.height, buf[..info.buffer_size()].to_vec())
}

fn inline(request: &craft_ai_core::Request, role: &InputRole) -> Vec<u8> {
    match request
        .inputs
        .iter()
        .find(|i| &i.role == role)
        .map(|i| &i.payload)
    {
        Some(Payload::Inline { data, .. }) => data.clone(),
        other => panic!("no inline {role:?}: {other:?}"),
    }
}

#[test]
fn the_sample_pictures_are_real_pngs_with_the_shapes_they_claim() {
    let fill = builtin()
        .into_iter()
        .find(|f| f.id == "generative-fill")
        .unwrap();
    let request = sample_request(&fill).unwrap();
    let (w, h, pixels) = decode(&inline(&request, &InputRole::Source));
    assert_eq!((w, h), (64, 64));
    assert_eq!(pixels[32 * 64 + 32], 90, "the disc is in the middle");
    assert_eq!(pixels[0], 255, "white around it");
    let (_, _, mask) = decode(&inline(&request, &InputRole::Mask));
    assert_eq!(mask[32 * 64 + 32], 0);
    assert_eq!(mask[0], 255);
}

#[test]
fn each_sample_matches_its_tool_and_validates_against_its_role() {
    let mut with_samples = 0;
    for f in builtin() {
        let Some(request) = sample_request(&f) else {
            continue;
        };
        with_samples += 1;
        assert_eq!(request.capability, f.capability, "{}", f.id);
        let role = f.role().unwrap();
        for input in &role.inputs {
            // Remove has no prompt in its sample; every other input the role lists is sent.
            if f.id == "remove-object" && *input == InputRole::Prompt {
                continue;
            }
            assert!(request.count(input) >= 1, "{} sends no {input:?}", f.id);
        }
    }
    assert_eq!(
        with_samples, 5,
        "select, remove, fill, image and video have samples; expand and extend do not"
    );
}

#[test]
fn only_video_is_flagged_as_costly() {
    for f in builtin() {
        assert_eq!(
            sample_is_costly(&f),
            f.capability.as_str().starts_with("video."),
            "{}",
            f.id
        );
    }
}

#[test]
fn a_tool_with_no_adapter_has_no_sample() {
    for id in ["generative-expand", "extend-sequence"] {
        let f = builtin().into_iter().find(|f| f.id == id).unwrap();
        assert!(sample_request(&f).is_none(), "{id}");
    }
}

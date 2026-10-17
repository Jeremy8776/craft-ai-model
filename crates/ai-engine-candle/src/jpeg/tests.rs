use jpeg_encoder::{ColorType, Encoder};

use super::decode_jpeg;

/// Left half red, right half blue.
fn two_halves(width: u16, height: u16) -> Vec<u8> {
    let mut pixels = Vec::new();
    for _ in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(if x < width / 2 {
                &[255, 0, 0]
            } else {
                &[0, 0, 255]
            });
        }
    }
    pixels
}

fn encode(pixels: &[u8], width: u16, height: u16, color: ColorType, exif: Option<u16>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = Encoder::new(&mut out, 100);
    if let Some(orientation) = exif {
        encoder
            .add_app_segment(1, &exif_block(orientation))
            .unwrap();
    }
    encoder.encode(pixels, width, height, color).unwrap();
    out
}

/// `Exif\0\0`, a little-endian TIFF header, and one entry: Orientation (tag 0x0112).
fn exif_block(orientation: u16) -> Vec<u8> {
    let mut b = b"Exif\0\0".to_vec();
    b.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00, 8, 0, 0, 0]); // II, 42, first IFD at 8
    b.extend_from_slice(&[1, 0]); // one entry
    b.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0]); // tag, SHORT, count 1
    b.extend_from_slice(&orientation.to_le_bytes());
    b.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // padding, no next IFD
    b
}

fn near(got: &[u8], want: [u8; 3]) -> bool {
    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) < 40)
}

fn px(rgb: &crate::image_io::Rgb, x: usize, y: usize) -> &[u8] {
    let at = (y * rgb.width + x) * 3;
    &rgb.bytes[at..at + 3]
}

#[test]
fn a_colour_jpeg_becomes_rgb_at_its_own_size() {
    let jpeg = encode(&two_halves(16, 8), 16, 8, ColorType::Rgb, None);
    let rgb = decode_jpeg(&jpeg).unwrap();
    assert_eq!(
        (rgb.width, rgb.height, rgb.bytes.len()),
        (16, 8, 16 * 8 * 3)
    );
    assert!(near(px(&rgb, 1, 4), [255, 0, 0]));
    assert!(near(px(&rgb, 14, 4), [0, 0, 255]));
}

#[test]
fn a_greyscale_jpeg_becomes_three_equal_channels() {
    let jpeg = encode(&[90; 8 * 8], 8, 8, ColorType::Luma, None);
    let rgb = decode_jpeg(&jpeg).unwrap();
    assert_eq!((rgb.width, rgb.height), (8, 8));
    let p = px(&rgb, 3, 3);
    assert!(
        p[0].abs_diff(90) < 6 && p[0] == p[1] && p[1] == p[2],
        "{p:?}"
    );
}

#[test]
fn the_exif_turn_is_applied_so_the_picture_is_upright() {
    // Orientation 6: the stored picture must be turned 90 degrees clockwise to be upright.
    // Stored 16x8 (red left, blue right) becomes 8x16 with red on top and blue below.
    let jpeg = encode(&two_halves(16, 8), 16, 8, ColorType::Rgb, Some(6));
    let rgb = decode_jpeg(&jpeg).unwrap();
    assert_eq!((rgb.width, rgb.height), (8, 16));
    assert!(near(px(&rgb, 4, 1), [255, 0, 0]));
    assert!(near(px(&rgb, 4, 14), [0, 0, 255]));
}

#[test]
fn orientation_one_and_no_exif_leave_the_picture_alone() {
    for exif in [None, Some(1)] {
        let jpeg = encode(&two_halves(16, 8), 16, 8, ColorType::Rgb, exif);
        let rgb = decode_jpeg(&jpeg).unwrap();
        assert_eq!((rgb.width, rgb.height), (16, 8), "{exif:?}");
    }
}

#[test]
fn a_nonsense_orientation_is_ignored_not_an_error() {
    let jpeg = encode(&two_halves(16, 8), 16, 8, ColorType::Rgb, Some(99));
    let rgb = decode_jpeg(&jpeg).unwrap();
    assert_eq!((rgb.width, rgb.height), (16, 8));
}

#[test]
fn a_huge_picture_is_refused_from_its_header_before_any_pixel_is_decoded() {
    let mut jpeg = encode(&two_halves(16, 8), 16, 8, ColorType::Rgb, None);
    // The start-of-frame marker (FF C0) is followed by length, precision, height, width.
    let at = jpeg.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap() + 5;
    jpeg[at..at + 2].copy_from_slice(&30_000u16.to_be_bytes());
    jpeg[at + 2..at + 4].copy_from_slice(&30_000u16.to_be_bytes());
    let e = decode_jpeg(&jpeg).err().unwrap();
    assert!(e.message.contains("too large"), "{}", e.message);
}

#[test]
fn damaged_and_cut_off_files_are_plain_errors_not_panics() {
    let jpeg = encode(&two_halves(64, 64), 64, 64, ColorType::Rgb, None);
    for cut in [2, 20, jpeg.len() / 2] {
        let e = decode_jpeg(&jpeg[..cut]);
        if let Err(e) = e {
            assert!(e.message.contains("could not be read"), "{}", e.message);
        }
    }
    let mut noisy = jpeg.clone();
    for byte in noisy.iter_mut().skip(40).step_by(3) {
        *byte ^= 0x5A;
    }
    let _ = decode_jpeg(&noisy); // any answer but a panic
}

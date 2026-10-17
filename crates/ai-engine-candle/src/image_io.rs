//! Pictures in and masks out: decoding a PNG or JPEG source to the RGB bytes the model reads, and
//! writing the mask as a grayscale PNG at the size of the (upright) source.

use std::io::Cursor;

use craft_ai_core::AiError;
use png::{BitDepth, ColorType, Transformations};

/// Pictures larger than this are refused before any pixel is read.
pub(crate) const MAX_PIXELS: u64 = 100_000_000;

/// What a person is told when a picture cannot be read at all.
pub(crate) const READ_FAILED: &str = "The picture could not be read. Use a PNG or a JPEG.";

pub(crate) fn too_large() -> AiError {
    AiError::invalid("The picture is too large to select from (over 100 megapixels).")
}

const PNG_START: [u8; 4] = [0x89, b'P', b'N', b'G'];
const JPEG_START: [u8; 3] = [0xFF, 0xD8, 0xFF];

/// Decodes a PNG or a JPEG, told apart by their first bytes rather than by the media type the
/// app sent, which is often just a guess from a file name.
pub(crate) fn decode_picture(data: &[u8]) -> Result<Rgb, AiError> {
    if data.starts_with(&PNG_START) {
        decode_png(data)
    } else if data.starts_with(&JPEG_START) {
        crate::jpeg::decode_jpeg(data)
    } else {
        Err(AiError::invalid(READ_FAILED))
    }
}

/// An 8-bit RGB picture, row by row.
pub(crate) struct Rgb {
    pub width: usize,
    pub height: usize,
    pub bytes: Vec<u8>,
}

/// Decodes a PNG of any colour type to RGB. Transparency is dropped.
pub(crate) fn decode_png(data: &[u8]) -> Result<Rgb, AiError> {
    let bad = |_| AiError::invalid(READ_FAILED);
    let mut decoder = png::Decoder::new(Cursor::new(data));
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(bad)?;
    let info = reader.info();
    let (width, height) = (u64::from(info.width), u64::from(info.height));
    if width == 0 || height == 0 || width * height > MAX_PIXELS {
        return Err(too_large());
    }
    let mut buffer = vec![0u8; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buffer).map_err(bad)?;
    let (width, height) = (frame.width as usize, frame.height as usize);
    let pixels = buffer.get(..frame.buffer_size()).unwrap_or(&[]);
    let channels = match frame.color_type {
        ColorType::Rgb => 3,
        ColorType::Rgba => 4,
        ColorType::Grayscale => 1,
        ColorType::GrayscaleAlpha => 2,
        ColorType::Indexed => {
            return Err(AiError::invalid(READ_FAILED));
        }
    };
    if frame.bit_depth != BitDepth::Eight || pixels.len() < width * height * channels {
        return Err(AiError::invalid(READ_FAILED));
    }
    let mut bytes = Vec::with_capacity(width * height * 3);
    for px in pixels.chunks_exact(channels).take(width * height) {
        match channels {
            1 | 2 => bytes.extend_from_slice(&[px[0], px[0], px[0]]),
            _ => bytes.extend_from_slice(&px[..3]),
        }
    }
    Ok(Rgb {
        width,
        height,
        bytes,
    })
}

/// Coverage 0..1 per pixel as an 8-bit grayscale PNG (white is selected).
pub(crate) fn encode_mask(alpha: &[f32], width: usize, height: usize) -> Result<Vec<u8>, AiError> {
    fn fail<E>(_: E) -> AiError {
        AiError::new(
            craft_ai_core::ErrorKind::Internal,
            "The mask could not be saved.",
        )
    }
    let gray: Vec<u8> = alpha
        .iter()
        .map(|a| (a.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(
        &mut out,
        u32::try_from(width).map_err(fail)?,
        u32::try_from(height).map_err(fail)?,
    );
    encoder.set_color(ColorType::Grayscale);
    encoder.set_depth(BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(fail)?;
    writer.write_image_data(&gray).map_err(fail)?;
    writer.finish().map_err(fail)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_of(color: ColorType, w: u32, h: u32, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(color);
        e.set_depth(BitDepth::Eight);
        let mut w = e.write_header().unwrap();
        w.write_image_data(data).unwrap();
        w.finish().unwrap();
        out
    }

    #[test]
    fn every_colour_type_becomes_rgb() {
        let rgb = decode_png(&png_of(ColorType::Rgb, 2, 1, &[1, 2, 3, 4, 5, 6])).unwrap();
        assert_eq!(
            (rgb.width, rgb.height, rgb.bytes),
            (2, 1, vec![1, 2, 3, 4, 5, 6])
        );
        let rgba = decode_png(&png_of(ColorType::Rgba, 1, 1, &[9, 8, 7, 0])).unwrap();
        assert_eq!(rgba.bytes, vec![9, 8, 7]);
        let gray = decode_png(&png_of(ColorType::Grayscale, 2, 1, &[5, 6])).unwrap();
        assert_eq!(gray.bytes, vec![5, 5, 5, 6, 6, 6]);
        let ga = decode_png(&png_of(ColorType::GrayscaleAlpha, 1, 1, &[5, 255])).unwrap();
        assert_eq!(ga.bytes, vec![5, 5, 5]);
    }

    #[test]
    fn something_that_is_not_a_picture_is_refused_in_plain_words() {
        for bytes in [&b"not a picture"[..], b"", b"GIF89a"] {
            let e = decode_picture(bytes).err().unwrap();
            assert!(e.message.contains("PNG or a JPEG"), "{}", e.message);
        }
    }

    #[test]
    fn a_picture_is_told_by_its_first_bytes_not_by_what_the_app_called_it() {
        let png = png_of(ColorType::Rgb, 1, 1, &[1, 2, 3]);
        assert_eq!(decode_picture(&png).unwrap().bytes, vec![1, 2, 3]);
        // A JPEG start with nothing behind it is a damaged JPEG, not "not a picture".
        let e = decode_picture(&[0xFF, 0xD8, 0xFF, 0xE0]).err().unwrap();
        assert!(e.message.contains("could not be read"));
    }

    #[test]
    fn a_mask_round_trips_at_the_size_of_the_source() {
        let png = encode_mask(&[0.0, 1.0, 0.5, 2.0], 2, 2).unwrap();
        let back = decode_png(&png).unwrap();
        assert_eq!((back.width, back.height), (2, 2));
        assert_eq!(
            back.bytes.iter().step_by(3).copied().collect::<Vec<_>>(),
            vec![0, 255, 128, 255]
        );
    }
}

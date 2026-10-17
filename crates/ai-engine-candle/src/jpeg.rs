//! Reading a JPEG source: decode it to RGB and turn it the way a person sees it.
//!
//! A phone or camera stores the pixels as the sensor read them and records in the EXIF block how
//! to turn the picture upright. Browsers, photo editors and OpenCV all apply that turn, so this
//! does too: the mask comes back in the upright picture, and a click normalised to the picture
//! lands where the person clicked. A picture with no EXIF block, or one that cannot be read, is
//! taken as it is.
//!
//! The decoder is `zune-jpeg` through the `image` crate, which handles baseline, progressive,
//! greyscale and CMYK files. Hostile input is bounded the same way as PNG: the size is read from
//! the header and refused before any pixel is decoded, and the decoder is given an allocation cap.

use std::io::Cursor;

use craft_ai_core::AiError;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, metadata::Orientation};

use crate::image_io::{MAX_PIXELS, READ_FAILED, Rgb, too_large};

/// The most memory the decoder may ask for. A 100 megapixel picture is 300 MB of RGB.
const MAX_ALLOC: u64 = 900 * 1024 * 1024;

pub(crate) fn decode_jpeg(data: &[u8]) -> Result<Rgb, AiError> {
    let bad = |_| AiError::invalid(READ_FAILED);
    let mut reader = ImageReader::with_format(Cursor::new(data), ImageFormat::Jpeg);
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(bad)?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(too_large());
    }
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut picture = DynamicImage::from_decoder(decoder).map_err(bad)?;
    picture.apply_orientation(orientation);
    let rgb = picture.into_rgb8();
    Ok(Rgb {
        width: rgb.width() as usize,
        height: rgb.height() as usize,
        bytes: rgb.into_raw(),
    })
}

#[cfg(test)]
mod tests;

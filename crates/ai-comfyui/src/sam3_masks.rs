//! Complete local SAM 3 mask batches. All descriptors are checked before downloads;
//! count, encoded bytes and decoded pixels are bounded, and any bad mask fails the batch.
use crate::http::Http;
use crate::{output, sam3_point::grayscale_mask};
use craft_ai_core::job::Event;
use craft_ai_core::payload::Payload;
use craft_ai_core::{AiError, ErrorKind};
use serde_json::{Value, json};

const MAX_MASKS: usize = 100;
const MAX_ENCODED_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DECODED_PIXELS: u64 = 64 * 1024 * 1024;

/// A complete validated batch of 8-bit grayscale PNG masks in server order.
/// Constructed only after every mask passes validation; no raw media appears in Debug.
pub struct Sam3MaskBatch {
    width: u32,
    height: u32,
    masks: Vec<Vec<u8>>,
}

impl Sam3MaskBatch {
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The independent PNG masks, never implicitly merged or filtered.
    pub fn masks(&self) -> &[Vec<u8>] {
        &self.masks
    }

    pub(crate) fn has_pixels(&self) -> Result<bool, AiError> {
        for mask in &self.masks {
            let mut reader = png::Decoder::new(mask.as_slice())
                .read_info()
                .map_err(|_| bad("SAM 3 returned an invalid mask"))?;
            let mut buf = vec![0; reader.output_buffer_size()];
            let frame = reader
                .next_frame(&mut buf)
                .map_err(|_| bad("SAM 3 mask pixels are invalid"))?;
            if buf
                .get(..frame.buffer_size())
                .is_some_and(|pixels| pixels.iter().any(|p| *p != 0))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Explicit union by pixelwise maximum, preserving soft mask coverage.
    pub fn union_png(&self) -> Result<Vec<u8>, AiError> {
        union_masks(
            self.width,
            self.height,
            self.masks.iter().map(Vec::as_slice),
        )
    }

    /// Take the validated parts: dimensions and the PNG masks in server order.
    pub(crate) fn into_parts(self) -> (u32, u32, Vec<Vec<u8>>) {
        (self.width, self.height, self.masks)
    }
}

/// Pixelwise-maximum union of validated grayscale PNG masks of identical size.
pub(crate) fn union_masks<'a>(
    width: u32,
    height: u32,
    masks: impl IntoIterator<Item = &'a [u8]>,
) -> Result<Vec<u8>, AiError> {
    let masks: Vec<&[u8]> = masks.into_iter().collect();
    if let [one] = masks.as_slice() {
        return Ok(one.to_vec());
    }
    let count = u64::from(width) * u64::from(height);
    let count = usize::try_from(count).map_err(|_| bad("SAM 3 mask dimensions are invalid"))?;
    let mut combined = vec![0u8; count];
    for mask in masks {
        let mut reader = png::Decoder::new(mask)
            .read_info()
            .map_err(|_| bad("SAM 3 returned an invalid mask"))?;
        let mut buf = vec![0; reader.output_buffer_size()];
        let frame = reader
            .next_frame(&mut buf)
            .map_err(|_| bad("SAM 3 mask pixels are invalid"))?;
        let pixels = buf
            .get(..frame.buffer_size())
            .filter(|pixels| pixels.len() == combined.len())
            .ok_or_else(|| bad("SAM 3 mask pixels do not match the source"))?;
        for (out, value) in combined.iter_mut().zip(pixels) {
            *out = (*out).max(*value);
        }
    }
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&combined))
        .map_err(|_| bad("SAM 3 union mask could not be encoded"))?;
    Ok(bytes)
}

pub(crate) fn fetch(
    http: &Http,
    node: &Value,
    width: u32,
    height: u32,
) -> Result<Sam3MaskBatch, AiError> {
    let files = node
        .get("images")
        .and_then(Value::as_array)
        .filter(|files| !files.is_empty())
        .ok_or_else(|| bad("SAM 3 returned no selection masks"))?;
    if files.len() > MAX_MASKS {
        return Err(bad("SAM 3 returned too many masks"));
    }
    let _checked_pixel_budget = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(files.len() as u64))
        .filter(|pixels| *pixels > 0 && *pixels <= MAX_DECODED_PIXELS)
        .ok_or_else(|| bad("SAM 3 batch exceeds the decoded pixel limit"))?;
    for file in files {
        validate_file(file)?;
    }
    let mut remaining = MAX_ENCODED_BYTES;
    let mut masks = Vec::with_capacity(files.len());
    for file in files {
        if remaining == 0 {
            return Err(bad("SAM 3 batch exceeds the encoded byte limit"));
        }
        let Event::Output(out) =
            output::fetch(http, &json!({"images":[file]}), false, None, remaining)?
        else {
            return Err(bad("SAM 3 returned no mask"));
        };
        let Payload::Inline { data, .. } = out.payload else {
            return Err(bad("SAM 3 mask was not inline"));
        };
        remaining = remaining
            .checked_sub(data.len() as u64)
            .ok_or_else(|| bad("SAM 3 batch exceeds the encoded byte limit"))?;
        // Check source dimensions before the conversion allocates a decoded image buffer.
        let reader = png::Decoder::new(data.as_slice())
            .read_info()
            .map_err(|_| bad("SAM 3 returned an invalid mask"))?;
        if reader.info().width != width || reader.info().height != height {
            return Err(bad("SAM 3 mask dimensions differ from the source image"));
        }
        let mask = grayscale_mask(&data)?;
        masks.push(mask);
    }
    Ok(Sam3MaskBatch {
        width,
        height,
        masks,
    })
}

fn validate_file(file: &Value) -> Result<(), AiError> {
    let name = file
        .get("filename")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("SAM 3 mask descriptor has no filename"))?;
    let subfolder = match file.get("subfolder") {
        None => "",
        Some(value) => value
            .as_str()
            .ok_or_else(|| bad("SAM 3 mask descriptor is unsafe"))?,
    };
    let safe = |s: &str| {
        !s.is_empty()
            && s.len() <= 255
            && !s.contains("..")
            && !s.contains(['/', '\\', ':'])
            && !s.chars().any(char::is_control)
    };
    if file.get("type").and_then(Value::as_str) != Some("output")
        || !safe(name)
        || (!subfolder.is_empty() && !safe(subfolder))
    {
        return Err(bad("SAM 3 mask descriptor is unsafe"));
    }
    if !name.to_ascii_lowercase().ends_with(".png") {
        return Err(bad("SAM 3 mask must be PNG"));
    }
    Ok(())
}

pub(crate) fn bad(message: &str) -> AiError {
    AiError::new(ErrorKind::Provider, message)
}

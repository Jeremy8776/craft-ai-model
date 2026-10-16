//! One local SAM 3 point-to-mask run through the installed ComfyUI nodes.
//! This is a job, not a hover session: every call uploads an image and queues a graph.
use crate::http::Http;
use crate::output;
use craft_ai_core::endpoint::Endpoint;
use craft_ai_core::job::Event;
use craft_ai_core::payload::Payload;
use craft_ai_core::{AiError, ErrorKind};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const REQUIRED: &[&str] = &[
    "LoadImage",
    "LoadSAM3Model",
    "SAM3CreatePoint",
    "SAM3CombinePoints",
    "SAM3Segmentation",
    "MaskToImage",
    "SaveImage",
];

pub struct Sam3Point {
    http: Http,
}

impl Sam3Point {
    /// Only connects to ComfyUI on this computer, never a URL with a path or query.
    pub fn new(base: &str) -> Result<Self, AiError> {
        let endpoint =
            Endpoint::parse(base).map_err(|_| AiError::invalid("invalid ComfyUI address"))?;
        if !endpoint.is_loopback() || base != endpoint.fingerprint() {
            return Err(AiError::invalid(
                "SAM 3 needs a loopback ComfyUI address without a path",
            ));
        }
        Ok(Self {
            http: Http::new(
                base.to_owned(),
                Duration::from_secs(3),
                Duration::from_secs(15),
            ),
        })
    }

    /// Segment the object at normalised image coordinates. Returns a validated grayscale PNG mask.
    /// This blocks on the ComfyUI job: callers must run it off the UI thread.
    pub fn segment(&self, image: &[u8], x: f32, y: f32) -> Result<Vec<u8>, AiError> {
        if !x.is_finite()
            || !y.is_finite()
            || !(0.0..=1.0).contains(&x)
            || !(0.0..=1.0).contains(&y)
        {
            return Err(AiError::invalid("SAM 3 point must be inside the image"));
        }
        if image.len() < 8
            || image.len() > 16 * 1024 * 1024
            || !image.starts_with(b"\x89PNG\r\n\x1a\n")
        {
            return Err(AiError::invalid("SAM 3 needs a PNG image under 16 MiB"));
        }
        let nodes = self.http.get_object_info()?;
        for class in REQUIRED {
            if nodes.get(*class).is_none() {
                return Err(AiError::unsupported(format!(
                    "ComfyUI is missing the {class} node"
                )));
            }
        }
        let filename = self.http.upload_image("sam3-source", "image/png", image)?;
        let graph = json!({
            "1":{"class_type":"LoadImage","inputs":{"image":filename}},
            "2":{"class_type":"LoadSAM3Model","inputs":{"precision":"auto","compile":false}},
            "3":{"class_type":"SAM3CreatePoint","inputs":{"x":x,"y":y,"is_foreground":true}},
            "4":{"class_type":"SAM3CombinePoints","inputs":{"point_1":["3",0]}},
            "5":{"class_type":"SAM3Segmentation","inputs":{
                "sam3_model_config":["2",0],"image":["1",0],"positive_points":["4",0],
                "refinement_iterations":0,"use_multimask":true,"output_best_mask":true
            }},
            "6":{"class_type":"MaskToImage","inputs":{"mask":["5",0]}},
            "7":{"class_type":"SaveImage","inputs":{"images":["6",0],"filename_prefix":"craft-sam3-mask"}}
        });
        let response = self.http.post("/prompt", &json!({"prompt":graph}))?;
        let id = response
            .get("prompt_id")
            .and_then(Value::as_str)
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 128
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
            .ok_or_else(|| {
                AiError::new(
                    ErrorKind::Provider,
                    "ComfyUI did not return a valid prompt id",
                )
            })?;
        let path = format!("/history/{id}");
        let deadline = Instant::now() + Duration::from_secs(240);
        loop {
            let history = self.http.get_json(&path)?;
            if let Some(record) = history.get(id) {
                if record.pointer("/status/status_str").and_then(Value::as_str) == Some("error") {
                    return Err(AiError::new(
                        ErrorKind::Provider,
                        "SAM 3 workflow failed in ComfyUI",
                    ));
                }
                if record.pointer("/status/completed").and_then(Value::as_bool) == Some(true) {
                    let node = record.pointer("/outputs/7").ok_or_else(|| {
                        AiError::new(ErrorKind::Provider, "SAM 3 returned no mask")
                    })?;
                    let event = output::fetch(&self.http, node, false, None, 32 * 1024 * 1024)?;
                    let Event::Output(output) = event else {
                        return Err(AiError::new(ErrorKind::Provider, "SAM 3 returned no mask"));
                    };
                    let Payload::Inline { media_type, data } = output.payload else {
                        return Err(AiError::new(
                            ErrorKind::Provider,
                            "SAM 3 mask was not inline",
                        ));
                    };
                    if media_type.as_str() != "image/png" {
                        return Err(AiError::new(ErrorKind::Provider, "SAM 3 mask was not PNG"));
                    }
                    return grayscale_mask(&data);
                }
            }
            if Instant::now() >= deadline {
                return Err(AiError::new(
                    ErrorKind::Network,
                    "SAM 3 timed out in ComfyUI",
                ));
            }
            std::thread::sleep(Duration::from_millis(400));
        }
    }
}

pub(crate) fn grayscale_mask(data: &[u8]) -> Result<Vec<u8>, AiError> {
    let mut decoder = png::Decoder::new(data);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|_| AiError::invalid("SAM 3 returned an invalid mask"))?;
    let info = reader.info();
    if info.width == 0
        || info.height == 0
        || info.width > 8192
        || info.height > 8192
        || u64::from(info.width) * u64::from(info.height) > 16_777_216
    {
        return Err(AiError::invalid("SAM 3 mask dimensions are invalid"));
    }
    if reader.output_buffer_size() > 64 * 1024 * 1024 {
        return Err(AiError::invalid("SAM 3 mask is too large"));
    }
    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader
        .next_frame(&mut buf)
        .map_err(|_| AiError::invalid("SAM 3 mask pixels are invalid"))?;
    let grey = match frame.color_type {
        png::ColorType::Grayscale => buf[..frame.buffer_size()].to_vec(),
        png::ColorType::Rgb => buf[..frame.buffer_size()]
            .as_chunks::<3>()
            .0
            .iter()
            .map(|rgb| {
                if rgb[0] == rgb[1] && rgb[1] == rgb[2] {
                    Some(rgb[0])
                } else {
                    None
                }
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| AiError::invalid("SAM 3 mask is not grayscale"))?,
        _ => {
            return Err(AiError::invalid(
                "SAM 3 mask has an unsupported colour type",
            ));
        }
    };
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, frame.width, frame.height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|_| AiError::invalid("SAM 3 mask could not be encoded"))?;
    writer
        .write_image_data(&grey)
        .map_err(|_| AiError::invalid("SAM 3 mask could not be encoded"))?;
    drop(writer);
    Ok(out)
}

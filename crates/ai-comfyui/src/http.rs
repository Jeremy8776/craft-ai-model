//! Local-only HTTP. Redirects are refused and replies are bounded before parsing.
use craft_ai_core::{AiError, ErrorKind};
use serde_json::Value;
use std::io::Read;
use std::time::Duration;
use ureq::{Body, http::Response};

pub(crate) struct Http {
    agent: ureq::Agent,
    base: String,
}
impl Http {
    pub fn new(base: String, connect_timeout: Duration, request_timeout: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .tls_config(craft_ai_http::tls_config())
            .http_status_as_error(false)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .timeout_connect(Some(connect_timeout))
            .timeout_global(Some(request_timeout))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            base,
        }
    }
    fn check(
        &self,
        result: Result<Response<Body>, ureq::Error>,
    ) -> Result<Response<Body>, AiError> {
        match result {
            Ok(resp) if resp.status().is_success() => Ok(resp),
            Ok(resp) => {
                let status = resp.status().as_u16();
                let kind = match status {
                    400 | 404 | 405 | 413 | 422 => ErrorKind::InvalidRequest,
                    401 | 403 => ErrorKind::Auth,
                    429 => ErrorKind::RateLimited,
                    _ => ErrorKind::Provider,
                };
                Err(AiError::new(kind, format!("ComfyUI answered {status}")))
            }
            Err(_) => Err(AiError::new(ErrorKind::Network, "could not reach ComfyUI")),
        }
    }
    fn read(&self, resp: Response<Body>, max: usize) -> Result<Vec<u8>, AiError> {
        let mut bytes = Vec::new();
        resp.into_body()
            .into_reader()
            .take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| AiError::new(ErrorKind::Network, "ComfyUI reply broke during reading"))?;
        if bytes.len() > max {
            return Err(AiError::new(
                ErrorKind::Provider,
                "ComfyUI reply is too large",
            ));
        }
        Ok(bytes)
    }
    pub fn get_json(&self, path: &str) -> Result<Value, AiError> {
        self.get_json_bounded(path, 1024 * 1024)
    }
    pub fn get_object_info(&self) -> Result<Value, AiError> {
        self.get_json_bounded("/object_info", 16 * 1024 * 1024)
    }
    fn get_json_bounded(&self, path: &str, max: usize) -> Result<Value, AiError> {
        let url = format!("{}{path}", self.base);
        let resp = self.check(self.agent.get(url).call())?;
        serde_json::from_slice(&self.read(resp, max)?)
            .map_err(|_| AiError::new(ErrorKind::Provider, "ComfyUI returned invalid JSON"))
    }

    /// Read at most the caller's remaining batch budget, plus one oversize sentinel byte.
    pub fn get_media_limited(&self, path: &str, max: u64) -> Result<Vec<u8>, AiError> {
        let max =
            usize::try_from(max).map_err(|_| AiError::invalid("ComfyUI media limit is invalid"))?;
        let url = format!("{}{path}", self.base);
        let resp = self.check(self.agent.get(url).call())?;
        self.read(resp, max)
    }
    /// Streams a bounded response into a temporary file owned by the caller. Redirects are off.
    pub fn get_media_to(
        &self,
        path: &str,
        writer: &mut impl std::io::Write,
        max: u64,
    ) -> Result<u64, AiError> {
        let url = format!("{}{path}", self.base);
        let resp = self.check(self.agent.get(url).call())?;
        let count = std::io::copy(&mut resp.into_body().into_reader().take(max + 1), writer)
            .map_err(|_| AiError::new(ErrorKind::Network, "ComfyUI media transfer failed"))?;
        if count > max {
            return Err(AiError::new(
                ErrorKind::Provider,
                "ComfyUI output exceeds the configured media limit",
            ));
        }
        Ok(count)
    }
    pub fn post(&self, path: &str, value: &Value) -> Result<Value, AiError> {
        let body = serde_json::to_vec(value)
            .map_err(|_| AiError::new(ErrorKind::Internal, "could not encode workflow"))?;
        let url = format!("{}{path}", self.base);
        let resp = self.check(
            self.agent
                .post(url)
                .content_type("application/json")
                .send(&body),
        )?;
        let bytes = self.read(resp, 1024 * 1024)?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| AiError::new(ErrorKind::Provider, "ComfyUI returned invalid JSON"))
    }
    pub fn upload_image(
        &self,
        role: &str,
        media_type: &str,
        data: &[u8],
    ) -> Result<String, AiError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let extension = if media_type == "image/jpeg" {
            "jpg"
        } else {
            "png"
        };
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let filename = format!(
            "craft-{role}-{}-{nonce}-{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            extension
        );
        let boundary = format!("craft-ai-{nonce}-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"type\"\r\n\r\ninput\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{filename}\"\r\nContent-Type: {media_type}\r\n\r\n").into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let resp = self.check(
            self.agent
                .post(format!("{}/upload/image", self.base))
                .header(
                    "Content-Type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .send(&body),
        )?;
        let reply: Value = serde_json::from_slice(&self.read(resp, 4096)?).map_err(|_| {
            AiError::new(
                ErrorKind::Provider,
                "ComfyUI returned an invalid upload reply",
            )
        })?;
        let name = reply.get("name").and_then(Value::as_str).ok_or_else(|| {
            AiError::new(
                ErrorKind::Provider,
                "ComfyUI did not name the uploaded image",
            )
        })?;
        if reply.get("type").and_then(Value::as_str) != Some("input")
            || reply.get("subfolder").and_then(Value::as_str) != Some("")
            || !safe_upload_name(name)
        {
            return Err(AiError::new(
                ErrorKind::Provider,
                "ComfyUI returned an unsafe upload location",
            ));
        }
        Ok(name.to_owned())
    }
}

pub(crate) fn safe_upload_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && !name.contains("..")
        && (name.ends_with(".png") || name.ends_with(".jpg"))
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b' ' | b'(' | b')')
        })
}

pub(crate) fn encode_query(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

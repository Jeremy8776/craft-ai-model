//! Source and mask images going up to ComfyUI.
//!
//! An image arrives inline or as a handle to a file on this machine. Either way it is read once,
//! bounded, and checked by its first bytes against the type it was declared as before anything is
//! sent: a text file, a web page or a different format dressed up as a PNG never leaves the
//! machine. Handles are limited to plain local files (no web address, no file on another machine),
//! and no error says where a file was or what it held.

use std::io::Read;

use craft_ai_core::{AiError, Payload};
use craft_ai_media::sniff;

pub(crate) const DEFAULT_MAX_UPLOAD_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) const HARD_MAX_UPLOAD_BYTES: u64 = 256 * 1024 * 1024;

/// An image that has been read and checked, ready to upload.
pub(crate) struct Image {
    pub media_type: String,
    pub data: Vec<u8>,
}

/// Reads the image in `payload`, refusing anything over `max` bytes or not the declared format.
pub(crate) fn read_image(payload: &Payload, max: u64) -> Result<Image, AiError> {
    let (declared, data) = match payload {
        Payload::Inline { media_type, data } => (media_type.as_str(), data.clone()),
        Payload::Handle(handle) => (handle.media_type.as_str(), read_file(&handle.uri, max)?),
        Payload::Text { .. } => return Err(AiError::invalid("an image is needed, not text")),
    };
    if u64::try_from(data.len()).unwrap_or(u64::MAX) > max {
        return Err(too_big());
    }
    let head = data.get(..sniff::HEAD_LEN).unwrap_or(&data);
    if sniff::media_type(head).map(|(media, _)| media) != Some(declared) {
        return Err(AiError::invalid(
            "an input is not the image it says it is, so nothing was sent",
        ));
    }
    Ok(Image {
        media_type: declared.to_owned(),
        data,
    })
}

fn too_big() -> AiError {
    AiError::invalid("an input image is larger than the upload limit, so nothing was sent")
}

fn unreadable() -> AiError {
    AiError::invalid("an input file could not be read")
}

/// The bytes of a plain file on this machine named by a `file:` address, at most `max` of them.
fn read_file(uri: &str, max: u64) -> Result<Vec<u8>, AiError> {
    let url = url::Url::parse(uri).map_err(|_| unreadable())?;
    if url.scheme() != "file" {
        return Err(AiError::unsupported(
            "ComfyUI uploads read local files only, not web addresses",
        ));
    }
    if !matches!(url.host_str(), None | Some("" | "localhost")) {
        // On Windows a host turns into a network share. Only this machine's files are read.
        return Err(AiError::invalid(
            "an input file must be on this machine, so nothing was sent",
        ));
    }
    let path = url.to_file_path().map_err(|_| unreadable())?;
    let file = std::fs::File::open(path).map_err(|_| unreadable())?;
    let meta = file.metadata().map_err(|_| unreadable())?;
    if !meta.is_file() {
        return Err(unreadable());
    }
    if meta.len() > max {
        return Err(too_big());
    }
    // The size above is only a first look: read at most one byte past the limit, so a file that
    // grows (or lies about its length) is still stopped.
    let data = read_bounded(file, max).map_err(|_| unreadable())?;
    Ok(data)
}

/// At most `max + 1` bytes of `reader`: one past the limit is enough to see it was crossed, and
/// a reader that never ends cannot fill memory.
fn read_bounded(reader: impl Read, max: u64) -> std::io::Result<Vec<u8>> {
    let mut data = Vec::new();
    reader.take(max.saturating_add(1)).read_to_end(&mut data)?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that claims nothing about its length and counts what was asked of it.
    struct Endless {
        read: u64,
    }

    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(4096);
            buf[..n].fill(7);
            self.read += n as u64;
            if self.read > 1_000_000 {
                return Err(std::io::Error::other("read far past any limit"));
            }
            Ok(n)
        }
    }

    #[test]
    fn a_reader_is_read_no_further_than_one_byte_past_the_limit() {
        let mut endless = Endless { read: 0 };
        let data = read_bounded(&mut endless, 100).unwrap();
        assert_eq!(data.len(), 101);
        assert!(
            endless.read <= 100 + 4096,
            "asked for {} bytes of an endless reader",
            endless.read
        );
    }

    #[test]
    fn a_file_over_the_limit_is_refused_by_its_length_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        let uri = url::Url::from_file_path(&path).unwrap().to_string();
        std::fs::write(&path, vec![1u8; 100]).unwrap();
        assert_eq!(read_file(&uri, 100).unwrap().len(), 100);
        std::fs::write(&path, vec![1u8; 101]).unwrap();
        // Refused here, before any of it is read, rather than later by the caller.
        assert!(read_file(&uri, 100).is_err());
    }
}

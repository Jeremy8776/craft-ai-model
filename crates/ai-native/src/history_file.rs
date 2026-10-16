//! [`HistoryFile`]: the shared request log on disk, one JSON line per record.
//!
//! Several apps append at once, so every write takes the same cross-process lock the shared store
//! uses. The file is capped: when a write would pass `max_bytes` the current file becomes
//! `jobs.1.jsonl` (replacing the previous one) and a new file starts, so the history holds between
//! one and two caps of lines. Reading is forgiving: a line that does not parse, including the
//! half line a crash leaves, is counted and skipped; it never stops the rest loading. Nothing here
//! panics, and callers are expected to treat an error as "history unavailable", never as a reason
//! to fail a job.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use craft_ai_core::HistoryRecord;

use crate::fsutil::{read_capped, with_lock};
use crate::paths;

/// Default cap for the live file, 4 MiB (roughly 10,000 jobs).
pub const DEFAULT_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Most one record line may take; a longer one is refused rather than written.
const MAX_LINE_BYTES: usize = 64 * 1024;

/// What a read found.
#[derive(Debug, Default)]
pub struct HistoryRead {
    /// Oldest first.
    pub records: Vec<HistoryRecord>,
    /// Lines that were not understood.
    pub skipped: usize,
}

#[derive(Clone, Debug)]
pub struct HistoryFile {
    path: PathBuf,
    max_bytes: u64,
}

impl HistoryFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    /// The standard location, or `None` if the OS reports no config directory.
    pub fn at_default_location() -> Option<Self> {
        paths::history_path().map(Self::new)
    }

    /// Replaces the cap (at least 1 KiB, so one record always fits).
    pub fn with_max_bytes(mut self, max: u64) -> Self {
        self.max_bytes = max.max(1024);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn rotated_path(&self) -> PathBuf {
        let stem = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "jobs".to_owned());
        let ext = self
            .path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        self.path.with_file_name(format!("{stem}.1{ext}"))
    }

    /// Appends one record. Safe to call from several processes at once.
    pub fn append(&self, record: &HistoryRecord) -> io::Result<()> {
        let mut line = serde_json::to_vec(record)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        if line.len() > MAX_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "history record is too large",
            ));
        }
        line.push(b'\n');
        with_lock(&self.path, || {
            let len = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
            if len > 0 && len + line.len() as u64 > self.max_bytes {
                fs::rename(&self.path, self.rotated_path())?;
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .open(&self.path)?;
            // A crash can leave a line with no newline. Starting on a fresh line keeps the next
            // record readable instead of gluing it to the damage.
            if ends_mid_line(&mut file)? {
                file.write_all(b"\n")?;
            }
            file.write_all(&line)?;
            file.flush()
        })
    }

    /// Reads the rotated file then the live one. A missing file is an empty history.
    pub fn read(&self) -> io::Result<HistoryRead> {
        let cap = usize::try_from(self.max_bytes)
            .unwrap_or(usize::MAX)
            .saturating_add(MAX_LINE_BYTES);
        let mut out = HistoryRead::default();
        for path in [self.rotated_path(), self.path.clone()] {
            if let Some(bytes) = read_capped(&path, cap)? {
                parse_lines(&bytes, &mut out);
            }
        }
        Ok(out)
    }

    /// Deletes the history (both files).
    pub fn clear(&self) -> io::Result<()> {
        with_lock(&self.path, || {
            for path in [self.rotated_path(), self.path.clone()] {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        })
    }
}

fn ends_mid_line(file: &mut fs::File) -> io::Result<bool> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(len - 1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    Ok(last[0] != b'\n')
}

fn parse_lines(bytes: &[u8], out: &mut HistoryRead) {
    for raw in bytes.split(|b| *b == b'\n') {
        let line = raw.trim_ascii();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<HistoryRecord>(line) {
            Ok(record) => out.records.push(record),
            Err(_) => out.skipped += 1,
        }
    }
}

#[cfg(test)]
mod tests;

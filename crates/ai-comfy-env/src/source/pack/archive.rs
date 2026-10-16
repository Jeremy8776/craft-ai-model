//! Unpacking a `.tar.gz` from GitHub, with the archive treated as hostile.
//!
//! The reader is small on purpose and refuses anything it does not need: links of any kind, device
//! files, a path that is not a plain relative path under one top folder, names Windows treats
//! specially, two names that differ only in case, a header whose checksum is wrong, and anything
//! past the limits (counted as the data is read, so a small download cannot expand without bound).
//! Files are created new, never over something; nothing is made executable; nothing is run.

use std::collections::{BTreeMap, btree_map::Entry};
use std::io::Read;
use std::path::Path;

use flate2::read::GzDecoder;

use super::{PackError, PackLimits, PackRefusal, unsafe_archive};

const BLOCK: usize = 512;
const MAX_EXTENDED: u64 = 64 * 1024;
const MAX_PATH_BYTES: usize = 240;
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

pub(in crate::source) struct Unpacked {
    pub files: u32,
    pub bytes: u64,
    /// Lower-case names of the files at the top of the pack.
    pub top_files: Vec<String>,
}

/// Unpacks `archive` into `dest/folder`. Everything refused is a [`PackError::Refused`]; a full
/// disk or similar is [`PackError::Failed`].
pub(in crate::source) fn unpack(
    archive: &[u8],
    limits: &PackLimits,
    dest: &Path,
    folder: &str,
) -> Result<Unpacked, PackError> {
    // The stream is bounded as well as counted, as a second wall against a decompression bomb.
    let budget = limits
        .max_expanded
        .saturating_add(u64::from(limits.max_entries).saturating_mul(2 * BLOCK as u64))
        .saturating_add(1 << 16);
    let mut input = GzDecoder::new(archive).take(budget);
    let root_dir = dest.join(folder);
    std::fs::create_dir_all(&root_dir).map_err(|_| io_failed())?;

    let mut top: Option<String> = None;
    let mut names = Names::default();
    let (mut files, mut entries, mut bytes) = (0u32, 0u32, 0u64);
    let mut top_files = Vec::new();
    let mut pending_path: Option<String> = None;

    loop {
        let mut head = [0u8; BLOCK];
        match read_full(&mut input, &mut head)? {
            0 => break,
            BLOCK => {}
            _ => return Err(unsafe_archive("the archive is cut short")),
        }
        if head.iter().all(|b| *b == 0) {
            break;
        }
        check_checksum(&head)?;
        let size = octal(&head[124..136])?;
        let kind = head[156];
        match kind {
            b'g' => {
                read_data(&mut input, size, MAX_EXTENDED)?;
                continue;
            }
            b'x' => {
                let data = read_data(&mut input, size, MAX_EXTENDED)?;
                pending_path = pax_path(&data)?.or(pending_path);
                continue;
            }
            b'L' => {
                let data = read_data(&mut input, size, MAX_EXTENDED)?;
                let text =
                    String::from_utf8(data).map_err(|_| unsafe_archive("a name is not text"))?;
                pending_path = Some(text.trim_end_matches('\0').to_owned());
                continue;
            }
            b'0' | 0 | b'5' => {}
            b'1' | b'2' => return Err(unsafe_archive("the archive contains links")),
            _ => return Err(unsafe_archive("the archive contains special files")),
        }

        entries += 1;
        if entries > limits.max_entries {
            return Err(PackError::Refused(PackRefusal::TooLarge));
        }
        let name = pending_path.take().unwrap_or_else(|| header_name(&head));
        let is_dir = kind == b'5';
        let parts = split_path(&name, limits)?;
        let (first, rest) = parts
            .split_first()
            .ok_or_else(|| unsafe_archive("an entry has no name"))?;
        match &top {
            None => top = Some(first.clone()),
            Some(t) if t == first => {}
            Some(_) => return Err(unsafe_archive("the archive has more than one top folder")),
        }
        if rest.len() > limits.max_depth {
            return Err(unsafe_archive("a path is too deep"));
        }
        let target = rest.iter().fold(root_dir.clone(), |p, c| p.join(c));
        if is_dir {
            read_data(&mut input, size, 0)?;
            if !rest.is_empty() {
                names.folders(rest, rest.len())?;
                std::fs::create_dir_all(&target)
                    .map_err(|_| unsafe_archive("two names conflict"))?;
            }
            continue;
        }
        if rest.is_empty() {
            return Err(unsafe_archive("a file sits where the top folder should be"));
        }
        if size > limits.max_file || bytes.saturating_add(size) > limits.max_expanded {
            return Err(PackError::Refused(PackRefusal::TooLarge));
        }
        names.folders(rest, rest.len() - 1)?;
        names.file(&rest.join("/"))?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|_| unsafe_archive("two names conflict"))?;
        }
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|_| unsafe_archive("two names conflict"))?;
        copy_exact(&mut input, &mut out, size)?;
        skip_padding(&mut input, size)?;
        bytes += size;
        files += 1;
        if rest.len() == 1 {
            top_files.push(rest[0].to_lowercase());
        }
    }
    if files == 0 {
        return Err(unsafe_archive("the archive holds no files"));
    }
    Ok(Unpacked {
        files,
        bytes,
        top_files,
    })
}

/// Every name used so far, by lower-case path, with the spelling first seen and whether it is a
/// folder. Two spellings of one name (`Dir` and `dir`) are two folders on a case-sensitive disk
/// and one on Windows and default macOS, so the same archive would unpack differently on each.
#[derive(Default)]
struct Names(BTreeMap<String, (String, bool)>);

impl Names {
    /// The first `count` folders on the way down `parts`. Using one spelling again is fine.
    fn folders(&mut self, parts: &[String], count: usize) -> Result<(), PackError> {
        for end in 1..=count {
            let path = parts[..end].join("/");
            match self.0.entry(path.to_lowercase()) {
                Entry::Vacant(slot) => {
                    slot.insert((path, true));
                }
                Entry::Occupied(slot) if slot.get().0 == path => {}
                Entry::Occupied(_) => return Err(same_in_any_case()),
            }
        }
        Ok(())
    }

    /// A file name must be new in any spelling.
    fn file(&mut self, path: &str) -> Result<(), PackError> {
        match self.0.entry(path.to_lowercase()) {
            Entry::Vacant(slot) => {
                slot.insert((path.to_owned(), false));
                Ok(())
            }
            Entry::Occupied(slot) if slot.get().1 && slot.get().0 == path => {
                Err(unsafe_archive("two names conflict"))
            }
            Entry::Occupied(_) => Err(same_in_any_case()),
        }
    }
}

fn same_in_any_case() -> PackError {
    unsafe_archive("two names differ only in case")
}

fn io_failed() -> PackError {
    PackError::Failed(craft_ai_core::AiError::new(
        craft_ai_core::ErrorKind::Internal,
        "could not write the node pack (is the disk full?)",
    ))
}

/// Reads until `buf` is full or the stream ends; returns how many bytes arrived.
fn read_full(input: &mut impl Read, buf: &mut [u8]) -> Result<usize, PackError> {
    let mut filled = 0;
    while filled < buf.len() {
        match input.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => return Err(unsafe_archive("the archive is damaged")),
        }
    }
    Ok(filled)
}

fn read_data(input: &mut impl Read, size: u64, max: u64) -> Result<Vec<u8>, PackError> {
    if size > max {
        return Err(unsafe_archive("an archive header is too large"));
    }
    let mut data = vec![0u8; usize::try_from(size).map_err(|_| unsafe_archive("bad size"))?];
    if read_full(input, &mut data)? != data.len() {
        return Err(unsafe_archive("the archive is cut short"));
    }
    skip_padding(input, size)?;
    Ok(data)
}

fn skip_padding(input: &mut impl Read, size: u64) -> Result<(), PackError> {
    let pad = (BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64;
    let mut sink = vec![0u8; usize::try_from(pad).unwrap_or(0)];
    if read_full(input, &mut sink)? != sink.len() {
        return Err(unsafe_archive("the archive is cut short"));
    }
    Ok(())
}

fn copy_exact(input: &mut impl Read, out: &mut std::fs::File, size: u64) -> Result<(), PackError> {
    use std::io::Write;
    let mut left = size;
    let mut buffer = vec![0u8; 64 * 1024];
    while left > 0 {
        let want = usize::try_from(left.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let got = read_full(input, &mut buffer[..want])?;
        if got != want {
            return Err(unsafe_archive("the archive is cut short"));
        }
        out.write_all(&buffer[..got]).map_err(|_| io_failed())?;
        left -= got as u64;
    }
    Ok(())
}

fn octal(field: &[u8]) -> Result<u64, PackError> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        return Err(unsafe_archive("an entry size is in a form not accepted"));
    }
    let text: String = field
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| char::from(*b))
        .collect();
    let digits = text.trim();
    if digits.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(digits, 8).map_err(|_| unsafe_archive("the archive is damaged"))
}

fn check_checksum(head: &[u8; BLOCK]) -> Result<(), PackError> {
    let stored = octal(&head[148..156])?;
    let sum: u64 = head
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(*b)
            }
        })
        .sum();
    if sum == stored {
        Ok(())
    } else {
        Err(unsafe_archive("the archive is damaged"))
    }
}

fn cstr(field: &[u8]) -> String {
    String::from_utf8_lossy(field.split(|b| *b == 0).next().unwrap_or_default()).into_owned()
}

fn header_name(head: &[u8; BLOCK]) -> String {
    let name = cstr(&head[0..100]);
    if head[257..262] == *b"ustar" {
        let prefix = cstr(&head[345..500]);
        if !prefix.is_empty() {
            return format!("{prefix}/{name}");
        }
    }
    name
}

/// The `path` record of a pax extended header. `size` and `linkpath` are not accepted.
fn pax_path(data: &[u8]) -> Result<Option<String>, PackError> {
    let text = std::str::from_utf8(data).map_err(|_| unsafe_archive("a name is not text"))?;
    let mut path = None;
    for record in text.split_inclusive('\n') {
        let (_, rest) = record
            .split_once(' ')
            .ok_or_else(|| unsafe_archive("the archive is damaged"))?;
        let (key, value) = rest
            .trim_end_matches('\n')
            .split_once('=')
            .ok_or_else(|| unsafe_archive("the archive is damaged"))?;
        match key {
            "path" => path = Some(value.to_owned()),
            "size" | "linkpath" => {
                return Err(unsafe_archive("an entry uses a header not accepted"));
            }
            _ => {}
        }
    }
    Ok(path)
}

fn split_path(name: &str, limits: &PackLimits) -> Result<Vec<String>, PackError> {
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    if trimmed.len() > MAX_PATH_BYTES {
        return Err(unsafe_archive("a path is too long"));
    }
    let _ = limits;
    let parts: Vec<String> = trimmed.split('/').map(str::to_owned).collect();
    if parts.iter().any(|part| !safe_component(part)) {
        return Err(unsafe_archive("a path is not a plain relative path"));
    }
    Ok(parts)
}

fn safe_component(part: &str) -> bool {
    if part.is_empty() || part == "." || part == ".." || part.len() > 255 {
        return false;
    }
    if part.ends_with('.') || part.ends_with(' ') {
        return false;
    }
    if part
        .chars()
        .any(|c| c.is_control() || "\\:<>\"|?*".contains(c))
    {
        return false;
    }
    let stem = part.split('.').next().unwrap_or(part).to_lowercase();
    !RESERVED.contains(&stem.as_str())
}

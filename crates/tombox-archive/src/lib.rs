//! `tombox-archive` — ZIP64-aware central-directory reader + extraction safety
//! validator. It reads only the archive's *tail* (End-of-Central-Directory and
//! the central directory), never the whole file, so it can inventory a 25 GB
//! archive cheaply. It performs **no decompression** and never writes files.
//!
//! Purpose (CLAUDE.md §2): inventory the dump and vet every entry for path
//! traversal, absolute paths, drive letters, symlinks, and decompression-bomb
//! ratios *before* any extraction happens.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_EOCD64: u32 = 0x0606_4b50;
const SIG_EOCD64_LOCATOR: u32 = 0x0706_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;

const EOCD_LEN: usize = 22;
const ZIP64_LOCATOR_LEN: usize = 20;

const MAX_CD_BYTES: u64 = 512 * 1024 * 1024; // 512 MiB cap on central directory

#[derive(Debug)]
pub enum ArchiveError {
    Io(String),
    NotZip(String),
    Malformed(String),
    TooLarge(String),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::Io(s) => write!(f, "I/O error: {s}"),
            ArchiveError::NotZip(s) => write!(f, "not a ZIP: {s}"),
            ArchiveError::Malformed(s) => write!(f, "malformed ZIP: {s}"),
            ArchiveError::TooLarge(s) => write!(f, "refusing: {s}"),
        }
    }
}
impl std::error::Error for ArchiveError {}

type Result<T> = std::result::Result<T, ArchiveError>;

// ---- little-endian helpers over an in-memory slice -------------------------

fn u16le(b: &[u8], o: usize) -> Result<u16> {
    b.get(o..o + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| ArchiveError::Malformed(format!("u16 read at {o} out of bounds")))
}
fn u32le(b: &[u8], o: usize) -> Result<u32> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| ArchiveError::Malformed(format!("u32 read at {o} out of bounds")))
}
fn u64le(b: &[u8], o: usize) -> Result<u64> {
    b.get(o..o + 8)
        .map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
        .ok_or_else(|| ArchiveError::Malformed(format!("u64 read at {o} out of bounds")))
}

fn read_at(file: &mut File, offset: u64, len: usize) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| ArchiveError::Io(e.to_string()))?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf)
        .map_err(|e| ArchiveError::Io(format!("reading {len} bytes at {offset}: {e}")))?;
    Ok(buf)
}

// ---- data model ------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ZipEntry {
    pub name: String,
    /// Raw name bytes, preserved (ZIP names can be CP437 or UTF-8).
    pub name_raw: Vec<u8>,
    pub method: u16,
    pub flags: u16,
    pub crc32: u32,
    pub comp_size: u64,
    pub uncomp_size: u64,
    pub local_header_offset: u64,
    pub external_attrs: u32,
    pub is_dir: bool,
    /// True if the Unix mode in the high 16 bits of external attrs is a symlink.
    pub is_symlink: bool,
    pub utf8_name: bool,
}

impl ZipEntry {
    /// Unix mode from external attributes (high 16 bits), if this looks like a
    /// Unix-origin entry (version-made-by host 3). We approximate by always
    /// extracting the high word; callers use it only for symlink detection.
    fn unix_mode(external_attrs: u32) -> u16 {
        (external_attrs >> 16) as u16
    }
}

#[derive(Debug, Clone)]
pub struct ZipInventory {
    pub entry_count: u64,
    pub zip64: bool,
    pub cd_offset: u64,
    pub cd_size: u64,
    pub entries: Vec<ZipEntry>,
}

// ---- parsing ---------------------------------------------------------------

pub fn read_inventory<P: AsRef<Path>>(path: P) -> Result<ZipInventory> {
    let mut file = File::open(path.as_ref()).map_err(|e| ArchiveError::Io(e.to_string()))?;
    let file_len = file
        .seek(SeekFrom::End(0))
        .map_err(|e| ArchiveError::Io(e.to_string()))?;
    if file_len < EOCD_LEN as u64 {
        return Err(ArchiveError::NotZip("file smaller than EOCD record".into()));
    }

    let max_tail_len = ZIP64_LOCATOR_LEN + EOCD_LEN + usize::from(u16::MAX);
    let tail_len = file_len.min(max_tail_len as u64) as usize;
    let tail_start = file_len - tail_len as u64;
    let tail = read_at(&mut file, tail_start, tail_len)?;
    let eocd_rel = find_eocd(&tail)
        .ok_or_else(|| ArchiveError::NotZip("no EOCD record ending at EOF in tail".into()))?;
    let eocd = &tail[eocd_rel..];

    let mut entry_count = u16le(eocd, 10)? as u64;
    let mut cd_size = u32le(eocd, 12)? as u64;
    let mut cd_offset = u32le(eocd, 16)? as u64;
    let mut zip64 = false;

    // --- ZIP64 if any sentinel value present ---
    let sentinel = entry_count == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_offset == 0xFFFF_FFFF;
    if sentinel {
        zip64 = true;
        // The ZIP64 EOCD locator sits 20 bytes before the EOCD record.
        if eocd_rel < ZIP64_LOCATOR_LEN {
            return Err(ArchiveError::Malformed(
                "ZIP64 indicated but no room for locator before EOCD".into(),
            ));
        }
        let loc = &tail[eocd_rel - ZIP64_LOCATOR_LEN..eocd_rel];
        if u32le(loc, 0)? != SIG_EOCD64_LOCATOR {
            return Err(ArchiveError::Malformed("ZIP64 EOCD locator signature wrong".into()));
        }
        let z64_eocd_offset = u64le(loc, 8)?;
        // Read the ZIP64 EOCD record (fixed part is 56 bytes).
        z64_eocd_offset
            .checked_add(56)
            .filter(|&end| end <= file_len)
            .ok_or_else(|| {
                ArchiveError::Malformed(format!(
                    "ZIP64 EOCD record at {z64_eocd_offset} extends past EOF ({file_len})"
                ))
            })?;
        let z = read_at(&mut file, z64_eocd_offset, 56)?;
        if u32le(&z, 0)? != SIG_EOCD64 {
            return Err(ArchiveError::Malformed("ZIP64 EOCD record signature wrong".into()));
        }
        entry_count = u64le(&z, 32)?;
        cd_size = u64le(&z, 40)?;
        cd_offset = u64le(&z, 48)?;
    }

    if cd_size > MAX_CD_BYTES {
        return Err(ArchiveError::TooLarge(format!(
            "central directory is {cd_size} bytes (> {MAX_CD_BYTES} cap)"
        )));
    }
    let cd_end = cd_offset.checked_add(cd_size).ok_or_else(|| {
        ArchiveError::Malformed(format!(
            "central directory bounds overflow (offset {cd_offset}, size {cd_size})"
        ))
    })?;
    if cd_end > file_len {
        return Err(ArchiveError::Malformed(format!(
            "central directory [{cd_offset}, {cd_end}) extends past EOF ({file_len})"
        )));
    }

    // --- read and parse the central directory ---
    let cd = read_at(&mut file, cd_offset, cd_size as usize)?;
    let entries = parse_central_directory(&cd, entry_count)?;

    Ok(ZipInventory {
        entry_count,
        zip64,
        cd_offset,
        cd_size,
        entries,
    })
}

fn find_eocd(buf: &[u8]) -> Option<usize> {
    let s = SIG_EOCD.to_le_bytes();
    let mut i = buf.len().checked_sub(EOCD_LEN)?;
    loop {
        if buf[i..i + 4] == s {
            let comment = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
            if i + EOCD_LEN + comment == buf.len() {
                return Some(i);
            }
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

fn parse_central_directory(cd: &[u8], expected: u64) -> Result<Vec<ZipEntry>> {
    let mut entries = Vec::new();
    let mut o = 0usize;
    while o + 46 <= cd.len() {
        if u32le(cd, o)? != SIG_CENTRAL {
            // End of central-directory headers (or digital-signature record).
            break;
        }
        let version_made_by = u16le(cd, o + 4)?;
        let flags = u16le(cd, o + 8)?;
        let method = u16le(cd, o + 10)?;
        let crc32 = u32le(cd, o + 16)?;
        let mut comp_size = u32le(cd, o + 20)? as u64;
        let mut uncomp_size = u32le(cd, o + 24)? as u64;
        let name_len = u16le(cd, o + 28)? as usize;
        let extra_len = u16le(cd, o + 30)? as usize;
        let comment_len = u16le(cd, o + 32)? as usize;
        let external_attrs = u32le(cd, o + 38)?;
        let mut local_header_offset = u32le(cd, o + 42)? as u64;

        let name_start = o + 46;
        let name_end = name_start
            .checked_add(name_len)
            .ok_or_else(|| ArchiveError::Malformed("name length overflow".into()))?;
        let name_raw = cd
            .get(name_start..name_end)
            .ok_or_else(|| ArchiveError::Malformed("entry name past end of CD".into()))?
            .to_vec();
        let extra_start = name_end;
        let extra_end = extra_start + extra_len;
        let extra = cd
            .get(extra_start..extra_end)
            .ok_or_else(|| ArchiveError::Malformed("extra field past end of CD".into()))?;

        // ZIP64 extended info (tag 0x0001): present fields appear in a fixed
        // order ONLY for those whose base value is the 0xFFFF.../sentinel.
        if comp_size == 0xFFFF_FFFF || uncomp_size == 0xFFFF_FFFF || local_header_offset == 0xFFFF_FFFF
        {
            if let Some((unc, cmp, lho)) =
                parse_zip64_extra(extra, uncomp_size, comp_size, local_header_offset)?
            {
                uncomp_size = unc;
                comp_size = cmp;
                local_header_offset = lho;
            }
        }

        let utf8_name = flags & 0x0800 != 0;
        let name = if utf8_name {
            String::from_utf8_lossy(&name_raw).into_owned()
        } else {
            // CP437-ish fallback: treat as latin-ish, lossy. Good enough for vetting.
            String::from_utf8_lossy(&name_raw).into_owned()
        };
        let is_dir = name.ends_with('/') || name.ends_with('\\');
        // Unix symlink detection: host 3 (Unix) in version-made-by high byte,
        // and S_IFLNK (0xA000) set in the mode.
        let host = (version_made_by >> 8) as u8;
        let mode = ZipEntry::unix_mode(external_attrs);
        let is_symlink = host == 3 && (mode & 0xF000) == 0xA000;

        entries.push(ZipEntry {
            name,
            name_raw,
            method,
            flags,
            crc32,
            comp_size,
            uncomp_size,
            local_header_offset,
            external_attrs,
            is_dir,
            is_symlink,
            utf8_name,
        });

        o = extra_end + comment_len;
    }

    if (entries.len() as u64) != expected {
        // Not fatal — some archives miscount or include extra records — but worth
        // surfacing to the caller via a Malformed only if we got zero.
        if entries.is_empty() && expected > 0 {
            return Err(ArchiveError::Malformed(format!(
                "expected {expected} entries, parsed 0"
            )));
        }
    }
    Ok(entries)
}

fn parse_zip64_extra(
    extra: &[u8],
    base_unc: u64,
    base_cmp: u64,
    base_lho: u64,
) -> Result<Option<(u64, u64, u64)>> {
    let mut o = 0usize;
    while o + 4 <= extra.len() {
        let tag = u16le(extra, o)?;
        let size = u16le(extra, o + 2)? as usize;
        let body_start = o + 4;
        let body_end = body_start
            .checked_add(size)
            .ok_or_else(|| ArchiveError::Malformed("zip64 extra size overflow".into()))?;
        if body_end > extra.len() {
            return Err(ArchiveError::Malformed("zip64 extra field past end".into()));
        }
        if tag == 0x0001 {
            let body = &extra[body_start..body_end];
            let mut p = 0usize;
            let mut unc = base_unc;
            let mut cmp = base_cmp;
            let mut lho = base_lho;
            // Fields present in order, only for those whose base was the sentinel.
            if base_unc == 0xFFFF_FFFF {
                unc = u64le(body, p)?;
                p += 8;
            }
            if base_cmp == 0xFFFF_FFFF {
                cmp = u64le(body, p)?;
                p += 8;
            }
            if base_lho == 0xFFFF_FFFF {
                lho = u64le(body, p)?;
                let _ = p + 8;
            }
            return Ok(Some((unc, cmp, lho)));
        }
        o = body_end;
    }
    Ok(None)
}

// ---- safety validation -----------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    Block,
    Warn,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: Severity,
    pub entry: String,
    pub issue: String,
}

#[derive(Debug, Clone, Default)]
pub struct SafetyReport {
    pub findings: Vec<Finding>,
    pub total_uncompressed: u64,
    pub total_compressed: u64,
    pub max_ratio: f64,
    pub file_count: usize,
    pub dir_count: usize,
    pub symlink_count: usize,
}

impl SafetyReport {
    pub fn is_safe_to_extract(&self) -> bool {
        !self.findings.iter().any(|f| f.severity == Severity::Block)
    }
}

/// Validate every entry name/attribute for unsafe extraction. `ratio_threshold`
/// flags likely decompression bombs (uncompressed/compressed per entry).
pub fn validate(inv: &ZipInventory, ratio_threshold: f64) -> SafetyReport {
    let mut r = SafetyReport::default();
    let mut seen: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for e in &inv.entries {
        if e.is_dir {
            r.dir_count += 1;
        } else {
            r.file_count += 1;
        }
        r.total_uncompressed = r.total_uncompressed.saturating_add(e.uncomp_size);
        r.total_compressed = r.total_compressed.saturating_add(e.comp_size);

        let name = &e.name;
        // Normalise separators for checks.
        let norm = name.replace('\\', "/");
        let key = norm.trim_end_matches('/').to_lowercase();
        if !key.is_empty() {
            if let Some((previous, previous_is_dir)) = seen.insert(key, (name.clone(), e.is_dir)) {
                if !(previous_is_dir && e.is_dir) {
                    block(
                        &mut r,
                        name,
                        &format!(
                            "entry name collides with '{previous}' on a case-insensitive file system"
                        ),
                    );
                }
            }
        }

        // Absolute path / drive letter / UNC.
        if norm.starts_with('/') {
            block(&mut r, name, "absolute path (leading '/')");
        }
        if norm.len() >= 2 && norm.as_bytes()[1] == b':' && norm.as_bytes()[0].is_ascii_alphabetic()
        {
            block(&mut r, name, "Windows drive-letter path");
        }
        if norm.starts_with("//") {
            block(&mut r, name, "UNC-style path");
        }
        // Path traversal: any '..' path component.
        if norm.split('/').any(|c| c == "..") {
            block(&mut r, name, "path traversal ('..' component)");
        }
        // NUL or control bytes in the raw name.
        if e.name_raw.iter().any(|&b| b == 0) {
            block(&mut r, name, "NUL byte in entry name");
        }
        // Symlinks: refuse by default (they can redirect writes outside the tree).
        if e.is_symlink {
            r.symlink_count += 1;
            block(&mut r, name, "symlink entry");
        }

        // Decompression bomb heuristic (per-entry ratio).
        if e.comp_size > 0 {
            let ratio = e.uncomp_size as f64 / e.comp_size as f64;
            if ratio > r.max_ratio {
                r.max_ratio = ratio;
            }
            if ratio > ratio_threshold && e.uncomp_size > 10 * 1024 * 1024 {
                warn(
                    &mut r,
                    name,
                    &format!(
                        "high compression ratio {ratio:.0}x ({} -> {} bytes)",
                        e.comp_size, e.uncomp_size
                    ),
                );
            }
        } else if e.uncomp_size > 10 * 1024 * 1024 {
            warn(
                &mut r,
                name,
                &format!("declares {} uncompressed bytes from 0 compressed bytes", e.uncomp_size),
            );
        }
    }
    r
}

fn block(r: &mut SafetyReport, entry: &str, issue: &str) {
    r.findings.push(Finding {
        severity: Severity::Block,
        entry: entry.to_string(),
        issue: issue.to_string(),
    });
}
fn warn(r: &mut SafetyReport, entry: &str, issue: &str) {
    r.findings.push(Finding {
        severity: Severity::Warn,
        entry: entry.to_string(),
        issue: issue.to_string(),
    });
}

#[cfg(test)]
mod tests;

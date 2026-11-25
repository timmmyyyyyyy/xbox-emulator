//! Deterministic tests over hand-built ZIP byte buffers. No external tools, no
//! decompression. Each malicious-entry test asserts the validator blocks it.

use super::*;
use std::io::Write;

/// Build one central-directory header for a stored entry.
fn cd_header(
    name: &str,
    comp: u32,
    uncomp: u32,
    version_made_by: u16,
    external_attrs: u32,
    extra: &[u8],
) -> Vec<u8> {
    let mut h = Vec::new();
    h.extend_from_slice(&SIG_CENTRAL.to_le_bytes());
    h.extend_from_slice(&version_made_by.to_le_bytes()); // version made by
    h.extend_from_slice(&20u16.to_le_bytes()); // version needed
    h.extend_from_slice(&0u16.to_le_bytes()); // flags
    h.extend_from_slice(&0u16.to_le_bytes()); // method = store
    h.extend_from_slice(&0u16.to_le_bytes()); // mod time
    h.extend_from_slice(&0u16.to_le_bytes()); // mod date
    h.extend_from_slice(&0u32.to_le_bytes()); // crc32
    h.extend_from_slice(&comp.to_le_bytes()); // comp size
    h.extend_from_slice(&uncomp.to_le_bytes()); // uncomp size
    h.extend_from_slice(&(name.len() as u16).to_le_bytes()); // name len
    h.extend_from_slice(&(extra.len() as u16).to_le_bytes()); // extra len
    h.extend_from_slice(&0u16.to_le_bytes()); // comment len
    h.extend_from_slice(&0u16.to_le_bytes()); // disk number start
    h.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
    h.extend_from_slice(&external_attrs.to_le_bytes()); // external attrs
    h.extend_from_slice(&0u32.to_le_bytes()); // local header offset
    h.extend_from_slice(name.as_bytes());
    h.extend_from_slice(extra);
    h
}

fn eocd(entry_count: u16, cd_size: u32, cd_offset: u32) -> Vec<u8> {
    let mut e = Vec::new();
    e.extend_from_slice(&SIG_EOCD.to_le_bytes());
    e.extend_from_slice(&0u16.to_le_bytes()); // disk num
    e.extend_from_slice(&0u16.to_le_bytes()); // disk with cd
    e.extend_from_slice(&entry_count.to_le_bytes()); // entries this disk
    e.extend_from_slice(&entry_count.to_le_bytes()); // total entries
    e.extend_from_slice(&cd_size.to_le_bytes());
    e.extend_from_slice(&cd_offset.to_le_bytes());
    e.extend_from_slice(&0u16.to_le_bytes()); // comment len
    e
}

fn eocd_with_comment(entry_count: u16, cd_size: u32, cd_offset: u32, comment: &[u8]) -> Vec<u8> {
    assert!(comment.len() <= u16::MAX as usize);
    let mut e = eocd(entry_count, cd_size, cd_offset);
    let n = e.len();
    e[n - 2..].copy_from_slice(&(comment.len() as u16).to_le_bytes());
    e.extend_from_slice(comment);
    e
}

/// Assemble an archive = [central directory][EOCD], cd at offset 0.
fn assemble(headers: &[Vec<u8>]) -> Vec<u8> {
    assemble_with_comment(headers, b"")
}

fn assemble_with_comment(headers: &[Vec<u8>], comment: &[u8]) -> Vec<u8> {
    let mut cd = Vec::new();
    for h in headers {
        cd.extend_from_slice(h);
    }
    let cd_size = cd.len() as u32;
    let mut buf = cd;
    buf.extend_from_slice(&eocd_with_comment(headers.len() as u16, cd_size, 0, comment));
    buf
}

/// Write bytes to a unique temp file and return its path.
fn temp_zip(bytes: &[u8], tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("tombox_test_{tag}_{nanos}.zip"));
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(bytes).unwrap();
    p
}

fn inspect(bytes: &[u8], tag: &str) -> ZipInventory {
    let p = temp_zip(bytes, tag);
    let inv = read_inventory(&p);
    let _ = std::fs::remove_file(&p);
    inv.expect("inventory should parse")
}

#[test]
fn parses_simple_entry() {
    let h = cd_header("dir/hello.txt", 11, 42, 0, 0, &[]);
    let inv = inspect(&assemble(&[h]), "simple");
    assert_eq!(inv.entries.len(), 1);
    let e = &inv.entries[0];
    assert_eq!(e.name, "dir/hello.txt");
    assert_eq!(e.comp_size, 11);
    assert_eq!(e.uncomp_size, 42);
    assert!(!e.is_dir);
}

#[test]
fn directory_entry_detected() {
    let h = cd_header("some/dir/", 0, 0, 0, 0, &[]);
    let inv = inspect(&assemble(&[h]), "dir");
    assert!(inv.entries[0].is_dir);
    let r = validate(&inv, 1000.0);
    assert_eq!(r.dir_count, 1);
    assert!(r.is_safe_to_extract());
}

#[test]
fn blocks_path_traversal() {
    for bad in ["../evil.txt", "a/../../b", "..\\windows\\system32\\x"] {
        let inv = inspect(&assemble(&[cd_header(bad, 1, 1, 0, 0, &[])]), "trav");
        let r = validate(&inv, 1000.0);
        assert!(!r.is_safe_to_extract(), "should block {bad}");
        assert!(r.findings.iter().any(|f| f.issue.contains("traversal")), "for {bad}");
    }
}

#[test]
fn blocks_absolute_and_drive_paths() {
    let inv = inspect(
        &assemble(&[
            cd_header("/etc/passwd", 1, 1, 0, 0, &[]),
            cd_header("C:/Windows/x", 1, 1, 0, 0, &[]),
            cd_header("//server/share/x", 1, 1, 0, 0, &[]),
        ]),
        "abs",
    );
    let r = validate(&inv, 1000.0);
    assert!(!r.is_safe_to_extract());
    assert!(r.findings.iter().any(|f| f.issue.contains("absolute")));
    assert!(r.findings.iter().any(|f| f.issue.contains("drive-letter")));
    assert!(r.findings.iter().any(|f| f.issue.contains("UNC")));
}

#[test]
fn blocks_symlink_entry() {
    // version made by: host 3 (Unix) in high byte; external attrs high word = S_IFLNK|0777
    let version_made_by = (3u16 << 8) | 20;
    let external = (0xA1FFu32) << 16;
    let inv = inspect(
        &assemble(&[cd_header("link", 5, 5, version_made_by, external, &[])]),
        "symlink",
    );
    assert!(inv.entries[0].is_symlink);
    let r = validate(&inv, 1000.0);
    assert!(!r.is_safe_to_extract());
    assert_eq!(r.symlink_count, 1);
}

#[test]
fn blocks_duplicate_entries() {
    let inv = inspect(
        &assemble(&[
            cd_header("Game/Content/pakchunk0.pak", 1, 1, 0, 0, &[]),
            cd_header("game/content/PAKCHUNK0.PAK", 1, 1, 0, 0, &[]),
        ]),
        "duplicate",
    );
    let r = validate(&inv, 1000.0);
    assert!(!r.is_safe_to_extract());
    assert!(r.findings.iter().any(|f| f.issue.contains("collides")));
}

#[test]
fn blocks_file_and_directory_with_the_same_name() {
    let inv = inspect(
        &assemble(&[
            cd_header("Game/Content", 1, 1, 0, 0, &[]),
            cd_header("game/content/", 0, 0, 0, 0, &[]),
        ]),
        "duplicate-file-dir",
    );
    let r = validate(&inv, 1000.0);
    assert!(!r.is_safe_to_extract());
    assert!(r.findings.iter().any(|f| f.issue.contains("collides")));
}

#[test]
fn allows_distinct_names_and_repeated_directories() {
    let inv = inspect(
        &assemble(&[
            cd_header("Game/Content/a.pak", 1, 1, 0, 0, &[]),
            cd_header("Game/Content/b.pak", 1, 1, 0, 0, &[]),
            cd_header("Game/", 0, 0, 0, 0, &[]),
            cd_header("game/", 0, 0, 0, 0, &[]),
        ]),
        "distinct",
    );
    let r = validate(&inv, 1000.0);
    assert!(r.is_safe_to_extract());
}

#[test]
fn warns_on_bomb_ratio() {
    // 20 MiB uncompressed from 1000 bytes => ratio 20971x, over threshold.
    let h = cd_header("bomb.bin", 1000, 20 * 1024 * 1024, 0, 0, &[]);
    let inv = inspect(&assemble(&[h]), "bomb");
    let r = validate(&inv, 100.0);
    assert!(r.findings.iter().any(|f| matches!(f.severity, Severity::Warn)));
    // A warning is not a hard block.
    assert!(r.is_safe_to_extract());
}

#[test]
fn warns_when_a_large_entry_claims_zero_compressed_bytes() {
    let h = cd_header("bomb0.bin", 0, 20 * 1024 * 1024, 0, 0, &[]);
    let inv = inspect(&assemble(&[h]), "bomb0");
    let r = validate(&inv, 100.0);
    assert!(r.findings.iter().any(|f| f.severity == Severity::Warn && f.entry == "bomb0.bin"));
    assert!(r.is_safe_to_extract());
}

#[test]
fn small_or_empty_entries_with_zero_compressed_bytes_are_not_flagged() {
    let empty = cd_header("empty.txt", 0, 0, 0, 0, &[]);
    let small = cd_header("small.bin", 0, 1024, 0, 0, &[]);
    let inv = inspect(&assemble(&[empty, small]), "zero_small");
    let r = validate(&inv, 100.0);
    assert!(r.findings.is_empty());
}

#[test]
fn resolves_zip64_extra_sizes() {
    // Build a zip64 extra with uncompressed + compressed 64-bit values, base
    // fields set to the 0xFFFFFFFF sentinel.
    let big_unc: u64 = 5_000_000_000; // > 4 GiB
    let big_cmp: u64 = 4_300_000_000;
    let mut extra = Vec::new();
    extra.extend_from_slice(&0x0001u16.to_le_bytes()); // tag
    extra.extend_from_slice(&16u16.to_le_bytes()); // size: two u64
    extra.extend_from_slice(&big_unc.to_le_bytes());
    extra.extend_from_slice(&big_cmp.to_le_bytes());
    let h = cd_header("huge.pak", 0xFFFF_FFFF, 0xFFFF_FFFF, 0, 0, &extra);
    let inv = inspect(&assemble(&[h]), "zip64");
    assert_eq!(inv.entries[0].uncomp_size, big_unc);
    assert_eq!(inv.entries[0].comp_size, big_cmp);
}

#[test]
fn ignores_fake_eocd_inside_comment() {
    let h = cd_header("../evil.txt", 1, 1, 0, 0, &[]);
    let fake = eocd(0, 0, 0);
    let mut comment = b"note: ".to_vec();
    comment.extend_from_slice(&fake);
    comment.extend_from_slice(b" -- end");
    let inv = inspect(&assemble_with_comment(&[h], &comment), "fake-eocd");
    assert_eq!(inv.entry_count, 1);
    assert_eq!(inv.entries.len(), 1);
    assert_eq!(inv.entries[0].name, "../evil.txt");
    let r = validate(&inv, 1000.0);
    assert!(!r.is_safe_to_extract());
    assert!(r.findings.iter().any(|f| f.issue.contains("traversal")));
}

#[test]
fn accepts_eocd_with_comment() {
    let inv = inspect(
        &assemble_with_comment(&[cd_header("ok.txt", 1, 1, 0, 0, &[])], b"release notes"),
        "comment",
    );
    assert_eq!(inv.entry_count, 1);
    assert_eq!(inv.entries.len(), 1);
    assert_eq!(inv.entries[0].name, "ok.txt");
}

fn zip64_eocd(cd_offset: u64, cd_size: u64, comment: &[u8]) -> Vec<u8> {
    let mut z = Vec::new();
    z.extend_from_slice(&SIG_EOCD64.to_le_bytes());
    z.extend_from_slice(&44u64.to_le_bytes()); // size of the remaining record
    z.extend_from_slice(&45u16.to_le_bytes()); // version made by
    z.extend_from_slice(&45u16.to_le_bytes()); // version needed
    z.extend_from_slice(&0u32.to_le_bytes()); // this disk
    z.extend_from_slice(&0u32.to_le_bytes()); // disk with CD
    z.extend_from_slice(&0u64.to_le_bytes()); // entries this disk
    z.extend_from_slice(&0u64.to_le_bytes()); // total entries
    z.extend_from_slice(&cd_size.to_le_bytes());
    z.extend_from_slice(&cd_offset.to_le_bytes());
    z.extend_from_slice(&SIG_EOCD64_LOCATOR.to_le_bytes());
    z.extend_from_slice(&0u32.to_le_bytes()); // disk with ZIP64 EOCD
    z.extend_from_slice(&0u64.to_le_bytes()); // offset of ZIP64 EOCD (0)
    z.extend_from_slice(&1u32.to_le_bytes()); // total disks
    z.extend_from_slice(&eocd_with_comment(u16::MAX, u32::MAX, u32::MAX, comment));
    z
}

#[test]
fn accepts_zip64_with_long_archive_comments() {
    for comment_len in [u16::MAX - 20, u16::MAX - 19, u16::MAX] {
        let comment = vec![b'a'; usize::from(comment_len)];
        let bytes = zip64_eocd(0, 0, &comment);
        let inv = inspect(&bytes, &format!("zip64-comment-{comment_len}"));
        assert!(inv.zip64);
        assert_eq!(inv.entry_count, 0);
        assert!(inv.entries.is_empty());
    }
}

#[test]
fn rejects_zip64_cd_bounds_overflow() {
    let bytes = zip64_eocd(0xFFFF_FFFF_FFFF_FFF8, 16, b"");
    let p = temp_zip(&bytes, "zip64-overflow");
    let res = read_inventory(&p);
    let _ = std::fs::remove_file(&p);
    match res {
        Err(ArchiveError::Malformed(msg)) => assert!(msg.contains("bounds overflow"), "{msg}"),
        Err(e) => panic!("expected malformed, got {e}"),
        Ok(_) => panic!("expected an error, got a valid inventory"),
    }
}

#[test]
fn rejects_zip64_eocd_past_eof() {
    let mut bytes = zip64_eocd(0, 0, b"");
    bytes[64..72].copy_from_slice(&u64::MAX.to_le_bytes());
    let p = temp_zip(&bytes, "zip64-past-eof");
    let res = read_inventory(&p);
    let _ = std::fs::remove_file(&p);
    match res {
        Err(ArchiveError::Malformed(msg)) => assert!(msg.contains("extends past EOF"), "{msg}"),
        Err(e) => panic!("expected malformed, got {e}"),
        Ok(_) => panic!("expected an error, got a valid inventory"),
    }
}

#[test]
fn clean_archive_is_safe() {
    let inv = inspect(
        &assemble(&[
            cd_header("Game/Content/paks/pakchunk0.pak", 100, 100, 0, 0, &[]),
            cd_header("Game/Binaries/Fortnite.exe", 50, 50, 0, 0, &[]),
            cd_header("Game/", 0, 0, 0, 0, &[]),
        ]),
        "clean",
    );
    let r = validate(&inv, 1000.0);
    assert!(r.is_safe_to_extract());
    assert_eq!(r.file_count, 2);
    assert_eq!(r.dir_count, 1);
}

#[test]
fn rejects_non_zip() {
    let p = temp_zip(&[0u8; 100], "notzip");
    let res = read_inventory(&p);
    let _ = std::fs::remove_file(&p);
    assert!(matches!(res, Err(ArchiveError::NotZip(_))));
}

use std::io::Write;

use base64::Engine;
use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};

use super::*;

// ── Test Helpers ────────────────────────────────────────────────────────

/// Create a tar archive containing a single regular file.
///
/// For paths that the tar crate rejects (e.g. absolute paths), builds
/// the raw tar entry bytes manually with correct checksums.
fn make_tar_with_file(name: &str, content: &[u8]) -> Vec<u8> {
    if name.starts_with('/') {
        return make_raw_tar_entry(name, content);
    }
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    builder.append_data(&mut header, name, content).unwrap();
    builder.into_inner().unwrap()
}

/// Build a raw tar entry with arbitrary path (including absolute paths).
///
/// Constructs a valid USTAR header + data blocks + EOF marker.
fn make_raw_tar_entry(name: &str, content: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut header = [0u8; 512];

    // Name field (offset 0, 100 bytes).
    let name_bytes = name.as_bytes();
    header[..name_bytes.len().min(100)].copy_from_slice(&name_bytes[..name_bytes.len().min(100)]);

    // Mode (offset 100, 8 bytes): 0644 octal.
    header[100..107].copy_from_slice(b"0000644");

    // UID (offset 108, 8 bytes): 0.
    header[108..115].copy_from_slice(b"0000000");

    // GID (offset 116, 8 bytes): 0.
    header[116..123].copy_from_slice(b"0000000");

    // Size (offset 124, 12 bytes): octal.
    let size_str = format!("{:011o}", content.len());
    header[124..135].copy_from_slice(size_str.as_bytes());

    // Mtime (offset 136, 12 bytes): 0.
    header[136..147].copy_from_slice(b"00000000000");

    // Type flag (offset 156, 1 byte): '0' = regular file.
    header[156] = b'0';

    // USTAR magic (offset 257, 6 bytes) + version (offset 263, 2 bytes).
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");

    // Checksum (offset 148, 8 bytes): compute with checksum field as spaces.
    header[148..156].fill(b' ');
    let cksum: u32 = header.iter().map(|&b| u32::from(b)).sum();
    let cksum_str = format!("{cksum:06o}\0 ");
    header[148..156].copy_from_slice(cksum_str.as_bytes());

    buf.extend_from_slice(&header);
    buf.extend_from_slice(content);

    // Pad to 512-byte boundary.
    let remainder = content.len() % 512;
    if remainder != 0 {
        buf.extend(std::iter::repeat_n(0u8, 512 - remainder));
    }

    // Two 512-byte zero blocks = end of archive.
    buf.extend(std::iter::repeat_n(0u8, 1024));
    buf
}

/// Create an empty tar archive (just the EOF markers).
fn make_empty_tar() -> Vec<u8> {
    let builder = tar::Builder::new(Vec::new());
    builder.into_inner().unwrap()
}

/// Compress tar bytes with gzip and encode as base64.
fn tar_to_base64_gzip(tar_data: &[u8]) -> String {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(tar_data).unwrap();
    let compressed = encoder.finish().unwrap();
    base64::engine::general_purpose::STANDARD.encode(&compressed)
}

/// Create a tar archive containing a char device entry (0, 0) — overlayfs whiteout.
fn make_tar_with_chardev(path: &str) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    header.set_mode(0o000);
    header.set_entry_type(tar::EntryType::Char);
    header.set_device_major(0).unwrap();
    header.set_device_minor(0).unwrap();
    header.set_cksum();
    builder
        .append_data(&mut header, path, std::io::empty())
        .unwrap();
    builder.into_inner().unwrap()
}

/// Create a tar archive with a directory that has the overlayfs opaque xattr.
fn make_tar_with_opaque_dir(dir_path: &str) -> Vec<u8> {
    build_tar_with_pax_xattr(dir_path, "SCHILY.xattr.trusted.overlay.opaque", "y")
}

/// Count decimal digits in a number.
fn count_digits(n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let mut count = 0;
    let mut val = n;
    while val > 0 {
        count += 1;
        val /= 10;
    }
    count
}

/// Build a tar archive with a directory entry carrying a pax extended attribute.
fn build_tar_with_pax_xattr(dir_path: &str, xattr_key: &str, xattr_val: &str) -> Vec<u8> {
    let mut buf = Vec::new();

    // 1) Build the pax extended header data.
    let record_body = format!("{xattr_key}={xattr_val}\n");
    // Pax record format: "<length> <key>=<value>\n" where length includes itself.
    let pax_record = {
        // Start with estimate, iterate to find stable length.
        let body_len = record_body.len();
        let mut total = body_len + 1; // +1 for the space
        loop {
            let digits = count_digits(total);
            let candidate = digits + 1 + body_len; // digits + space + body
            if candidate == total {
                break;
            }
            total = candidate;
        }
        format!("{total} {record_body}")
    };
    let pax_data = pax_record.as_bytes();

    // 2) Pax extended header entry (type 'x').
    let mut pax_header = tar::Header::new_ustar();
    pax_header.set_size(pax_data.len() as u64);
    pax_header.set_entry_type(tar::EntryType::XHeader);
    pax_header.set_mode(0o644);
    pax_header.set_path("PaxHeader/opaque").unwrap();
    pax_header.set_cksum();
    buf.extend_from_slice(pax_header.as_bytes());

    // Pax data + padding to 512-byte boundary.
    buf.extend_from_slice(pax_data);
    let remainder = pax_data.len() % 512;
    if remainder != 0 {
        buf.extend(std::iter::repeat_n(0u8, 512 - remainder));
    }

    // 3) The actual directory entry.
    let mut dir_header = tar::Header::new_ustar();
    dir_header.set_size(0);
    dir_header.set_mode(0o755);
    dir_header.set_entry_type(tar::EntryType::Directory);
    dir_header.set_path(dir_path).unwrap();
    dir_header.set_cksum();
    buf.extend_from_slice(dir_header.as_bytes());

    // 4) Two 512-byte zero blocks = end of archive.
    buf.extend(std::iter::repeat_n(0u8, 1024));

    buf
}

/// Create a tar with one regular file and one hardlink pointing to it.
fn make_tar_with_hardlinks(first: &str, second: &str, content: &[u8]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());

    // First entry: regular file with content.
    let mut h1 = tar::Header::new_gnu();
    h1.set_size(content.len() as u64);
    h1.set_mode(0o644);
    h1.set_entry_type(tar::EntryType::Regular);
    h1.set_cksum();
    builder.append_data(&mut h1, first, content).unwrap();

    // Second entry: hardlink pointing to the first.
    let mut h2 = tar::Header::new_gnu();
    h2.set_size(0);
    h2.set_entry_type(tar::EntryType::Link);
    h2.set_mode(0o644);
    h2.set_cksum();
    builder.append_link(&mut h2, second, first).unwrap();

    builder.into_inner().unwrap()
}

// ── Tests ───────────────────────────────────────────────────────────────

#[test]
fn process_empty_tar() {
    let tar_data = make_empty_tar();
    let result = LayerCreator::from_tar(&tar_data, &[]).unwrap();

    assert!(result.empty, "empty tar should produce empty layer");
    assert_eq!(result.media_type, OCI_LAYER_MEDIA_TYPE);
    assert!(result.digest.starts_with("sha256:"));
    assert!(result.diff_id.starts_with("sha256:"));
}

#[test]
fn process_single_file() {
    let tar_data = make_tar_with_file("hello.txt", b"hello world");
    let result = LayerCreator::from_tar(&tar_data, &[]).unwrap();

    assert!(!result.empty);
    assert!(result.compressed_size > 0);
    assert!(result.uncompressed_size > 0);
    assert!(result.digest.starts_with("sha256:"));
    assert!(result.diff_id.starts_with("sha256:"));
    // Compressed and uncompressed digests must differ.
    assert_ne!(result.digest, result.diff_id);
}

#[test]
fn path_normalization_strips_leading_slash() {
    let tar_data = make_tar_with_file("/usr/bin/foo", b"binary");
    let result = LayerCreator::from_tar(&tar_data, &[]).unwrap();

    // tar crate strips `./` prefix, but paths are relative (no leading `/`).
    let entries = read_tar_gz_entries(&result.compressed_data);
    assert!(
        entries.iter().any(|p| p == "usr/bin/foo"),
        "expected usr/bin/foo in entries: {entries:?}"
    );
}

#[test]
fn path_normalization_adds_dot_prefix() {
    let tar_data = make_tar_with_file("usr/bin/bar", b"binary");
    let result = LayerCreator::from_tar(&tar_data, &[]).unwrap();

    let entries = read_tar_gz_entries(&result.compressed_data);
    assert!(
        entries.iter().any(|p| p == "usr/bin/bar"),
        "expected usr/bin/bar in entries: {entries:?}"
    );
}

#[test]
fn whiteout_conversion_char_device() {
    // Linux overlayfs encodes deletions as char device (0,0).
    // OCI expects a regular file named .wh.<original>.
    let tar_data = make_tar_with_chardev("etc/passwd");
    let result = LayerCreator::from_tar(&tar_data, &[]).unwrap();

    let entries = read_tar_gz_entries(&result.compressed_data);
    assert!(
        entries.iter().any(|p| p.ends_with(".wh.passwd")),
        "expected .wh.passwd whiteout in entries: {entries:?}"
    );
    // Original char device entry should NOT be present.
    assert!(
        !entries
            .iter()
            .any(|p| p.ends_with("etc/passwd") && !p.contains(".wh.")),
        "char device entry should be converted, not preserved: {entries:?}"
    );
}

#[test]
fn opaque_whiteout_xattr() {
    let tar_data = make_tar_with_opaque_dir("var/cache/");
    let result = LayerCreator::from_tar(&tar_data, &[]).unwrap();

    let entries = read_tar_gz_entries(&result.compressed_data);
    assert!(
        entries.iter().any(|p| p.ends_with(".wh..wh..opq")),
        "expected opaque whiteout marker in entries: {entries:?}"
    );
}

#[test]
fn hardlink_tracking() {
    let tar_data = make_tar_with_hardlinks("usr/bin/a", "usr/bin/b", b"same content");
    let result = LayerCreator::from_tar(&tar_data, &[]).unwrap();

    let (entries, links) = read_tar_gz_entries_with_links(&result.compressed_data);
    // First entry should be a regular file.
    assert!(
        entries.iter().any(|p| p.ends_with("usr/bin/a")),
        "first file should be present: {entries:?}"
    );
    // Second entry should be a hardlink pointing to the first.
    assert!(
        links
            .iter()
            .any(|(path, target)| path.ends_with("usr/bin/b") && target.ends_with("usr/bin/a")),
        "second file should be a hardlink to first: links={links:?}"
    );
}

#[test]
fn excluded_paths_filtered() {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, content) in &[
        ("app/main.py", b"code" as &[u8]),
        ("run/secrets/db_password", b"hunter2"),
        ("app/config.py", b"config"),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append_data(&mut header, name, *content).unwrap();
    }
    let tar_data = builder.into_inner().unwrap();

    let excluded = vec!["run/secrets".to_owned(), "./run/secrets".to_owned()];
    let result = LayerCreator::from_tar(&tar_data, &excluded).unwrap();

    let entries = read_tar_gz_entries(&result.compressed_data);
    assert!(
        !entries.iter().any(|p| p.contains("run/secrets")),
        "excluded path should not appear: {entries:?}"
    );
    assert!(
        entries.iter().any(|p| p.ends_with("app/main.py")),
        "non-excluded paths should remain: {entries:?}"
    );
}

#[test]
fn dual_digest_computation() {
    let tar_data = make_tar_with_file("test.txt", b"digest test data");
    let result = LayerCreator::from_tar(&tar_data, &[]).unwrap();

    // Both digests should be valid sha256 hex strings.
    assert!(result.digest.starts_with("sha256:"));
    assert!(result.diff_id.starts_with("sha256:"));
    assert_eq!(result.digest.len(), 7 + 64); // "sha256:" + 64 hex chars
    assert_eq!(result.diff_id.len(), 7 + 64);

    // Compressed and uncompressed digests must differ.
    assert_ne!(result.digest, result.diff_id);
}

#[test]
fn from_tar_raw_bytes() {
    let tar_data = make_tar_with_file("raw.txt", b"raw content");
    let result = LayerCreator::from_tar(&tar_data, &[]).unwrap();

    assert!(!result.empty);
    assert_eq!(result.media_type, OCI_LAYER_MEDIA_TYPE);
    assert!(result.compressed_size > 0);
    // Compressed data should be valid gzip.
    let mut decoder = flate2::read::GzDecoder::new(&result.compressed_data[..]);
    let mut decompressed = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decompressed).unwrap();
    assert!(!decompressed.is_empty());
}

#[test]
fn media_type_is_oci_gzip() {
    let tar_data = make_tar_with_file("type.txt", b"check media type");
    let result = LayerCreator::from_tar(&tar_data, &[]).unwrap();

    assert_eq!(
        result.media_type,
        "application/vnd.oci.image.layer.v1.tar+gzip"
    );
}

#[test]
fn hash_writer_computes_correct_sha256() {
    let data = b"known test data for hashing";

    let mut writer = HashWriter::new(Vec::new());
    writer.write_all(data).unwrap();
    let (buf, digest) = writer.finish();

    // Verify the data passed through.
    assert_eq!(buf, data);

    // Compute expected digest independently.
    let mut hasher = Sha256::new();
    hasher.update(data);
    let expected = format!("sha256:{:x}", hasher.finalize());
    assert_eq!(digest, expected);
}

#[test]
fn process_snapshot_base64_gzip() {
    let tar_data = make_tar_with_file("snapshot.txt", b"snapshot content");
    let encoded = tar_to_base64_gzip(&tar_data);

    let result = LayerCreator::process_snapshot(&encoded, &[]).unwrap();

    assert!(!result.empty);
    assert!(result.digest.starts_with("sha256:"));
    assert!(result.diff_id.starts_with("sha256:"));

    // Should produce valid gzip output.
    let entries = read_tar_gz_entries(&result.compressed_data);
    assert!(
        entries.iter().any(|p| p.ends_with("snapshot.txt")),
        "expected snapshot.txt in entries: {entries:?}"
    );
}

// ── Decompression helpers for verification ──────────────────────────────

/// Decompress tar.gz and return all entry paths.
fn read_tar_gz_entries(compressed: &[u8]) -> Vec<String> {
    let decoder = flate2::read::GzDecoder::new(compressed);
    let mut archive = tar::Archive::new(decoder);
    archive
        .entries()
        .unwrap()
        .filter_map(|e| {
            let entry = e.ok()?;
            Some(entry.path().ok()?.to_string_lossy().into_owned())
        })
        .collect()
}

/// Decompress tar.gz and return (paths, hardlinks) where hardlinks are (path, link_target).
fn read_tar_gz_entries_with_links(compressed: &[u8]) -> (Vec<String>, Vec<(String, String)>) {
    let decoder = flate2::read::GzDecoder::new(compressed);
    let mut archive = tar::Archive::new(decoder);
    let mut paths = Vec::new();
    let mut links = Vec::new();

    for entry in archive.entries().unwrap() {
        let entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        if entry.header().entry_type() == tar::EntryType::Link {
            let target = entry
                .link_name()
                .unwrap()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            links.push((path.clone(), target));
        }
        paths.push(path);
    }

    (paths, links)
}

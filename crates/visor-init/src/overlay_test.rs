use super::*;
use crate::testutil::tempdir;

// ── Path constant tests ────────────────────────────────────────────────────

#[test]
fn overlay_base_path_is_correct() {
    assert_eq!(OVERLAY_BASE, "/tmp/build-overlay");
}

#[test]
fn overlay_upper_is_under_base() {
    assert!(OVERLAY_UPPER.starts_with(OVERLAY_BASE));
    assert_eq!(OVERLAY_UPPER, "/tmp/build-overlay/upper");
}

#[test]
fn overlay_work_is_under_base() {
    assert!(OVERLAY_WORK.starts_with(OVERLAY_BASE));
    assert_eq!(OVERLAY_WORK, "/tmp/build-overlay/work");
}

#[test]
fn overlay_merged_is_under_base() {
    assert!(OVERLAY_MERGED.starts_with(OVERLAY_BASE));
    assert_eq!(OVERLAY_MERGED, "/tmp/build-overlay/merged");
}

// ── Struct construction tests ──────────────────────────────────────────────

#[test]
fn build_overlay_stores_lower_dir() {
    let overlay = BuildOverlay {
        lower_dir: std::path::PathBuf::from("/rootfs"),
        baseline: std::collections::BTreeMap::new(),
    };
    assert_eq!(overlay.lower_dir, std::path::PathBuf::from("/rootfs"));
}

#[test]
fn build_overlay_debug_format() {
    let overlay = BuildOverlay {
        lower_dir: std::path::PathBuf::from("/mnt/base"),
        baseline: std::collections::BTreeMap::new(),
    };
    let debug = format!("{overlay:?}");
    assert!(
        debug.contains("BuildOverlay"),
        "should contain struct name: {debug}"
    );
    assert!(
        debug.contains("/mnt/base"),
        "should contain lower_dir: {debug}"
    );
}

// ── Hex encode tests ───────────────────────────────────────────────────────

#[test]
fn hex_encode_empty() {
    assert_eq!(hex_encode(&[]), "");
}

#[test]
fn hex_encode_known_bytes() {
    assert_eq!(hex_encode(&[0xab, 0xcd, 0xef]), "abcdef");
}

#[test]
fn hex_encode_zeros() {
    assert_eq!(hex_encode(&[0x00, 0x00]), "0000");
}

// ── Diff snapshot tests ───────────────────────────────────────────────────

fn decode_layer_entries(result: &crate::agent::SnapshotLayerResult) -> Vec<String> {
    use base64::Engine as _;
    use std::io::Read as _;

    let compressed = base64::engine::general_purpose::STANDARD
        .decode(&result.data)
        .expect("decode layer data");
    let mut decoder = flate2::read::GzDecoder::new(&compressed[..]);
    let mut tar_bytes = Vec::new();
    decoder
        .read_to_end(&mut tar_bytes)
        .expect("decompress layer tar");

    let mut archive = tar::Archive::new(&tar_bytes[..]);
    archive
        .entries()
        .expect("read tar entries")
        .map(|entry| {
            entry
                .expect("read tar entry")
                .path()
                .expect("entry path")
                .into_owned()
                .display()
                .to_string()
        })
        .collect()
}

#[test]
fn snapshot_layer_tracks_added_and_deleted_files() {
    let root = tempdir().expect("create root dir");
    std::fs::write(root.path().join("old.txt"), "before\n").expect("write baseline file");

    let mut overlay = BuildOverlay::init(root.path().to_str().expect("utf8 path"))
        .expect("initialize build overlay");

    std::fs::remove_file(root.path().join("old.txt")).expect("remove baseline file");
    std::fs::write(root.path().join("new.txt"), "after\n").expect("write new file");

    let layer = overlay.snapshot_layer().expect("snapshot layer");
    let entries = decode_layer_entries(&layer);

    assert!(
        entries.contains(&"new.txt".to_owned()),
        "new files should be included in diff entries: {entries:?}"
    );
    assert!(
        entries.contains(&".wh.old.txt".to_owned()),
        "deleted files should become OCI whiteouts: {entries:?}"
    );
}

#[test]
fn flatten_refreshes_baseline_for_next_snapshot() {
    let root = tempdir().expect("create root dir");
    std::fs::write(root.path().join("hello.txt"), "v1\n").expect("write baseline file");

    let mut overlay = BuildOverlay::init(root.path().to_str().expect("utf8 path"))
        .expect("initialize build overlay");

    std::fs::write(root.path().join("hello.txt"), "v2\n").expect("update file");
    let first = overlay.snapshot_layer().expect("first snapshot");
    let first_entries = decode_layer_entries(&first);
    assert!(
        first_entries.contains(&"hello.txt".to_owned()),
        "first diff should include modified file: {first_entries:?}"
    );

    overlay.flatten().expect("refresh baseline");

    let second = overlay.snapshot_layer().expect("second snapshot");
    let second_entries = decode_layer_entries(&second);
    assert!(
        second_entries.is_empty(),
        "flatten should reset the baseline for the next diff: {second_entries:?}"
    );
}

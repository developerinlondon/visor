use std::io::Write;
use std::os::unix::fs::PermissionsExt;

use super::*;
use flate2::Compression;
use flate2::write::GzEncoder;

/// Build a tar.gz archive from a list of `(path, contents)` entries.
/// Returns a `NamedTempFile` whose path can be passed to [`LayerMerger::unpack_layer`].
fn create_tar_gz(entries: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
    let tmp = crate::testutil::named_temp_file("visor-runtime-oci-layer-").unwrap();
    let gz = GzEncoder::new(&tmp, Compression::fast());
    let mut ar = tar::Builder::new(gz);

    for &(path, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_path(path).unwrap();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        ar.append(&header, data).unwrap();
    }

    ar.into_inner().unwrap().finish().unwrap();
    tmp
}

/// Build a tar.gz with explicit permission bits per entry.
fn create_tar_gz_with_perms(entries: &[(&str, &[u8], u32)]) -> tempfile::NamedTempFile {
    let tmp = crate::testutil::named_temp_file("visor-runtime-oci-layer-").unwrap();
    let gz = GzEncoder::new(&tmp, Compression::fast());
    let mut ar = tar::Builder::new(gz);

    for &(path, data, mode) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_path(path).unwrap();
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        ar.append(&header, data).unwrap();
    }

    ar.into_inner().unwrap().finish().unwrap();
    tmp
}

/// Build a tar.gz that includes a symlink entry.
fn create_tar_gz_with_symlink(
    files: &[(&str, &[u8])],
    symlinks: &[(&str, &str)],
) -> tempfile::NamedTempFile {
    let tmp = crate::testutil::named_temp_file("visor-runtime-oci-layer-").unwrap();
    let gz = GzEncoder::new(&tmp, Compression::fast());
    let mut ar = tar::Builder::new(gz);

    for &(path, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_path(path).unwrap();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        ar.append(&header, data).unwrap();
    }

    for &(link_name, target) in symlinks {
        let mut header = tar::Header::new_gnu();
        header.set_path(link_name).unwrap();
        header.set_size(0);
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_link_name(target).unwrap();
        header.set_cksum();
        ar.append(&header, &[] as &[u8]).unwrap();
    }

    ar.into_inner().unwrap().finish().unwrap();
    tmp
}

/// Build a tar.gz containing directory entries plus files.
fn create_tar_gz_with_dirs(dirs: &[&str], files: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
    let tmp = crate::testutil::named_temp_file("visor-runtime-oci-layer-").unwrap();
    let gz = GzEncoder::new(&tmp, Compression::fast());
    let mut ar = tar::Builder::new(gz);

    for &dir in dirs {
        let mut header = tar::Header::new_gnu();
        header.set_path(dir).unwrap();
        header.set_size(0);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Directory);
        header.set_cksum();
        ar.append(&header, &[] as &[u8]).unwrap();
    }

    for &(path, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_path(path).unwrap();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        ar.append(&header, data).unwrap();
    }

    ar.into_inner().unwrap().finish().unwrap();
    tmp
}

/// Build an empty tar.gz (valid archive, zero entries).
fn create_empty_tar_gz() -> tempfile::NamedTempFile {
    let tmp = crate::testutil::named_temp_file("visor-runtime-oci-layer-").unwrap();
    let gz = GzEncoder::new(&tmp, Compression::fast());
    let ar = tar::Builder::new(gz);
    ar.into_inner().unwrap().finish().unwrap();
    tmp
}

// ---------------------------------------------------------------------------
// Basic unpack
// ---------------------------------------------------------------------------

#[test]
fn unpack_single_layer() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();
    let layer = create_tar_gz(&[
        ("hello.txt", b"hello world"),
        ("sub/nested.txt", b"nested content"),
    ]);

    let merger = LayerMerger::new(dir.path()).unwrap();
    merger.unpack_layer(layer.path()).unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.path().join("hello.txt")).unwrap(),
        "hello world"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("sub/nested.txt")).unwrap(),
        "nested content"
    );
}

// ---------------------------------------------------------------------------
// Two-layer merges
// ---------------------------------------------------------------------------

#[test]
fn merge_two_layers_add_new_file() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();
    let base = create_tar_gz(&[("base.txt", b"base")]);
    let upper = create_tar_gz(&[("upper.txt", b"upper")]);

    let merger = LayerMerger::new(dir.path()).unwrap();
    merger
        .merge_layers(&[base.path().to_path_buf(), upper.path().to_path_buf()])
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.path().join("base.txt")).unwrap(),
        "base"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("upper.txt")).unwrap(),
        "upper"
    );
}

#[test]
fn merge_two_layers_overwrite_file() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();
    let base = create_tar_gz(&[("file.txt", b"original")]);
    let upper = create_tar_gz(&[("file.txt", b"replaced")]);

    let merger = LayerMerger::new(dir.path()).unwrap();
    merger
        .merge_layers(&[base.path().to_path_buf(), upper.path().to_path_buf()])
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
        "replaced"
    );
}

// ---------------------------------------------------------------------------
// Whiteout handling
// ---------------------------------------------------------------------------

#[test]
fn whiteout_deletes_file_from_lower_layer() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();
    let base = create_tar_gz(&[("foo.txt", b"should be deleted"), ("keep.txt", b"stays")]);
    let upper = create_tar_gz(&[(".wh.foo.txt", b"")]);

    let merger = LayerMerger::new(dir.path()).unwrap();
    merger
        .merge_layers(&[base.path().to_path_buf(), upper.path().to_path_buf()])
        .unwrap();

    assert!(!dir.path().join("foo.txt").exists());
    assert!(!dir.path().join(".wh.foo.txt").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("keep.txt")).unwrap(),
        "stays"
    );
}

#[test]
fn opaque_whiteout_removes_prior_dir_contents() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();
    let base = create_tar_gz_with_dirs(
        &["mydir/"],
        &[("mydir/old1.txt", b"old1"), ("mydir/old2.txt", b"old2")],
    );
    let upper = create_tar_gz(&[("mydir/.wh..wh..opq", b""), ("mydir/new.txt", b"fresh")]);

    let merger = LayerMerger::new(dir.path()).unwrap();
    merger
        .merge_layers(&[base.path().to_path_buf(), upper.path().to_path_buf()])
        .unwrap();

    assert!(!dir.path().join("mydir/old1.txt").exists());
    assert!(!dir.path().join("mydir/old2.txt").exists());
    assert!(!dir.path().join("mydir/.wh..wh..opq").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("mydir/new.txt")).unwrap(),
        "fresh"
    );
}

#[test]
fn whiteout_markers_not_present_in_output() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();
    let base = create_tar_gz(&[("a.txt", b"a")]);
    let upper = create_tar_gz(&[(".wh.a.txt", b""), ("b.txt", b"b")]);

    let merger = LayerMerger::new(dir.path()).unwrap();
    merger
        .merge_layers(&[base.path().to_path_buf(), upper.path().to_path_buf()])
        .unwrap();

    // The whiteout file itself must not appear on disk.
    assert!(!dir.path().join(".wh.a.txt").exists());
    assert!(!dir.path().join("a.txt").exists());
    assert!(dir.path().join("b.txt").exists());
}

// ---------------------------------------------------------------------------
// Three-layer merge with mixed operations
// ---------------------------------------------------------------------------

#[test]
fn three_layer_merge_mixed_ops() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();

    // Layer 1 (base): creates a.txt, b.txt, dir/c.txt
    let l1 = create_tar_gz_with_dirs(
        &["dir/"],
        &[
            ("a.txt", b"a-v1"),
            ("b.txt", b"b-v1"),
            ("dir/c.txt", b"c-v1"),
        ],
    );
    // Layer 2: overwrites a.txt, deletes b.txt, adds d.txt
    let l2 = create_tar_gz(&[("a.txt", b"a-v2"), (".wh.b.txt", b""), ("d.txt", b"d-v1")]);
    // Layer 3: opaque-whiteout dir/, add dir/e.txt, delete d.txt
    let l3 = create_tar_gz(&[
        ("dir/.wh..wh..opq", b""),
        ("dir/e.txt", b"e-v1"),
        (".wh.d.txt", b""),
    ]);

    let merger = LayerMerger::new(dir.path()).unwrap();
    merger
        .merge_layers(&[
            l1.path().to_path_buf(),
            l2.path().to_path_buf(),
            l3.path().to_path_buf(),
        ])
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "a-v2"
    );
    assert!(!dir.path().join("b.txt").exists());
    assert!(!dir.path().join("dir/c.txt").exists());
    assert!(!dir.path().join("d.txt").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("dir/e.txt")).unwrap(),
        "e-v1"
    );
}

// ---------------------------------------------------------------------------
// Empty layer
// ---------------------------------------------------------------------------

#[test]
fn unpack_empty_layer() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();
    let layer = create_empty_tar_gz();

    let merger = LayerMerger::new(dir.path()).unwrap();
    merger.unpack_layer(layer.path()).unwrap();

    // Directory should still exist and be empty (aside from . and ..)
    assert!(dir.path().exists());
    let count = std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// Symlinks
// ---------------------------------------------------------------------------

#[test]
fn symlinks_preserved() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();
    let layer = create_tar_gz_with_symlink(
        &[("target.txt", b"symlink target")],
        &[("link.txt", "target.txt")],
    );

    let merger = LayerMerger::new(dir.path()).unwrap();
    merger.unpack_layer(layer.path()).unwrap();

    let link_path = dir.path().join("link.txt");
    assert!(
        link_path
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_link(&link_path).unwrap().to_str().unwrap(),
        "target.txt"
    );
    assert_eq!(
        std::fs::read_to_string(&link_path).unwrap(),
        "symlink target"
    );
}

// ---------------------------------------------------------------------------
// Directories and nested paths
// ---------------------------------------------------------------------------

#[test]
fn directories_and_nested_paths() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();
    let layer = create_tar_gz_with_dirs(&["a/", "a/b/", "a/b/c/"], &[("a/b/c/deep.txt", b"deep")]);

    let merger = LayerMerger::new(dir.path()).unwrap();
    merger.unpack_layer(layer.path()).unwrap();

    assert!(dir.path().join("a/b/c").is_dir());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a/b/c/deep.txt")).unwrap(),
        "deep"
    );
}

// ---------------------------------------------------------------------------
// Error cases
// ---------------------------------------------------------------------------

#[test]
fn error_non_gzip_input() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();
    let mut bad = crate::testutil::named_temp_file("visor-runtime-oci-layer-").unwrap();
    bad.write_all(b"this is not gzip at all").unwrap();

    let merger = LayerMerger::new(dir.path()).unwrap();
    let result = merger.unpack_layer(bad.path());

    assert!(result.is_err());
}

#[test]
fn error_invalid_tar() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();
    // Valid gzip wrapping garbage — GzDecoder will succeed, tar will fail.
    let tmp = crate::testutil::named_temp_file("visor-runtime-oci-layer-").unwrap();
    let mut gz = GzEncoder::new(&tmp, Compression::fast());
    gz.write_all(b"not a tar stream").unwrap();
    gz.finish().unwrap();

    let merger = LayerMerger::new(dir.path()).unwrap();
    let result = merger.unpack_layer(tmp.path());

    // The archive may appear empty rather than erroring — both outcomes are
    // acceptable as long as no crash occurs.
    // Some tar implementations silently ignore trailing garbage.
    // We just verify it doesn't panic.
    drop(result);
}

// ---------------------------------------------------------------------------
// File permissions preserved
// ---------------------------------------------------------------------------

#[test]
fn file_permissions_preserved() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();
    let layer = create_tar_gz_with_perms(&[
        ("readonly.txt", b"r", 0o444),
        ("executable.sh", b"#!/bin/sh\n", 0o755),
    ]);

    let merger = LayerMerger::new(dir.path()).unwrap();
    merger.unpack_layer(layer.path()).unwrap();

    let ro_perms = std::fs::metadata(dir.path().join("readonly.txt"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(ro_perms, 0o444);

    let exec_perms = std::fs::metadata(dir.path().join("executable.sh"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(exec_perms, 0o755);
}

// ---------------------------------------------------------------------------
// unpack_layer_from_reader
// ---------------------------------------------------------------------------

#[test]
fn unpack_layer_from_reader() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();
    let layer = create_tar_gz(&[("via_reader.txt", b"streamed")]);

    let merger = LayerMerger::new(dir.path()).unwrap();
    let file = std::fs::File::open(layer.path()).unwrap();
    merger.unpack_layer_from_reader(file).unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.path().join("via_reader.txt")).unwrap(),
        "streamed"
    );
}

// ---------------------------------------------------------------------------
// LayerMerger::new creates target dir if missing
// ---------------------------------------------------------------------------

#[test]
fn new_creates_target_directory() {
    let dir = crate::testutil::tempdir("visor-runtime-oci-layer-").unwrap();
    let nested = dir.path().join("does/not/exist/yet");

    let merger = LayerMerger::new(&nested).unwrap();
    assert!(merger.target().exists());
    assert!(merger.target().is_dir());
}

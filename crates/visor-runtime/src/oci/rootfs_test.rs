use std::fs;
use std::io::Read;
use std::os::unix::fs as unix_fs;
use std::path::Path;

use super::*;

/// EXT4 magic number bytes (0xEF53) found at offset 0x438 in the superblock.
const EXT4_MAGIC_OFFSET: u64 = 0x438;
const EXT4_MAGIC: [u8; 2] = [0x53, 0xEF]; // little-endian

/// Read the ext4 magic number from an image file.
fn read_ext4_magic(path: &Path) -> [u8; 2] {
    let mut file = fs::File::open(path).unwrap();
    let mut buf = [0u8; 2];
    std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(EXT4_MAGIC_OFFSET)).unwrap();
    file.read_exact(&mut buf).unwrap();
    buf
}

/// Verify a file is a valid ext4 image by checking the superblock magic.
fn assert_valid_ext4(path: &Path) {
    assert!(
        path.exists(),
        "ext4 image does not exist: {}",
        path.display()
    );
    let magic = read_ext4_magic(path);
    assert_eq!(
        magic, EXT4_MAGIC,
        "not a valid ext4 image (bad magic at 0x438)"
    );
}

// ─── Default options ────────────────────────────────────────────

#[test]
fn default_options_have_expected_values() {
    let opts = RootfsOptions::default();
    assert_eq!(opts.label, "visor-rootfs");
    assert_eq!(opts.extra_size_mb, 256);
    assert!(opts.sparse);
}

#[test]
fn custom_options_override_defaults() {
    let opts = RootfsOptions {
        label: "my-rootfs".into(),
        extra_size_mb: 128,
        sparse: false,
        ..RootfsOptions::default()
    };
    assert_eq!(opts.label, "my-rootfs");
    assert_eq!(opts.extra_size_mb, 128);
    assert!(!opts.sparse);
}

// ─── Size calculation ───────────────────────────────────────────

#[test]
fn calculate_dir_size_empty_directory() {
    let tmp = crate::testutil::tempdir("visor-runtime-oci-rootfs-").unwrap();
    let size = calculate_dir_size(tmp.path()).unwrap();
    assert_eq!(size, 0, "empty directory should report zero content bytes");
}

#[test]
fn calculate_dir_size_with_known_files() {
    let tmp = crate::testutil::tempdir("visor-runtime-oci-rootfs-").unwrap();
    // Write exactly 1000 bytes in two files.
    fs::write(tmp.path().join("a.txt"), vec![0x41u8; 600]).unwrap();
    fs::write(tmp.path().join("b.txt"), vec![0x42u8; 400]).unwrap();
    let size = calculate_dir_size(tmp.path()).unwrap();
    assert_eq!(size, 1000);
}

#[test]
fn calculate_dir_size_with_nested_dirs() {
    let tmp = crate::testutil::tempdir("visor-runtime-oci-rootfs-").unwrap();
    let sub = tmp.path().join("sub/deep");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("file.bin"), vec![0xFFu8; 2048]).unwrap();
    fs::write(tmp.path().join("top.bin"), vec![0xAAu8; 512]).unwrap();
    let size = calculate_dir_size(tmp.path()).unwrap();
    assert_eq!(size, 2560);
}

#[test]
fn calculate_dir_size_skips_symlinks() {
    let tmp = crate::testutil::tempdir("visor-runtime-oci-rootfs-").unwrap();
    fs::write(tmp.path().join("real.txt"), vec![0x41u8; 100]).unwrap();
    unix_fs::symlink(tmp.path().join("real.txt"), tmp.path().join("link.txt")).unwrap();
    let size = calculate_dir_size(tmp.path()).unwrap();
    // Symlink target bytes should NOT be double-counted.
    assert_eq!(size, 100);
}

// ─── Build from empty directory ─────────────────────────────────

#[test]
fn build_ext4_from_empty_directory() {
    let tmp = crate::testutil::tempdir("visor-runtime-oci-rootfs-").unwrap();
    let source = tmp.path().join("rootfs");
    fs::create_dir_all(&source).unwrap();
    let output = tmp.path().join("rootfs.ext4");

    let result = RootfsBuilder::new(&source, &output).build();
    assert!(result.is_ok(), "build failed: {}", result.unwrap_err());

    let out_path = result.unwrap();
    assert_eq!(out_path, output);
    assert_valid_ext4(&out_path);
}

// ─── Build with files ───────────────────────────────────────────

#[test]
fn build_ext4_with_files() {
    let tmp = crate::testutil::tempdir("visor-runtime-oci-rootfs-").unwrap();
    let source = tmp.path().join("rootfs");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("hello.txt"), b"Hello, visor!").unwrap();
    fs::write(source.join("data.bin"), vec![0xABu8; 4096]).unwrap();
    let output = tmp.path().join("image.ext4");

    let path = RootfsBuilder::new(&source, &output).build().unwrap();
    assert_valid_ext4(&path);

    // Image must be at least 64MB (minimum ext4 size with our defaults).
    let meta = fs::metadata(&path).unwrap();
    assert!(
        meta.len() >= 64 * 1024 * 1024,
        "image too small: {} bytes",
        meta.len()
    );
}

// ─── Build with nested tree + symlinks ──────────────────────────

#[test]
fn build_ext4_with_nested_tree() {
    let tmp = crate::testutil::tempdir("visor-runtime-oci-rootfs-").unwrap();
    let source = tmp.path().join("rootfs");

    // Create a realistic rootfs-like structure.
    let dirs = ["bin", "etc", "usr/lib", "var/log"];
    for d in &dirs {
        fs::create_dir_all(source.join(d)).unwrap();
    }
    fs::write(source.join("bin/sh"), vec![0u8; 128]).unwrap();
    fs::write(source.join("etc/hostname"), b"visor-vm").unwrap();
    fs::write(source.join("usr/lib/libc.so"), vec![0u8; 8192]).unwrap();
    unix_fs::symlink("usr/lib", source.join("lib")).unwrap();

    let output = tmp.path().join("nested.ext4");
    let path = RootfsBuilder::new(&source, &output).build().unwrap();
    assert_valid_ext4(&path);
}

// ─── Output file has reasonable apparent size ───────────────────

#[test]
fn output_image_apparent_size_matches_expected() {
    let tmp = crate::testutil::tempdir("visor-runtime-oci-rootfs-").unwrap();
    let source = tmp.path().join("rootfs");
    fs::create_dir_all(&source).unwrap();
    // 1MB of data + 256MB headroom = ~257MB minimum.
    fs::write(source.join("payload"), vec![0xCDu8; 1024 * 1024]).unwrap();
    let output = tmp.path().join("sized.ext4");

    let path = RootfsBuilder::new(&source, &output).build().unwrap();
    let meta = fs::metadata(&path).unwrap();
    // Apparent size should be at least 257MB.
    assert!(
        meta.len() >= 257 * 1024 * 1024,
        "apparent size {} too small",
        meta.len()
    );
}

// ─── Custom options: label and extra_size ───────────────────────

#[test]
fn build_ext4_with_custom_options() {
    let tmp = crate::testutil::tempdir("visor-runtime-oci-rootfs-").unwrap();
    let source = tmp.path().join("rootfs");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("file.txt"), b"custom").unwrap();
    let output = tmp.path().join("custom.ext4");

    let opts = RootfsOptions {
        label: "custom-label".into(),
        extra_size_mb: 32,
        ..RootfsOptions::default()
    };
    let path = RootfsBuilder::with_options(&source, &output, opts)
        .build()
        .unwrap();
    assert_valid_ext4(&path);

    // With 32MB extra the image should still be at least 32MB.
    let meta = fs::metadata(&path).unwrap();
    assert!(
        meta.len() >= 32 * 1024 * 1024,
        "image too small for 32MB extra: {} bytes",
        meta.len()
    );
}

// ─── Error: source directory does not exist ─────────────────────

#[test]
fn error_when_source_dir_does_not_exist() {
    let tmp = crate::testutil::tempdir("visor-runtime-oci-rootfs-").unwrap();
    let missing = tmp.path().join("does-not-exist");
    let output = tmp.path().join("out.ext4");

    let err = RootfsBuilder::new(&missing, &output).build();
    assert!(err.is_err());
    let msg = format!("{:#}", err.unwrap_err());
    assert!(
        msg.contains("source directory") || msg.contains("does not exist"),
        "unexpected error message: {msg}"
    );
}

// ─── Verify with `file` command ─────────────────────────────────

#[test]
fn file_command_identifies_ext4() {
    let tmp = crate::testutil::tempdir("visor-runtime-oci-rootfs-").unwrap();
    let source = tmp.path().join("rootfs");
    fs::create_dir_all(&source).unwrap();
    let output = tmp.path().join("check.ext4");

    RootfsBuilder::new(&source, &output).build().unwrap();

    let out = std::process::Command::new("file")
        .arg(&output)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ext4") || stdout.contains("ext2"),
        "`file` did not detect ext4: {stdout}"
    );
}

// ─── Build returns output path ──────────────────────────────────

#[test]
fn build_returns_output_path() {
    let tmp = crate::testutil::tempdir("visor-runtime-oci-rootfs-").unwrap();
    let source = tmp.path().join("rootfs");
    fs::create_dir_all(&source).unwrap();
    let output = tmp.path().join("returned.ext4");

    let path = RootfsBuilder::new(&source, &output).build().unwrap();
    assert_eq!(path, output);
}

// ─── Minimum image size enforcement ─────────────────────────────

#[test]
fn minimum_image_size_enforced() {
    let tmp = crate::testutil::tempdir("visor-runtime-oci-rootfs-").unwrap();
    let source = tmp.path().join("rootfs");
    fs::create_dir_all(&source).unwrap();
    // Even with nothing inside, the image should be at least MIN_IMAGE_SIZE_MB.
    let output = tmp.path().join("min.ext4");

    let opts = RootfsOptions {
        extra_size_mb: 0,
        ..RootfsOptions::default()
    };
    let path = RootfsBuilder::with_options(&source, &output, opts)
        .build()
        .unwrap();
    let meta = fs::metadata(&path).unwrap();
    assert!(
        meta.len() >= 64 * 1024 * 1024,
        "image below minimum: {} bytes",
        meta.len()
    );
}

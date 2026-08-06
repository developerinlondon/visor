use std::path::{Path, PathBuf};

pub(crate) fn named_temp_file(prefix: &str) -> std::io::Result<tempfile::NamedTempFile> {
    let root = workspace_test_temp_root();
    std::fs::create_dir_all(&root)?;
    tempfile::Builder::new()
        .prefix(prefix)
        .tempfile_in(root)
        .map_err(std::io::Error::from)
}

pub(crate) fn tempdir(prefix: &str) -> std::io::Result<tempfile::TempDir> {
    let root = workspace_test_temp_root();
    std::fs::create_dir_all(&root)?;
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .map_err(std::io::Error::from)
}

fn workspace_test_temp_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp")
        .join("visor-runtime-tests")
}

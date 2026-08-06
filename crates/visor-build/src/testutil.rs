use std::path::PathBuf;

use tempfile::TempDir;

pub(crate) fn tempdir() -> std::io::Result<TempDir> {
    let root = workspace_test_temp_root();
    std::fs::create_dir_all(&root)?;
    tempfile::Builder::new()
        .prefix("visor-build-")
        .tempdir_in(root)
        .map_err(std::io::Error::from)
}

fn workspace_test_temp_root() -> PathBuf {
    std::env::current_dir()
        .unwrap()
        .join(".tmp")
        .join("visor-build-tests")
}

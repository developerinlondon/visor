use std::ffi::OsString;
use std::path::{Path, PathBuf};

const TEST_TEMP_ROOT_ENV: &str = "VISOR_TEST_TEMP_ROOT";

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
    test_temp_root_from(std::env::var_os(TEST_TEMP_ROOT_ENV))
}

fn test_temp_root_from(override_path: Option<OsString>) -> PathBuf {
    override_path
        .filter(|path| !path.is_empty())
        .map_or_else(
            || {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(".tmp")
                    .join("visor-vmm-tests")
            },
            PathBuf::from,
        )
}

#[cfg(test)]
#[path = "testutil_test.rs"]
mod tests;

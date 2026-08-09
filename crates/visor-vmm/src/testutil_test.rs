use std::ffi::OsString;
use std::path::PathBuf;

use super::*;

#[test]
fn test_temp_root_prefers_runtime_override() {
    let root = test_temp_root_from(Some(OsString::from("/tmp/visor-tests")));

    assert_eq!(root, PathBuf::from("/tmp/visor-tests"));
}

#[test]
fn test_temp_root_uses_workspace_for_empty_override() {
    let root = test_temp_root_from(Some(OsString::new()));

    assert_eq!(
        root,
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".tmp")
            .join("visor-vmm-tests")
    );
}

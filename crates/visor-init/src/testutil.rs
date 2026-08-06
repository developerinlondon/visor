pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp")
        .join("visor-init-tests");
    std::fs::create_dir_all(&root)?;
    tempfile::Builder::new()
        .prefix("visor-init-")
        .tempdir_in(root)
        .map_err(std::io::Error::from)
}

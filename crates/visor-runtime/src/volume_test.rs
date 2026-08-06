use super::*;

fn test_manager() -> (VolumeManager, tempfile::TempDir) {
    let dir = crate::testutil::tempdir("visor-runtime-volume-").unwrap();
    let mgr = VolumeManager::new(dir.path()).unwrap();
    (mgr, dir)
}

#[test]
fn create_volume_produces_ext4_file() {
    let (mgr, _dir) = test_manager();
    let info = mgr.create("test-vol", 10).unwrap();

    assert_eq!(info.name, "test-vol");
    assert_eq!(info.size_mib, 10);
    assert!(std::path::Path::new(&info.path).exists());
}

#[test]
fn create_volume_writes_metadata_json() {
    let (mgr, dir) = test_manager();
    mgr.create("myvol", 10).unwrap();

    let meta_path = dir.path().join("myvol.json");
    assert!(meta_path.exists());

    let data = std::fs::read_to_string(&meta_path).unwrap();
    let info: VolumeInfo = serde_json::from_str(&data).unwrap();
    assert_eq!(info.name, "myvol");
    assert_eq!(info.size_mib, 10);
}

#[test]
fn create_duplicate_fails() {
    let (mgr, _dir) = test_manager();
    mgr.create("dup", 10).unwrap();

    let result = mgr.create("dup", 10);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("already exists"));
}

#[test]
fn create_invalid_name_fails() {
    let (mgr, _dir) = test_manager();

    // Empty name.
    assert!(mgr.create("", 10).is_err());
    // Spaces.
    assert!(mgr.create("has spaces", 10).is_err());
    // Slashes.
    assert!(mgr.create("has/slash", 10).is_err());
    // Starts with dash.
    assert!(mgr.create("-starts-dash", 10).is_err());
}

#[test]
fn create_zero_size_fails() {
    let (mgr, _dir) = test_manager();

    let result = mgr.create("zero", 0);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("greater than 0"));
}

#[test]
fn list_empty_directory() {
    let (mgr, _dir) = test_manager();
    let volumes = mgr.list().unwrap();
    assert!(volumes.is_empty());
}

#[test]
fn list_returns_all_volumes_sorted() {
    let (mgr, _dir) = test_manager();
    mgr.create("beta", 20).unwrap();
    mgr.create("alpha", 10).unwrap();

    let volumes = mgr.list().unwrap();
    assert_eq!(volumes.len(), 2);
    assert_eq!(volumes[0].name, "alpha");
    assert_eq!(volumes[1].name, "beta");
}

#[test]
fn inspect_existing_volume() {
    let (mgr, _dir) = test_manager();
    mgr.create("vol1", 10).unwrap();

    let info = mgr.inspect("vol1").unwrap();
    assert_eq!(info.name, "vol1");
    assert_eq!(info.size_mib, 10);
}

#[test]
fn inspect_missing_volume_fails() {
    let (mgr, _dir) = test_manager();

    let result = mgr.inspect("nope");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn remove_deletes_both_files() {
    let (mgr, dir) = test_manager();
    mgr.create("removeme", 10).unwrap();

    assert!(dir.path().join("removeme.ext4").exists());
    assert!(dir.path().join("removeme.json").exists());

    mgr.remove("removeme").unwrap();

    assert!(!dir.path().join("removeme.ext4").exists());
    assert!(!dir.path().join("removeme.json").exists());
}

#[test]
fn remove_missing_volume_fails() {
    let (mgr, _dir) = test_manager();

    let result = mgr.remove("ghost");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn resize_grows_volume() {
    let (mgr, _dir) = test_manager();
    mgr.create("growme", 10).unwrap();

    let info = mgr.resize("growme", 20).unwrap();
    assert_eq!(info.size_mib, 20);

    // Verify metadata was persisted.
    let info = mgr.inspect("growme").unwrap();
    assert_eq!(info.size_mib, 20);
}

#[test]
fn resize_rejects_shrink() {
    let (mgr, _dir) = test_manager();
    mgr.create("noshrink", 20).unwrap();

    let result = mgr.resize("noshrink", 10);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("must be larger"));
}

#[test]
fn resize_rejects_same_size() {
    let (mgr, _dir) = test_manager();
    mgr.create("samesize", 20).unwrap();

    let result = mgr.resize("samesize", 20);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("must be larger"));
}

#[test]
fn resize_missing_volume_fails() {
    let (mgr, _dir) = test_manager();

    let result = mgr.resize("nonexistent", 10);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn valid_names_accepted() {
    let (mgr, _dir) = test_manager();

    // All of these should succeed.
    assert!(mgr.create("simple", 10).is_ok());
    assert!(mgr.create("with-dashes", 10).is_ok());
    assert!(mgr.create("with_underscores", 10).is_ok());
    assert!(mgr.create("Mixed123", 10).is_ok());
    assert!(mgr.create("a", 10).is_ok());
}

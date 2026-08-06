use std::path::Path;

use super::*;

#[test]
fn visor_home_dir_from_env_prefers_visor_home() {
    let path = visor_home_dir_from_env(
        Some(Path::new("/var/lib/visor")),
        Some(Path::new("/home/dev")),
    )
    .unwrap();
    assert_eq!(path, Path::new("/var/lib/visor"));
}

#[test]
fn visor_home_dir_from_env_falls_back_to_home_dot_visor() {
    let path = visor_home_dir_from_env(None, Some(Path::new("/home/dev"))).unwrap();
    assert_eq!(path, Path::new("/home/dev/.visor"));
}

#[test]
fn visor_home_dir_from_env_requires_explicit_home_source() {
    let err = visor_home_dir_from_env(None, None).unwrap_err();
    assert!(
        err.to_string()
            .contains("VISOR_HOME or HOME environment variable must be set")
    );
}

#[test]
fn persistent_subdir_from_env_appends_subdirectory() {
    let path = persistent_subdir_from_env(
        "state",
        Some(Path::new("/var/lib/visor")),
        Some(Path::new("/home/dev")),
    )
    .unwrap();
    assert_eq!(path, Path::new("/var/lib/visor/state"));
}

#[test]
fn daemon_log_path_from_env_uses_visor_home() {
    let path = daemon_log_path_from_env(Some(Path::new("/var/lib/visor")), None).unwrap();
    assert_eq!(path, Path::new("/var/lib/visor/visor-daemon.log"));
}

#[test]
fn best_effort_persistent_subdir_from_env_uses_temp_dir_without_home() {
    let path = best_effort_persistent_subdir_from_env("images", None, None);
    assert_eq!(path, std::env::temp_dir().join(".visor").join("images"));
}

use super::*;

fn make_volume(host: &str, guest: &str, read_only: bool) -> VolumeConfig {
    VolumeConfig {
        host_path: host.to_owned(),
        guest_path: guest.to_owned(),
        read_only,
        mount_tag: String::new(),
        device_path: String::new(),
        fs_type: String::new(),
    }
}

#[test]
fn validate_volume_accepts_valid_config() {
    let vol = make_volume("/host/data", "/guest/data", false);
    assert!(validate_volume(&vol).is_ok());
}

#[test]
fn validate_volume_accepts_read_only() {
    let vol = make_volume("/host/data", "/guest/data", true);
    assert!(validate_volume(&vol).is_ok());
}

#[test]
fn validate_volume_rejects_empty_host_path() {
    let vol = make_volume("", "/guest/data", false);
    let err = validate_volume(&vol).unwrap_err();
    assert!(
        err.to_string().contains("mount source must not be empty"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_volume_rejects_empty_guest_path() {
    let vol = make_volume("/host/data", "", false);
    let err = validate_volume(&vol).unwrap_err();
    assert!(
        err.to_string().contains("guest_path must not be empty"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_volume_rejects_relative_guest_path() {
    let vol = make_volume("/host/data", "relative/path", false);
    let err = validate_volume(&vol).unwrap_err();
    assert!(
        err.to_string().contains("must be absolute"),
        "unexpected error: {err}"
    );
}

#[test]
fn mount_volumes_succeeds_with_empty_list() {
    assert!(mount_volumes(&[]).is_ok());
}

#[test]
fn default_volume_config_has_empty_fields() {
    let vol = VolumeConfig::default();
    assert!(vol.host_path.is_empty());
    assert!(vol.guest_path.is_empty());
    assert!(!vol.read_only);
}

#[test]
fn validate_volume_rejects_default_config() {
    let vol = VolumeConfig::default();
    assert!(validate_volume(&vol).is_err());
}

#[test]
fn validate_volume_accepts_mount_tag_without_host_path() {
    let vol = VolumeConfig {
        host_path: String::new(),
        guest_path: "/workspace".to_owned(),
        read_only: true,
        mount_tag: "visor-fs-0".to_owned(),
        device_path: String::new(),
        fs_type: String::new(),
    };
    assert!(validate_volume(&vol).is_ok());
}

#[test]
fn validate_volume_accepts_block_device_without_host_path() {
    let vol = VolumeConfig {
        host_path: String::new(),
        guest_path: "/var/lib/data".to_owned(),
        read_only: false,
        mount_tag: String::new(),
        device_path: "/dev/vdb".to_owned(),
        fs_type: "ext4".to_owned(),
    };
    assert!(validate_volume(&vol).is_ok());
}

use super::*;

// ── systemd unit generation ─────────────────────────────────────

#[test]
fn test_generate_systemd_unit() {
    let args = ServiceInstallArgs {
        listen: "0.0.0.0:7800".to_owned(),
        user: false,
    };
    let unit = generate_systemd_unit(&args);
    assert!(unit.contains("[Unit]"));
    assert!(unit.contains("[Service]"));
    assert!(unit.contains("[Install]"));
    assert!(unit.contains("visor"));
}

#[test]
fn test_systemd_unit_has_required_sections() {
    let args = ServiceInstallArgs {
        listen: "0.0.0.0:7800".to_owned(),
        user: false,
    };
    let unit = generate_systemd_unit(&args);
    let sections = ["[Unit]", "[Service]", "[Install]"];
    for section in &sections {
        assert!(
            unit.contains(section),
            "systemd unit missing section: {section}"
        );
    }
}

#[test]
fn test_systemd_unit_exec_start() {
    let args = ServiceInstallArgs {
        listen: "0.0.0.0:9000".to_owned(),
        user: false,
    };
    let unit = generate_systemd_unit(&args);
    assert!(
        unit.contains("ExecStart=/usr/local/bin/visor start --listen 0.0.0.0:9000 --foreground"),
        "ExecStart should point to visor binary with correct listen addr, got:\n{unit}"
    );
}

#[test]
fn test_systemd_unit_restart_policy() {
    let args = ServiceInstallArgs {
        listen: "0.0.0.0:7800".to_owned(),
        user: false,
    };
    let unit = generate_systemd_unit(&args);
    assert!(
        unit.contains("Restart=on-failure"),
        "systemd unit should have Restart=on-failure, got:\n{unit}"
    );
}

// ── launchd plist generation ────────────────────────────────────

#[test]
fn test_generate_launchd_plist() {
    let args = ServiceInstallArgs {
        listen: "0.0.0.0:7800".to_owned(),
        user: false,
    };
    let plist = generate_launchd_plist(&args);
    assert!(plist.contains("<?xml"));
    assert!(plist.contains("plist"));
    assert!(plist.contains("rs.visor.daemon"));
}

#[test]
fn test_launchd_plist_has_label() {
    let args = ServiceInstallArgs {
        listen: "0.0.0.0:7800".to_owned(),
        user: false,
    };
    let plist = generate_launchd_plist(&args);
    assert!(
        plist.contains("<string>rs.visor.daemon</string>"),
        "launchd plist should have label rs.visor.daemon, got:\n{plist}"
    );
}

#[test]
fn test_launchd_plist_program() {
    let args = ServiceInstallArgs {
        listen: "0.0.0.0:7800".to_owned(),
        user: false,
    };
    let plist = generate_launchd_plist(&args);
    assert!(
        plist.contains("<string>/usr/local/bin/visor</string>"),
        "plist should contain visor binary path"
    );
    assert!(
        plist.contains("<string>start</string>"),
        "plist ProgramArguments should contain 'start'"
    );
    assert!(
        plist.contains("<string>--listen</string>"),
        "plist ProgramArguments should contain '--listen'"
    );
    assert!(
        plist.contains("<string>0.0.0.0:7800</string>"),
        "plist ProgramArguments should contain listen address"
    );
    assert!(
        plist.contains("<string>--foreground</string>"),
        "plist ProgramArguments should contain '--foreground'"
    );
}

// ── platform detection ──────────────────────────────────────────

#[test]
fn test_detect_platform_linux() {
    let platform = detect_platform().expect("should detect platform");
    // We are on Linux (AX41), so expect Systemd.
    if cfg!(target_os = "linux") {
        assert_eq!(platform, ServicePlatform::Systemd);
    }
}

// ── install paths ───────────────────────────────────────────────

#[test]
fn test_install_service_path_systemd() {
    let path = service_file_path(ServicePlatform::Systemd, false);
    assert_eq!(
        path.to_str().expect("should be valid utf-8"),
        "/etc/systemd/system/visor.service"
    );
}

#[test]
fn test_install_service_path_launchd() {
    let home = std::env::var("HOME").expect("HOME should be set");
    let path = service_file_path(ServicePlatform::Launchd, false);
    let expected = format!("{home}/Library/LaunchAgents/rs.visor.daemon.plist");
    assert_eq!(path.to_str().expect("should be valid utf-8"), expected);
}

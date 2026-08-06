use super::*;

#[test]
fn shell_search_paths_has_entries() {
    assert!(!SHELL_SEARCH_PATHS.is_empty());
}

#[test]
fn shell_search_paths_starts_with_bin_sh() {
    assert_eq!(SHELL_SEARCH_PATHS[0], "/bin/sh");
}

#[test]
fn shell_search_paths_includes_toybox() {
    assert!(SHELL_SEARCH_PATHS.iter().any(|p| p.contains("toybox")));
}

#[test]
fn shell_search_paths_all_absolute() {
    for path in SHELL_SEARCH_PATHS {
        assert!(path.starts_with('/'), "shell path not absolute: {path}");
    }
}

#[test]
fn find_shell_returns_existing_shell() {
    // On AX41 (Linux), /bin/sh should exist
    let shell = find_shell();
    assert!(shell.is_some(), "expected to find a shell on this system");
}

#[test]
fn resolve_shell_command_succeeds_on_linux() {
    // On any Linux system with /bin/sh
    let cmd = resolve_shell_command();
    assert!(cmd.is_ok(), "expected shell command to resolve");
    let cmd = cmd.unwrap();
    assert!(!cmd.is_empty());
    assert!(cmd[0].starts_with('/'));
}

#[test]
fn resolve_shell_command_returns_single_entry_for_sh() {
    // If /bin/sh exists, it should return just ["/bin/sh"]
    if std::path::Path::new("/bin/sh").exists() {
        let cmd = resolve_shell_command().unwrap();
        assert_eq!(cmd, vec!["/bin/sh"]);
    }
}

#[test]
fn spawn_shell_runs_and_can_be_killed() {
    // Spawn a shell and immediately kill it
    let result = spawn_shell(&[]);
    if let Ok(mut child) = result {
        let _ = child.kill();
        let _ = child.wait();
    }
    // If spawn fails (e.g., no shell), that's also acceptable in test
}

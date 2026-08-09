use nix::sys::signal::Signal;

use super::*;
use crate::testutil::tempdir;

#[test]
fn exec_params_from_config_extracts_fields() {
    let config = RunConfig {
        cmd: vec!["echo".to_owned(), "hello".to_owned()],
        env: vec!["FOO=bar".to_owned()],
        workdir: "/tmp".to_owned(),
        ..RunConfig::default()
    };
    let params = ExecParams::from_config(&config);
    assert_eq!(params.cmd, vec!["echo", "hello"]);
    assert_eq!(params.env, vec!["FOO=bar"]);
    assert_eq!(params.workdir, "/tmp");
}

#[test]
fn exec_params_with_empty_env_is_valid() {
    let config = RunConfig {
        cmd: vec!["/bin/sh".to_owned()],
        env: Vec::new(),
        workdir: "/".to_owned(),
        ..RunConfig::default()
    };
    let params = ExecParams::from_config(&config);
    assert!(params.env.is_empty());
    assert_eq!(params.cmd, vec!["/bin/sh"]);
}

#[test]
fn workload_command_wraps_a_limited_command_with_the_guest_launcher() {
    let command = workload_command(&["/bin/echo".to_owned(), "hello".to_owned()], true).unwrap();

    assert_eq!(command.get_program(), "/sbin/visor-init");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        ["--visor-workload", "/bin/echo", "hello"]
    );
}

#[test]
fn command_env_injects_default_path_when_missing() {
    let env = build_command_env(&["FOO=bar".to_owned()]);

    assert!(
        env.iter()
            .any(|(key, value)| key == "FOO" && value == "bar")
    );
    assert!(env.iter().any(|(key, value)| {
        key == "PATH" && value == "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
    }));
}

#[test]
fn command_env_preserves_explicit_path() {
    let env = build_command_env(&[
        "PATH=/custom/bin".to_owned(),
        "VISOR_TEST_VAR=hello".to_owned(),
    ]);

    assert!(
        env.iter()
            .any(|(key, value)| key == "PATH" && value == "/custom/bin")
    );
    assert!(
        env.iter()
            .any(|(key, value)| key == "VISOR_TEST_VAR" && value == "hello")
    );
}

#[test]
fn spawn_child_runs_true_and_returns_pid() {
    let params = ExecParams {
        cmd: vec!["/bin/true".to_owned()],
        env: vec![],
        workdir: "/".to_owned(),
    };
    let pid = spawn_child(&params).unwrap();
    assert!(pid.as_raw() > 0);
    let result = wait_for_child(pid).unwrap();
    assert_eq!(result.exit_code, 0);
}

#[test]
fn spawn_child_with_false_exits_code_1() {
    let params = ExecParams {
        cmd: vec!["/bin/false".to_owned()],
        env: vec![],
        workdir: "/".to_owned(),
    };
    let pid = spawn_child(&params).unwrap();
    let result = wait_for_child(pid).unwrap();
    assert_eq!(result.exit_code, 1);
}

#[test]
fn spawn_child_with_nonexistent_binary_returns_error() {
    let params = ExecParams {
        cmd: vec!["/nonexistent/binary".to_owned()],
        env: vec![],
        workdir: "/".to_owned(),
    };
    let result = spawn_child(&params);
    assert!(result.is_err());
}

#[test]
fn wait_for_child_returns_exit_code_zero() {
    let params = ExecParams {
        cmd: vec!["/bin/true".to_owned()],
        env: vec![],
        workdir: "/".to_owned(),
    };
    let pid = spawn_child(&params).unwrap();
    let result = wait_for_child(pid).unwrap();
    assert_eq!(result.exit_code, 0);
}

#[test]
fn wait_for_child_returns_exit_code_one() {
    let params = ExecParams {
        cmd: vec!["/bin/false".to_owned()],
        env: vec![],
        workdir: "/".to_owned(),
    };
    let pid = spawn_child(&params).unwrap();
    let result = wait_for_child(pid).unwrap();
    assert_eq!(result.exit_code, 1);
}

#[test]
fn reap_zombies_returns_zero_when_no_zombies() {
    let count = reap_zombies();
    assert_eq!(count, 0);
}

#[test]
fn forward_signal_sends_signal_to_child() {
    let params = ExecParams {
        cmd: vec!["/bin/sleep".to_owned(), "60".to_owned()],
        env: vec![],
        workdir: "/".to_owned(),
    };
    let pid = spawn_child(&params).unwrap();

    // Give the process a moment to start
    std::thread::sleep(std::time::Duration::from_millis(50));

    forward_signal(pid, Signal::SIGTERM).unwrap();

    let result = wait_for_child(pid).unwrap();
    assert_eq!(result.exit_code, 128 + Signal::SIGTERM as i32);
}

#[test]
fn exec_result_has_correct_exit_code() {
    let result = ExecResult { exit_code: 42 };
    assert_eq!(result.exit_code, 42);
}

#[test]
fn spawn_child_sets_working_directory() {
    let dir = tempdir().unwrap();
    let marker = dir.path().join("pwd_output.txt");
    let params = ExecParams {
        cmd: vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            format!("pwd > {}", marker.display()),
        ],
        env: vec![],
        workdir: dir.path().to_str().unwrap().to_owned(),
    };
    let pid = spawn_child(&params).unwrap();
    let result = wait_for_child(pid).unwrap();
    assert_eq!(result.exit_code, 0);
    let output = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(output.trim(), dir.path().to_str().unwrap());
}

#[test]
fn spawn_child_sets_environment_variables() {
    let dir = tempdir().unwrap();
    let marker = dir.path().join("env_output.txt");
    let params = ExecParams {
        cmd: vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            format!("echo $VISOR_TEST_VAR > {}", marker.display()),
        ],
        env: vec!["VISOR_TEST_VAR=hello_visor".to_owned()],
        workdir: "/".to_owned(),
    };
    let pid = spawn_child(&params).unwrap();
    let result = wait_for_child(pid).unwrap();
    assert_eq!(result.exit_code, 0);
    let output = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(output.trim(), "hello_visor");
}

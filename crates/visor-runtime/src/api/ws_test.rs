use super::*;

use crate::api::sse::VmEvent;

#[test]
fn vm_event_for_attach_creates_correctly() {
    let event = VmEvent::new("vm.attached", "vm-ws-1");
    assert_eq!(event.event_type, "vm.attached");
    assert_eq!(event.vm_id, "vm-ws-1");
}

// ── WsMessage serialization ─────────────────────────────────────

#[test]
fn ws_message_stdin_serializes_correctly() {
    let msg = WsMessage::Stdin {
        data: "ls -la\n".to_owned(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["type"], "stdin");
    assert_eq!(parsed["data"], "ls -la\n");
}

#[test]
fn ws_message_stdout_serializes_correctly() {
    let msg = WsMessage::Stdout {
        data: "total 42\n".to_owned(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["type"], "stdout");
    assert_eq!(parsed["data"], "total 42\n");
}

#[test]
fn ws_message_stderr_serializes_correctly() {
    let msg = WsMessage::Stderr {
        data: "error: not found\n".to_owned(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["type"], "stderr");
    assert_eq!(parsed["data"], "error: not found\n");
}

#[test]
fn ws_message_exit_serializes_correctly() {
    let msg = WsMessage::Exit { code: 0 };
    let json = serde_json::to_string(&msg).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["type"], "exit");
    assert_eq!(parsed["code"], 0);
}

#[test]
fn ws_message_error_serializes_correctly() {
    let msg = WsMessage::Error {
        data: "vm not found".to_owned(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["type"], "error");
    assert_eq!(parsed["data"], "vm not found");
}

#[test]
fn ws_message_info_serializes_correctly() {
    let msg = WsMessage::Info {
        data: "connected".to_owned(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["type"], "info");
    assert_eq!(parsed["data"], "connected");
}

// ── WsMessage deserialization ───────────────────────────────────

#[test]
fn ws_message_stdin_deserializes_correctly() {
    let json = r#"{"type":"stdin","data":"echo hello\n"}"#;
    let msg: WsMessage = serde_json::from_str(json).unwrap();
    match msg {
        WsMessage::Stdin { data } => assert_eq!(data, "echo hello\n"),
        other => panic!("expected Stdin, got {other:?}"),
    }
}

#[test]
fn ws_message_exit_deserializes_correctly() {
    let json = r#"{"type":"exit","code":127}"#;
    let msg: WsMessage = serde_json::from_str(json).unwrap();
    match msg {
        WsMessage::Exit { code } => assert_eq!(code, 127),
        other => panic!("expected Exit, got {other:?}"),
    }
}

#[test]
fn ws_message_round_trips() {
    let original = WsMessage::Stdout {
        data: "hello world".to_owned(),
    };
    let json = serde_json::to_string(&original).unwrap();
    let recovered: WsMessage = serde_json::from_str(&json).unwrap();
    match recovered {
        WsMessage::Stdout { data } => assert_eq!(data, "hello world"),
        other => panic!("expected Stdout, got {other:?}"),
    }
}

#[test]
fn ws_message_unknown_type_fails() {
    let json = r#"{"type":"unknown","data":"x"}"#;
    let result = serde_json::from_str::<WsMessage>(json);
    assert!(result.is_err());
}

#[test]
fn shell_exec_request_preserves_shell_syntax() {
    let request = shell_exec_request(r#"echo "hello world" | wc -c && echo done"#);

    assert_eq!(
        request.cmd,
        vec![
            "/bin/sh".to_owned(),
            "-lc".to_owned(),
            r#"echo "hello world" | wc -c && echo done"#.to_owned(),
        ]
    );
}

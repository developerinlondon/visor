use super::*;

// ── URL scheme conversion ───────────────────────────────────────

#[test]
fn convert_http_to_ws() {
    let ws_url = to_ws_url("http://127.0.0.1:7800");
    assert_eq!(ws_url, "ws://127.0.0.1:7800");
}

#[test]
fn convert_https_to_wss() {
    let ws_url = to_ws_url("https://example.com:443");
    assert_eq!(ws_url, "wss://example.com:443");
}

#[test]
fn convert_preserves_non_http_scheme() {
    let ws_url = to_ws_url("ws://already-ws:8080");
    assert_eq!(ws_url, "ws://already-ws:8080");
}

#[test]
fn convert_http_no_port() {
    let ws_url = to_ws_url("http://localhost");
    assert_eq!(ws_url, "ws://localhost");
}

// ── WsMessage protocol from CLI perspective ─────────────────────

#[test]
fn stdin_message_format() {
    let msg = crate::api::ws::WsMessage::Stdin {
        data: "ls -la\n".to_owned(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""type":"stdin""#));
    assert!(json.contains(r#""data":"ls -la\n""#));
}

#[test]
fn stdout_message_parses() {
    let json = r#"{"type":"stdout","data":"file1.txt\nfile2.txt\n"}"#;
    let msg: crate::api::ws::WsMessage = serde_json::from_str(json).unwrap();
    match msg {
        crate::api::ws::WsMessage::Stdout { data } => {
            assert_eq!(data, "file1.txt\nfile2.txt\n");
        }
        other => panic!("expected Stdout, got {other:?}"),
    }
}

#[test]
fn exit_message_parses() {
    let json = r#"{"type":"exit","code":0}"#;
    let msg: crate::api::ws::WsMessage = serde_json::from_str(json).unwrap();
    match msg {
        crate::api::ws::WsMessage::Exit { code } => assert_eq!(code, 0),
        other => panic!("expected Exit, got {other:?}"),
    }
}

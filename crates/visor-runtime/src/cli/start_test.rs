use super::*;

#[tokio::test]
async fn is_daemon_running_returns_false_for_unused_port() {
    // Port 19999 should have nothing listening
    let result = is_daemon_running("127.0.0.1:19999").await;
    assert!(!result);
}

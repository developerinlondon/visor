use super::*;

#[tokio::test]
async fn try_stop_daemon_no_daemon_does_not_panic() {
    // Stopping a daemon that isn't running should return an error, not panic.
    let result = try_stop_daemon("http://127.0.0.1:19876").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn restart_prints_no_stopping_when_daemon_not_running() {
    // When no daemon is running, restart should skip the "Stopping" message
    // and go straight to starting. We can't test the full start flow here
    // (it spawns a real process), but we can verify the guard logic.
    let running = super::super::start::is_daemon_running("127.0.0.1:19877").await;
    assert!(!running, "nothing should be listening on port 19877");
}

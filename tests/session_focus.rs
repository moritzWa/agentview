#![cfg(unix)]

use std::os::unix::net::UnixDatagram;
use std::process::Command;
use std::time::Duration;

#[test]
fn notification_command_routes_the_exact_id_and_reports_no_dashboard() {
    let state = tempfile::tempdir().unwrap();
    let dir = state.path().join("agentview/focus");
    std::fs::create_dir_all(&dir).unwrap();
    // A non-PID socket exercises IPC without activating any host application.
    let socket = UnixDatagram::bind(dir.join("fixture.sock")).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_agentview"))
            .args(["sessions", "open", "opencode:host:ses_exact"])
            .env("XDG_STATE_HOME", state.path())
            .output()
            .unwrap()
    };
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut bytes = [0; 512];
    let count = socket.recv(&mut bytes).unwrap();
    assert_eq!(&bytes[..count], b"opencode:host:ses_exact");
    drop(socket);
    let output = run();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no running Agent View dashboard"));
}

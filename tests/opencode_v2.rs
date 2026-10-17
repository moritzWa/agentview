#![cfg(any(target_os = "linux", target_os = "macos"))]

use agentview::adapters::{DiscoveryRequest, OpenCodeSource, SessionSource};
use agentview::opencode_supervisor::OpenCodeSupervisor;
use std::sync::Arc;

struct Shutdown(Arc<OpenCodeSupervisor>);
impl Drop for Shutdown {
    fn drop(&mut self) {
        let _ = self.0.shutdown_server();
    }
}

/// Run in its own process with isolated XDG/OPENCODE_DB, explicit V2 protocol,
/// and AGENTVIEW_REAL_OPENCODE_BIN pointing at a source launcher or candidate.
#[test]
#[ignore = "requires an isolated V2 CLI; see docs/opencode-v2-parity.md"]
fn real_v2_lifecycle_without_provider_credentials() {
    assert_eq!(std::env::var("AGENTVIEW_OPENCODE_API").unwrap(), "v2");
    let executable = std::env::var("AGENTVIEW_REAL_OPENCODE_BIN").unwrap();
    let directory = tempfile::tempdir().unwrap();
    for (name, child) in [
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
        ("OPENCODE_CONFIG_DIR", "config/opencode"),
    ] {
        std::env::set_var(name, directory.path().join(child));
    }
    // A fresh store also gives the persistent FTS cache a fresh identity.
    std::env::set_var("OPENCODE_DB", directory.path().join("opencode.db"));
    let supervisor = Arc::new(
        OpenCodeSupervisor::with_state_dir(executable.clone(), directory.path().join("supervisor"))
            .unwrap(),
    );
    let shutdown = Shutdown(supervisor.clone());
    let session = supervisor
        .launch("agentview isolated V2 lifecycle probe", directory.path())
        .unwrap();
    assert!(session.id.starts_with("ses_"));
    assert!(supervisor.is_v2().unwrap());
    let reconnect =
        OpenCodeSupervisor::with_state_dir(executable.clone(), directory.path().join("supervisor"))
            .unwrap();
    let sessions = reconnect.list().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, session.id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if reconnect
            .inspect(&session.id)
            .unwrap()
            .contains("agentview isolated V2 lifecycle probe")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "durably admitted V2 input did not enter the projected transcript"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let source = OpenCodeSource::managed(executable.clone(), supervisor.clone());
    let discovered = source
        .discover(&DiscoveryRequest {
            include_completed: true,
            include_external: true,
            ..DiscoveryRequest::default()
        })
        .unwrap();
    assert!(discovered
        .iter()
        .any(|s| s.provider_session_id == session.id));
    let db = std::path::PathBuf::from(std::env::var_os("OPENCODE_DB").unwrap());
    let index = agentview::history_index::HistoryIndex::for_source(db).unwrap();
    index
        .refresh(Arc::new(std::sync::atomic::AtomicBool::new(false)))
        .unwrap();
    assert!(index
        .search("isolated V2 lifecycle probe")
        .unwrap()
        .contains_key(&format!("opencode:host:{}", session.id)));
    let command = reconnect.native_attach_command(&session.id).unwrap();
    let args = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(args[0], "--server");
    assert!(args.contains(&session.id));
    assert!(!args.iter().any(|a| a == "attach" || a == "--dir"));
    reconnect.available_models(directory.path()).unwrap();
    reconnect.interrupt(&session.id).unwrap();
    reconnect
        .reply(&session.id, "second isolated input")
        .unwrap();
    reconnect.interrupt(&session.id).unwrap();
    let old_pid = reconnect.live_server_pid().unwrap().unwrap();
    let restarted = reconnect.restart_server().unwrap();
    assert_eq!(restarted.previous_pid, Some(old_pid));
    assert_ne!(reconnect.live_server_pid().unwrap(), Some(old_pid));
    assert!(reconnect.is_v2().unwrap());
    assert_eq!(reconnect.list().unwrap()[0].id, session.id);
    drop(shutdown);
    assert!(supervisor.live_server_pid().unwrap().is_none());
    service_registration_roundtrip(&executable, directory.path());
}

fn service_registration_roundtrip(executable: &str, root: &std::path::Path) {
    use std::os::unix::process::CommandExt;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let child = std::process::Command::new(executable)
        .args([
            "serve",
            "--service",
            "--hostname",
            "127.0.0.1",
            "--port",
            &port.to_string(),
        ])
        .process_group(0)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                unsafe {
                    libc::kill(-(self.0.id() as i32), libc::SIGKILL);
                }
            }
            let _ = self.0.wait();
        }
    }
    let mut child = Child(child);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let registration = loop {
        let found = std::fs::read_dir(root.join("state/opencode"))
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|p| {
                p.file_name().is_some_and(|n| {
                    n.to_string_lossy().starts_with("service")
                        && n.to_string_lossy().ends_with(".json")
                })
            });
        if let Some(path) = found {
            break path;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "V2 CLI did not advertise service registration"
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "V2 service exited before registration"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    std::env::set_var("AGENTVIEW_OPENCODE_SERVICE_FILE", &registration);
    let supervisor = Arc::new(
        OpenCodeSupervisor::with_state_dir(executable, root.join("adopted-service")).unwrap(),
    );
    let shutdown = Shutdown(supervisor.clone());
    let session = supervisor
        .launch("registered V2 service lifecycle probe", root)
        .unwrap();
    assert!(supervisor.is_v2().unwrap());
    let previous = supervisor.live_server_pid().unwrap().unwrap();
    supervisor.interrupt(&session.id).unwrap();
    supervisor.restart_server().unwrap();
    let current = supervisor.live_server_pid().unwrap().unwrap();
    assert_ne!(current, previous);
    let info: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registration).unwrap()).unwrap();
    assert_eq!(info["pid"].as_u64(), Some(current as u64));
    let reconnect =
        OpenCodeSupervisor::with_state_dir(executable, root.join("adopted-service")).unwrap();
    assert_eq!(reconnect.list().unwrap()[0].id, session.id);
    reconnect.stop_server_for_resume().unwrap();
    let stopped =
        OpenCodeSupervisor::with_state_dir(executable, root.join("adopted-service")).unwrap();
    assert!(stopped.live_server_pid().unwrap().is_none());
    stopped
        .launch("restart a stopped registered V2 service", root)
        .unwrap();
    assert_eq!(stopped.list().unwrap().len(), 2);
    drop(shutdown);
    assert!(supervisor.live_server_pid().unwrap().is_none());
    std::env::remove_var("AGENTVIEW_OPENCODE_SERVICE_FILE");
}

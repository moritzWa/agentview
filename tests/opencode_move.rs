#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agentview::domain::{AgentSession, Provider, Runtime, SessionKind, SessionState};
use agentview::migration::{MigrationClient, MigrationRequest};
use agentview::process::{CommandRequest, CommandRunner, ProcessRunner};
use serde_json::{json, Value};

fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

fn run(cli: &Path, cwd: &Path, args: Vec<String>, stdout: Option<PathBuf>) -> Value {
    let mut request = CommandRequest::new(cli.to_str().unwrap(), args);
    request.current_dir = Some(cwd.to_owned());
    request.stdout_file = stdout;
    request.timeout = Duration::from_secs(60);
    let output = ProcessRunner.run(&request).unwrap();
    assert_eq!(output.status, 0, "{}", output.stderr_lossy());
    if request.args[0] == "export" {
        serde_json::from_slice(&output.stdout).expect("complete native export JSON")
    } else {
        Value::Null
    }
}

#[test]
#[ignore = "set AGENTVIEW_REAL_OPENCODE_BIN; isolated native move, no credentials or model calls"]
fn real_opencode_moves_large_history_without_session_migrate() {
    let binary = PathBuf::from(std::env::var_os("AGENTVIEW_REAL_OPENCODE_BIN").unwrap())
        .canonicalize()
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("source");
    let target = root.join("target with spaces");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&target).unwrap();
    run(
        Path::new("git"),
        &target,
        vec![
            "-c".into(),
            "init.templateDir=".into(),
            "init".into(),
            "--quiet".into(),
        ],
        None,
    );
    run(
        Path::new("git"),
        &target,
        vec![
            "-c".into(),
            "core.hooksPath=/dev/null".into(),
            "-c".into(),
            "commit.gpgsign=false".into(),
            "-c".into(),
            "user.name=Move Regression".into(),
            "-c".into(),
            "user.email=move-regression@example.invalid".into(),
            "commit".into(),
            "--allow-empty".into(),
            "--quiet".into(),
            "-m".into(),
            "Isolated move fixture".into(),
        ],
        None,
    );

    // Keep the real CLI entirely away from the user's stores and plugins.
    let cli = root.join("isolated-opencode");
    let mut wrapper = String::from(
        "#!/bin/sh\nunset OPENCODE_CONFIG OPENCODE_CONFIG_DIR OPENCODE_CONFIG_CONTENT\n",
    );
    for (key, dir) in [
        ("HOME", "home"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
    ] {
        fs::create_dir(root.join(dir)).unwrap();
        wrapper.push_str(&format!("export {key}={}\n", quoted(&root.join(dir))));
    }
    wrapper.push_str(&format!(
        "export OPENCODE_DISABLE_AUTOUPDATE=true\nexec {} \"$@\"\n",
        quoted(&binary)
    ));
    fs::write(&cli, wrapper).unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();

    let id = "ses_agentview_move_regression";
    let text = "questionnaire evidence: Grüße 日本語\n".repeat(20_000);
    assert!(text.len() > 512 * 1024);
    let bundle = json!({
        "info": {"id": id, "slug": "move-regression", "projectID": "global",
            "directory": source, "title": "move regression", "version": "test",
            "time": {"created": 1700000000000_u64, "updated": 1700000000000_u64}},
        "messages": [{"info": {"id": "msg_move_regression", "sessionID": id,
            "role": "user", "time": {"created": 1700000000000_u64}, "agent": "build",
            "model": {"providerID": "openai", "modelID": "custom-dev-model"}},
            "parts": [{"id": "prt_move_regression", "sessionID": id,
                "messageID": "msg_move_regression", "type": "text", "text": text}]}]
    });
    let seed = root.join("seed.json");
    fs::write(&seed, serde_json::to_vec(&bundle).unwrap()).unwrap();
    run(
        &cli,
        &source,
        vec!["import".into(), seed.display().to_string()],
        None,
    );
    let before = run(
        &cli,
        &source,
        vec!["export".into(), id.into()],
        Some(root.join("before.json")),
    );
    assert_eq!(
        before["messages"][0]["parts"][0]["text"],
        bundle["messages"][0]["parts"][0]["text"]
    );

    let client = MigrationClient::host(
        root.join("nonexistent-session-migrate")
            .display()
            .to_string(),
    )
    .with_source_cli(Provider::OpenCode, cli.display().to_string());
    // A completed agent's worktree is often removed before its session moves.
    // The first move must work even though its recorded cwd no longer exists.
    fs::remove_dir(&source).unwrap();
    let mut session = AgentSession {
        id: format!("opencode:host:{id}"),
        provider_session_id: id.into(),
        provider: Provider::OpenCode,
        runtime: Runtime::Host,
        kind: SessionKind::Managed,
        name: "move regression".into(),
        cwd: source.clone(),
        state: SessionState::Completed,
        summary: String::new(),
        wake: None,
        raw_state: None,
        pid: None,
        started_at: None,
        updated_at: None,
        pull_requests: None,
        capabilities: BTreeSet::new(),
    };
    // Moving back and moving again catches stale-directory and duplicate-ID bugs.
    for (index, destination) in [&target, &source, &target].into_iter().enumerate() {
        let outcome = client
            .migrate(&MigrationRequest {
                source: session.clone(),
                target: Provider::OpenCode,
                name: session.name.clone(),
                cwd: destination.clone(),
            })
            .unwrap();
        assert_eq!(outcome.session_id, id);
        assert_eq!(outcome.normalized_id, session.id);
        let after = run(
            &cli,
            destination,
            vec!["export".into(), id.into()],
            Some(root.join(format!("after-{index}.json"))),
        );
        assert_eq!(after["info"]["directory"].as_str(), destination.to_str());
        if destination == &target {
            assert_ne!(
                after["info"]["projectID"], before["info"]["projectID"],
                "moving into a Git repo must reassign the native project"
            );
        } else {
            assert_eq!(after["info"]["projectID"], before["info"]["projectID"]);
        }
        assert_eq!(after["info"]["title"], before["info"]["title"]);
        assert_eq!(
            after["messages"], before["messages"],
            "history changed during move {index}"
        );
        session.cwd = destination.clone();
        if index == 0 {
            fs::create_dir(&source).unwrap();
        }
    }
    let db = rusqlite::Connection::open_with_flags(
        root.join("data/opencode/opencode.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM session", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1, "move must not clone or delete the session");
}

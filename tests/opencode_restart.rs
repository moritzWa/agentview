#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use agentview::domain::SessionState;
use agentview::opencode_supervisor::OpenCodeSupervisor;
use serde_json::{json, Value};
use tempfile::tempdir;

#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "the Python fake server misses the 10s readiness window on hosted macOS runners"
)]
fn restart_keeps_the_endpoint_and_resumes_only_interrupted_top_level_turns() {
    let directory = tempdir().unwrap();
    let fake = directory.path().join("fake-opencode");
    write_fake_opencode(&fake);
    let state_file = directory.path().join("server-state.json");
    let supervisor = OpenCodeSupervisor::with_state_dir(
        fake.display().to_string(),
        directory.path().join("state"),
    )
    .unwrap();

    let launched = supervisor.launch("owned task", directory.path()).unwrap();
    let first_pid = launched.server_pid;

    let mut state: Value = serde_json::from_slice(&fs::read(&state_file).unwrap()).unwrap();
    let cwd = directory.path().display().to_string();
    for (id, parent) in [
        ("ses_run", None),
        ("ses_child", Some("ses_run")),
        ("ses_wait", None),
        ("ses_idle", None),
    ] {
        state["sessions"][id] = json!({"id": id, "directory": cwd, "parentID": parent, "title": id, "time": {"created": 1, "updated": 2}});
        state["statuses"][id] = json!(if id == "ses_idle" { "idle" } else { "busy" });
        state["messages"][id] = json!([]);
    }
    state["messages"]["ses_run"] = json!([
        {"info": {"role": "user", "agent": "build", "model": {"providerID": "old", "modelID": "old"}, "time": {"created": 10}}, "parts": []},
        {"info": {"role": "user", "agent": "plan", "model": {"providerID": "x", "modelID": "y", "variant": "high"}, "time": {"created": 20}}, "parts": []},
        {"info": {"role": "assistant", "time": {"created": 21}}, "parts": []}
    ]);
    state["questions"] = json!([{"id": "que_1", "sessionID": "ses_wait"}]);
    fs::write(&state_file, serde_json::to_vec(&state).unwrap()).unwrap();

    let report = supervisor.restart_server().unwrap();

    assert_eq!(report.previous_pid, Some(first_pid));
    assert_ne!(report.pid, first_pid, "the server was not replaced");
    assert!(
        report.same_port,
        "attached TUIs need the old port to reconnect"
    );
    let mut resumed = report.resumed.clone();
    resumed.sort();
    assert_eq!(resumed, ["ses_owned", "ses_run"]);
    assert_eq!(report.awaiting_input, ["ses_wait"]);
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    let state: Value = serde_json::from_slice(&fs::read(&state_file).unwrap()).unwrap();
    let prompts = state["prompts"].as_array().unwrap();
    let resumed_run = prompts
        .iter()
        .find(|prompt| prompt["session"] == "ses_run")
        .expect("ses_run was not resumed on the new server");
    assert_eq!(resumed_run["server"], json!(report.pid));
    assert_eq!(
        resumed_run["body"],
        json!({
            "parts": [{"type": "text", "text": "continue"}],
            "agent": "plan",
            "model": {"providerID": "x", "modelID": "y"},
            "variant": "high"
        })
    );
    assert!(
        prompts
            .iter()
            .all(|prompt| prompt["session"] != "ses_child" && prompt["session"] != "ses_wait"),
        "a subagent or prompt-blocked session was resumed: {prompts:?}"
    );
    assert!(!directory.path().join("state/restart-pending.json").exists());

    // The new server is a verified record, so normal control keeps working.
    let listed = supervisor.list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].server_pid, report.pid);
    supervisor.shutdown_server().unwrap();
}

#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "the Python fake server misses the 10s readiness window on hosted macOS runners"
)]
fn shared_client_is_used_only_when_the_server_can_switch_one_tui() {
    let directory = tempdir().unwrap();
    let fake = directory.path().join("fake-opencode");
    write_fake_opencode(&fake);
    let state_file = directory.path().join("server-state.json");
    let supervisor = |dir: &Path| {
        OpenCodeSupervisor::with_state_dir(fake.display().to_string(), dir.join("state")).unwrap()
    };
    let launched = supervisor(directory.path())
        .launch("owned task", directory.path())
        .unwrap();

    assert!(
        supervisor(directory.path())
            .shared_client_command("ses_owned", directory.path(), "pane-1")
            .unwrap()
            .is_none(),
        "a server that drops `client` would switch every TUI"
    );

    let mut state: Value = serde_json::from_slice(&fs::read(&state_file).unwrap()).unwrap();
    state["client_targeting"] = json!(true);
    fs::write(&state_file, serde_json::to_vec(&state).unwrap()).unwrap();
    let targeting = supervisor(directory.path());
    let (command, server_pid) = targeting
        .shared_client_command("ses_owned", directory.path(), "pane-1")
        .unwrap()
        .expect("a server that targets one client gets a shared TUI");
    assert_eq!(server_pid, launched.server_pid);
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        args.windows(2).any(|pair| pair == ["--client", "pane-1"]),
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--session", "ses_owned"]),
        "{args:?}"
    );

    assert_eq!(
        targeting
            .select_in_shared_client("ses_owned", directory.path(), "pane-1")
            .unwrap(),
        launched.server_pid
    );
    let state: Value = serde_json::from_slice(&fs::read(&state_file).unwrap()).unwrap();
    assert_eq!(
        state["selects"][0]["body"],
        json!({"sessionID": "ses_owned", "client": "pane-1"})
    );
    assert_eq!(
        targeting.live_server_pid().unwrap(),
        Some(launched.server_pid)
    );
    targeting.shutdown_server().unwrap();
}

#[test]
#[ignore = "set AGENTVIEW_REAL_OPENCODE_BIN; runs one short model turn, AGENTVIEW_REAL_OPENCODE_MODEL is optional"]
fn real_opencode_restart_resumes_a_turn_cut_off_mid_tool_call() {
    let executable = std::env::var("AGENTVIEW_REAL_OPENCODE_BIN").unwrap();
    let model = std::env::var("AGENTVIEW_REAL_OPENCODE_MODEL").ok();
    let directory = tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let supervisor =
        OpenCodeSupervisor::with_state_dir(executable, directory.path().join("state")).unwrap();
    let launched = supervisor
        .launch_with_model(
            "Run the shell command `sleep 40`, then reply with only the word finished.",
            &workspace,
            model.as_deref(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(90);
    while !supervisor
        .inspect(&launched.id)
        .unwrap()
        .contains("sleep 40")
    {
        assert!(
            Instant::now() < deadline,
            "the turn never reached its tool call"
        );
        thread::sleep(Duration::from_millis(500));
    }

    let report = supervisor.restart_server().unwrap();
    assert!(report.same_port);
    assert_eq!(report.resumed, [launched.id.clone()], "{report:?}");

    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let transcript = supervisor.inspect(&launched.id).unwrap();
        let session = supervisor.list().unwrap().remove(0);
        if transcript.contains("User: continue") && session.state != SessionState::Working {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the resumed turn did not finish:\n{transcript}"
        );
        thread::sleep(Duration::from_secs(1));
    }
    supervisor.shutdown_server().unwrap();
}

fn write_fake_opencode(path: &Path) {
    fs::write(
        path,
        r##"#!/usr/bin/env python3
import base64
import json
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

if len(sys.argv) < 2 or sys.argv[1] != "serve":
    if len(sys.argv) > 1 and sys.argv[1] == "db":
        print("directory")
    raise SystemExit(0)

state_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "server-state.json")
lock = threading.Lock()

def load():
    try:
        with open(state_path) as handle:
            return json.load(handle)
    except FileNotFoundError:
        return {"sessions": {}, "statuses": {}, "messages": {}, "questions": [], "prompts": []}

def save(state):
    with open(state_path, "w") as handle:
        json.dump(state, handle)

# Turns do not survive the process, like a real server restart.
with lock:
    state = load()
    state["statuses"] = {key: "idle" for key in state["statuses"]}
    state["questions"] = []
    save(state)

port = int(sys.argv[sys.argv.index("--port") + 1])
username = os.environ.get("OPENCODE_SERVER_USERNAME", "opencode")
password = os.environ["OPENCODE_SERVER_PASSWORD"]
authorization = "Basic " + base64.b64encode(f"{username}:{password}".encode()).decode()

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def authorized(self):
        if self.headers.get("Authorization") != authorization:
            self.send_response(401)
            self.end_headers()
            return False
        return True

    def body(self):
        length = int(self.headers.get("Content-Length", "0"))
        return json.loads(self.rfile.read(length) or b"{}")

    def respond(self, status, value=None):
        body = b"" if value is None else json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if not self.authorized(): return
        parsed = urlparse(self.path)
        with lock:
            state = load()
        if parsed.path == "/global/health":
            return self.respond(200, {"healthy": True, "version": "fake"})
        if parsed.path == "/session/status":
            return self.respond(200, {key: {"type": value} for key, value in state["statuses"].items() if value != "idle"})
        if parsed.path == "/question":
            return self.respond(200, state["questions"])
        if parsed.path == "/permission":
            return self.respond(200, [])
        if parsed.path == "/doc":
            properties = {"sessionID": {"type": "string"}}
            if state.get("client_targeting"):
                properties["client"] = {"type": "string"}
            schema = {"type": "object", "properties": properties}
            return self.respond(200, {"paths": {"/tui/select-session": {"post": {"requestBody": {"content": {"application/json": {"schema": schema}}}}}}})
        if parsed.path == "/session":
            return self.respond(200, list(state["sessions"].values()))
        if parsed.path.endswith("/message"):
            session_id = parsed.path.split("/")[2]
            if session_id not in state["sessions"]: return self.respond(404, {"error": "missing"})
            return self.respond(200, state["messages"][session_id])
        if parsed.path.startswith("/session/"):
            session_id = parsed.path.split("/")[2]
            if session_id not in state["sessions"]: return self.respond(404, {"error": "missing"})
            return self.respond(200, state["sessions"][session_id])
        return self.respond(404, {"error": "unknown"})

    def do_POST(self):
        if not self.authorized(): return
        parsed = urlparse(self.path)
        data = self.body()
        with lock:
            state = load()
            if parsed.path == "/session":
                directory = parse_qs(parsed.query).get("directory", [os.getcwd()])[0]
                session_id = "ses_owned"
                state["sessions"][session_id] = {"id": session_id, "title": "task", "directory": directory, "time": {"created": 1, "updated": 2}}
                state["statuses"][session_id] = "idle"
                state["messages"][session_id] = []
                save(state)
                return self.respond(200, state["sessions"][session_id])
            if parsed.path.endswith("/prompt_async"):
                session_id = parsed.path.split("/")[2]
                if session_id not in state["sessions"]: return self.respond(404, {"error": "missing"})
                state["prompts"].append({"session": session_id, "server": os.getpid(), "body": data})
                state["statuses"][session_id] = "busy"
                save(state)
                return self.respond(204)
            if parsed.path == "/tui/select-session":
                state.setdefault("selects", []).append({"query": parsed.query, "body": data})
                save(state)
                return self.respond(200, True)
        return self.respond(404, {"error": "unknown"})

ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
"##,
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

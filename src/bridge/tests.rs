use super::*;
use rusqlite::Connection;
use source::{Capture, Message};
use tempfile::tempdir;

#[test]
fn truncated_emoji_escape_does_not_block_projection() {
    let raw = r#"{"text":"a \ud83d\n b \ude00 c \ud83d\ude00 \\ud83d"}"#;
    assert!(serde_json::from_str::<Value>(raw).is_err());
    let value = source::parse_json(raw).unwrap();
    assert_eq!(value["text"], "a \u{fffd}\n b \u{fffd} c \u{1F600} \\ud83d");
}

#[test]
fn queued_context_allows_new_turns_but_blocks_rewritten_history() {
    let mut db = fixture();
    let mut capture = source::capture(&mut db, "s", None).unwrap().unwrap();
    let guard = json!({"version":1,"anchor_order":1,"anchor_bytes":7,"prefix_hash":"c7831bf2b8c6c012e0c622a9ae47915d64e9b2a3ef0fa579eb2680a5a74104b3","visible_before":null});
    assert!(context::matches(&capture, &guard));
    capture.messages[0].text.push_str(" streaming suffix");
    assert!(context::matches(&capture, &guard));
    capture.messages.push(Message {
        id: "new_reply".into(),
        role: "assistant".into(),
        order: 2,
        created_at: 2,
        updated_at: 2,
        text: "Progress after the phone read".into(),
    });
    capture.messages.push(Message {
        id: "new_user".into(),
        role: "user".into(),
        order: 3,
        created_at: 3,
        updated_at: 3,
        text: "Another appended turn".into(),
    });
    assert!(context::matches(&capture, &guard));
    capture.messages[0].text = "Edited old input".into();
    assert!(!context::matches(&capture, &guard));
    capture.messages[0].text = "Visible".into();
    capture.state["visible_before"] = json!(2);
    assert!(!context::matches(&capture, &guard));
}

#[test]
fn runtime_projection_includes_requested_directory_but_never_chat_or_tool_payloads() {
    use crate::domain::{AgentSession, Provider, Runtime, SessionKind, SessionState};
    let mut session = AgentSession {
        id: "opencode:host:ses_x".into(),
        provider_session_id: "ses_x".into(),
        provider: Provider::OpenCode,
        runtime: Runtime::Host,
        kind: SessionKind::Managed,
        name: "PRIVATE_TITLE".into(),
        cwd: PathBuf::from("PRIVATE_PATH"),
        state: SessionState::NeedsInput,
        summary: "PRIVATE_TOOL_OUTPUT".into(),
        raw_state: Some("permission requested".into()),
        wake: None,
        pid: None,
        started_at: None,
        updated_at: None,
        pull_requests: None,
        capabilities: Default::default(),
    };
    let status = serde_json::to_value(runtime::project(&session)).unwrap();
    assert_eq!(status["status"], "needs_you");
    assert_eq!(status["reason"], "approval");
    assert_eq!(status["can_message"], false);
    assert_eq!(status["directory"], "PRIVATE_PATH");
    assert!(!status.to_string().contains("PRIVATE_TOOL_OUTPUT"));
    assert!(!status.to_string().contains("PRIVATE_TITLE"));
    session.state = SessionState::Working;
    session.raw_state = None;
    assert_eq!(
        serde_json::to_value(runtime::project(&session)).unwrap()["status"],
        "working"
    );
}

#[test]
#[ignore = "read-only inspection of the paired Mac's native session classifier; no uploads or model calls"]
fn real_runtime_snapshot_audit_without_upload() {
    let config = load(&state_dir().unwrap()).unwrap();
    let start = std::time::Instant::now();
    let snapshot = serde_json::to_value(runtime::snapshot(&config).unwrap()).unwrap();
    let mut counts = std::collections::BTreeMap::new();
    for status in snapshot["statuses"].as_array().unwrap() {
        *counts
            .entry(status["status"].as_str().unwrap())
            .or_insert(0usize) += 1;
    }
    eprintln!("Native runtime audit: available={}, partial={}, truncated={}, states={counts:?}, elapsed_ms={}",snapshot["available"],snapshot["partial"],snapshot["truncated"],start.elapsed().as_millis());
    assert_eq!(snapshot["available"], true);
    assert_eq!(snapshot["partial"], false);
}

#[test]
fn login_daemon_retains_shared_v2_discovery_but_not_secrets_or_isolated_profiles() {
    let dir = tempdir().unwrap();
    let service = dir.path().join("runtime/service.json");
    let state = dir.path().join("state");
    let shared = login_environment(false, Some(&service), Some(&state));
    assert_eq!(
        shared.get("AGENTVIEW_OPENCODE_API").map(String::as_str),
        Some("v2")
    );
    assert_eq!(
        shared
            .get("AGENTVIEW_OPENCODE_SERVICE_FILE")
            .map(String::as_str),
        service.to_str()
    );
    assert_eq!(
        shared.get("XDG_STATE_HOME").map(String::as_str),
        state.to_str()
    );
    assert_eq!(
        shared.len(),
        3,
        "only nonsecret discovery settings are persisted"
    );
    let isolated = login_environment(true, Some(&service), Some(&state));
    assert_eq!(isolated.len(), 1);
    assert!(!isolated.contains_key("AGENTVIEW_OPENCODE_SERVICE_FILE"));
    assert_eq!(
        login_environment(false, Some(Path::new("relative/service.json")), None).len(),
        1
    );
}

fn fixture() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE session_v2(id TEXT PRIMARY KEY,title TEXT,directory TEXT,time_archived INTEGER,revert TEXT);
      CREATE TABLE session_message(id TEXT PRIMARY KEY,session_id TEXT,type TEXT,seq INTEGER,time_created INTEGER,time_updated INTEGER,data TEXT);
      CREATE TABLE event_sequence(aggregate_id TEXT PRIMARY KEY,seq INTEGER);
      CREATE TABLE event(aggregate_id TEXT,seq INTEGER,type TEXT,data TEXT);
      INSERT INTO session_v2 VALUES('s','Fixture','/fixture',NULL,NULL);
      INSERT INTO event_sequence VALUES('s',1);
      INSERT INTO event VALUES('s',0,'session.created.1','{}'),('s',1,'session.inbox.delivered.1','{\"inboxID\":\"msg_a\"}');
      INSERT INTO session_message VALUES('msg_a','s','user',1,1,1,'{\"text\":\"Visible\",\"files\":[\"PRIVATE_ATTACHMENT\"]}');").unwrap();
    db
}
fn ack(body: &Value) -> Value {
    let complete = body
        .get("complete")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    json!({"batch_id":body["batch_id"],"session_id":body["session_id"],"committed":complete,"through":if complete{body["through"].clone()}else{Value::Null},"next_chunk":if complete{Value::Null}else{json!(body["chunk"].as_i64().unwrap()+1)}})
}
fn drain(journal: &mut Journal) {
    while let Some(body) = journal.pending().unwrap() {
        journal.ack(&ack(&body)).unwrap();
    }
}

#[test]
fn text_projection_never_serializes_tools_reasoning_or_metadata() {
    let raw = json!({"content":[{"type":"text","text":"Hello","native":"SECRET"},{"type":"tool","state":{"output":"SECRET"}},{"type":"reasoning","text":"SECRET"}],"metadata":"SECRET"});
    let projected = source::project("msg_x", "assistant", 2, 1, 2, &raw.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(projected.text, "Hello");
    assert!(!serde_json::to_string(&projected)
        .unwrap()
        .contains("SECRET"));
    assert!(source::project("x", "system", 1, 1, 1, "{}")
        .unwrap()
        .is_none());
    assert!(source::project(
        "x",
        "assistant",
        1,
        1,
        1,
        r#"{"content":[{"type":"future","text":"secret"}]}"#
    )
    .is_err());
}

#[test]
fn a_newest_tool_only_assistant_message_does_not_displace_real_text_replies() {
    let mut db = fixture();
    db.execute_batch("INSERT INTO session_message VALUES
      ('real_a','s','assistant',2,2,2,'{\"content\":[{\"type\":\"text\",\"text\":\"First actual reply\"}]}'),
      ('user_b','s','user',3,3,3,'{\"text\":\"Second request\"}'),
      ('real_b','s','assistant',4,4,4,'{\"content\":[{\"type\":\"text\",\"text\":\"Second actual reply\"},{\"type\":\"tool\",\"state\":{\"output\":\"PRIVATE_TOOL_RESULT\"}}]}'),
      ('tool_only','s','assistant',5,5,5,'{\"content\":[{\"type\":\"tool\",\"state\":{\"output\":\"PRIVATE_TOOL_RESULT\"}},{\"type\":\"reasoning\",\"text\":\"PRIVATE_REASONING\"}]}');
      UPDATE event_sequence SET seq=5;").unwrap();
    let capture = source::capture(&mut db, "s", None).unwrap().unwrap();
    let replies = capture
        .messages
        .iter()
        .filter(|m| m.role == "assistant")
        .collect::<Vec<_>>();
    assert_eq!(
        replies.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
        vec!["First actual reply", "Second actual reply"]
    );
    assert!(!capture.ids.contains("tool_only"));
    assert!(!serde_json::to_string(&capture.messages)
        .unwrap()
        .contains("PRIVATE"));
}

#[test]
fn source_captures_consistent_watermark_and_revert_boundary_then_detects_edits() {
    let mut db = fixture();
    let first = source::capture(&mut db, "s", None).unwrap().unwrap();
    assert_eq!(first.through, 1);
    assert_eq!(first.messages[0].text, "Visible");
    db.execute_batch("UPDATE session_message SET data='{\"text\":\"Edited\"}',time_updated=2 WHERE id='msg_a';
      INSERT INTO event VALUES('s',2,'session.message.content.updated.1','{\"messageID\":\"msg_a\"}');UPDATE event_sequence SET seq=2;
      UPDATE session_v2 SET revert='{\"messageID\":\"msg_a\"}';").unwrap();
    let edit = source::capture(&mut db, "s", Some(1)).unwrap().unwrap();
    assert_eq!(edit.messages[0].text, "Edited");
    assert_eq!(edit.state["visible_before"], 1);
    db.execute("UPDATE event_sequence SET seq=4", []).unwrap();
    let reconciled = source::capture(&mut db, "s", Some(2)).unwrap().unwrap();
    assert_eq!(reconciled.through, 4);
    assert_eq!(reconciled.messages[0].text, "Edited");
    db.execute("UPDATE event_sequence SET seq=1", []).unwrap();
    assert!(source::capture(&mut db, "s", Some(2)).is_err());
}

#[test]
fn journal_retries_exact_batch_and_never_advances_cloud_cursor_without_ack() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("journal.sqlite");
    let mut db = fixture();
    let mut journal = Journal::open(&path).unwrap();
    journal
        .stage(
            "s",
            source::capture(&mut db, "s", None).unwrap().unwrap(),
            true,
        )
        .unwrap();
    let first = journal.pending().unwrap().unwrap();
    assert!(journal.checkpoint("s").unwrap().is_none());
    drop(journal);
    let mut journal = Journal::open(&path).unwrap();
    assert_eq!(journal.pending().unwrap().unwrap(), first);
    let mut bad = ack(&first);
    bad["through"] = json!(999);
    assert!(journal.ack(&bad).is_err());
    assert!(journal.pending().unwrap().is_some());
    drain(&mut journal);
    assert_eq!(journal.checkpoint("s").unwrap().unwrap().cursor, 1);
    assert_eq!(journal.cached("s").unwrap().len(), 1);
}

#[test]
fn invisible_events_do_not_upload_or_advance_cloud_ack_and_large_changes_chunk() {
    let dir = tempdir().unwrap();
    let mut journal = Journal::open(&dir.path().join("j.sqlite")).unwrap();
    let mut db = fixture();
    journal
        .stage(
            "s",
            source::capture(&mut db, "s", None).unwrap().unwrap(),
            true,
        )
        .unwrap();
    drain(&mut journal);
    let mut same = source::capture(&mut db, "s", None).unwrap().unwrap();
    same.through = 5;
    same.messages[0].updated_at = 999;
    assert!(!journal.stage("s", same, true).unwrap());
    assert!(journal.pending().unwrap().is_none());
    let checkpoint = journal.checkpoint("s").unwrap().unwrap();
    assert_eq!(checkpoint.cursor, 1);
    assert_eq!(checkpoint.observed, 5);
    let messages = (0..900)
        .map(|i| Message {
            id: format!("msg_{i}"),
            role: "assistant".into(),
            order: i,
            created_at: 1,
            updated_at: 1,
            text: "text".into(),
        })
        .collect::<Vec<_>>();
    let capture = Capture {
        through: 10,
        state: json!({"title":"Fixture","archived":false,"deleted":false,"visible_before":null}),
        ids: messages.iter().map(|m| m.id.clone()).collect(),
        messages,
        cwd: "/fixture".into(),
    };
    journal.stage("s", capture, true).unwrap();
    let body = journal.pending().unwrap().unwrap();
    assert_eq!(body["from"], 1);
    assert_eq!(body["complete"], false);
    journal.ack(&ack(&body)).unwrap();
    assert_eq!(journal.checkpoint("s").unwrap().unwrap().cursor, 1);
    drain(&mut journal);
    assert_eq!(journal.checkpoint("s").unwrap().unwrap().cursor, 10);
    assert_eq!(journal.cached("s").unwrap().len(), 900);
}

#[test]
fn journal_refuses_an_opencode_database_without_modifying_it() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("source.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute("CREATE TABLE session_v2(id TEXT)", []).unwrap();
    drop(db);
    assert!(Journal::open(&path).is_err());
    let db = Connection::open(&path).unwrap();
    let n: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn credential_wrapper_cannot_read_or_write_subscription_items() {
    assert!(
        crate::usage::save_bridge_credential("Claude Code-credentials", &"x".repeat(43)).is_none()
    );
    assert!(crate::usage::read_bridge_credential("Claude Code-credentials").is_none());
    assert!(crate::usage::save_bridge_credential(
        "agentview-bridge-00000000000000000000000000000000",
        "bad\nsecret"
    )
    .is_none());
}

#[test]
fn split_parts_allow_only_text_and_resolve_revert_against_nontext_rows() {
    let mut db = fixture();
    db.execute_batch("CREATE TABLE session_part(message_id TEXT,ordinal INTEGER,type TEXT,data TEXT);
      INSERT INTO session_message VALUES('msg_split','s','assistant',2,2,2,'{}');
      INSERT INTO session_part VALUES('msg_split',0,'reasoning','{\"text\":\"PRIVATE_REASONING\"}'),('msg_split',1,'tool','{\"text\":\"PRIVATE_TOOL\"}'),('msg_split',2,'text','{\"text\":\"Split visible reply\"}');
      INSERT INTO session_message VALUES('msg_boundary','s','assistant',3,3,3,'{\"content\":[{\"type\":\"tool\",\"text\":\"PRIVATE\"}]}');
      UPDATE session_v2 SET revert='{\"messageID\":\"msg_boundary\"}';").unwrap();
    let capture = source::capture(&mut db, "s", None).unwrap().unwrap();
    assert_eq!(capture.messages.len(), 2);
    assert_eq!(capture.messages[1].text, "Split visible reply");
    assert_eq!(capture.state["visible_before"], 3);
    assert!(!serde_json::to_string(&capture.messages)
        .unwrap()
        .contains("PRIVATE"));
}

#[test]
fn legacy_sessions_are_included_once_with_text_only_and_edited_parts() {
    let mut db = fixture();
    db.execute_batch("CREATE TABLE session(id TEXT,title TEXT,directory TEXT,time_archived INTEGER,revert TEXT,time_updated INTEGER);
      CREATE TABLE message(id TEXT,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);
      CREATE TABLE part(id TEXT,message_id TEXT,session_id TEXT,time_updated INTEGER,data TEXT);
      INSERT INTO session VALUES('legacy','Legacy','/fixture',NULL,NULL,10),('s','Duplicate','/fixture',NULL,NULL,10);
      INSERT INTO message VALUES('msg_old','legacy',1,1,'{\"role\":\"user\"}'),('msg_reply','legacy',2,2,'{\"role\":\"assistant\"}');
      INSERT INTO part VALUES('part1','msg_old','legacy',10,'{\"type\":\"text\",\"text\":\"Old user text\"}'),('part2','msg_reply','legacy',10,'{\"type\":\"reasoning\",\"text\":\"PRIVATE\"}'),('part3','msg_reply','legacy',10,'{\"type\":\"text\",\"text\":\"Legacy reply\"}'),('part4','msg_reply','legacy',10,'{\"type\":\"text\",\"text\":\"PRIVATE\",\"synthetic\":true}');").unwrap();
    assert_eq!(source::sessions(&db).unwrap(), vec!["s", "legacy"]);
    let capture = source::capture(&mut db, "legacy", None).unwrap().unwrap();
    assert_eq!(capture.through, 10);
    assert_eq!(capture.messages.len(), 2);
    assert_eq!(capture.messages[1].text, "Legacy reply");
    db.execute("UPDATE part SET data='{\"type\":\"text\",\"text\":\"Edited legacy reply\"}',time_updated=11 WHERE id='part3'",[]).unwrap();
    assert_eq!(
        source::capture(&mut db, "legacy", Some(10))
            .unwrap()
            .unwrap()
            .messages[1]
            .text,
        "Edited legacy reply"
    );
    db.execute(
        "UPDATE part SET data='{\"type\":\"text\",\"text\":\"\"}',time_updated=12 WHERE id='part3'",
        [],
    )
    .unwrap();
    let emptied = source::capture(&mut db, "legacy", Some(11))
        .unwrap()
        .unwrap();
    assert!(!emptied.ids.contains("msg_reply"));
    db.execute(
        "UPDATE session SET revert='{\"messageID\":\"msg_reply\"}' WHERE id='legacy'",
        [],
    )
    .unwrap();
    assert_eq!(
        source::capture(&mut db, "legacy", None)
            .unwrap()
            .unwrap()
            .state["visible_before"],
        1
    );
}

#[test]
#[ignore = "set AGENTVIEW_BRIDGE_SOURCE for a read-only full source projection audit; never uploads"]
fn real_source_projection_audit_without_upload() {
    let path = std::env::var("AGENTVIEW_BRIDGE_SOURCE").expect("set AGENTVIEW_BRIDGE_SOURCE");
    let mut db = source::open(Path::new(&path)).unwrap();
    let ids = source::sessions(&db).unwrap();
    let mut messages = 0usize;
    let mut bytes = 0usize;
    let mut largest = 0usize;
    for id in &ids {
        let capture = source::capture(&mut db, id, None)
            .unwrap_or_else(|error| panic!("source projection failed for {id}: {error}"));
        if let Some(capture) = capture {
            messages += capture.messages.len();
            bytes += capture.messages.iter().map(|m| m.text.len()).sum::<usize>();
            for message in capture.messages {
                largest = largest.max(serde_json::to_vec(&message).unwrap().len());
            }
        }
    }
    eprintln!("Read-only source audit: {} sessions, {messages} visible messages, {bytes} UTF-8 text bytes, largest encoded message {largest} bytes. No upload.",ids.len());
    assert!(
        largest <= 850_000,
        "a source message needs transport fragmentation before upload"
    );
}

#[test]
fn large_visible_paste_is_not_silently_dropped_or_truncated() {
    let text = "visible text ".repeat(57_000);
    let message = source::project(
        "long-message",
        "user",
        1,
        1,
        1,
        &json!({"text":text}).to_string(),
    )
    .unwrap()
    .unwrap();
    assert!(message.text.len() > 500_000);
    let dir = tempdir().unwrap();
    let mut journal = Journal::open(&dir.path().join("j.sqlite")).unwrap();
    journal.stage("s",Capture{through:1,state:json!({"title":"Large paste","archived":false,"deleted":false,"visible_before":null}),ids:[message.id.clone()].into_iter().collect(),messages:vec![message.clone()],cwd:"/fixture".into()},true).unwrap();
    assert_eq!(
        journal.pending().unwrap().unwrap()["messages"][0]["text"],
        message.text
    );
}

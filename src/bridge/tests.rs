use super::*;
use rusqlite::Connection;
use source::{Capture, Message};
use tempfile::tempdir;

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
}

//! Explicit ownership transfer after a frozen, quiescent V1 snapshot.
//! Admission IDs deduplicate continuation inputs, not external tool actions.

use super::*;

const LEDGER: &str = "handoffs.json";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    version: u32,
    #[serde(rename = "handoffID")]
    handoff_id: String,
    endpoint: Endpoint,
    sessions: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Endpoint {
    url: String,
    pid: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Entry {
    id: String,
    title: String,
    directory: PathBuf,
    agent: Option<String>,
    model: Option<Model>,
    /// Exact newest projected message after migration (including notices).
    #[serde(rename = "lastMessageID")]
    last_message_id: Option<String>,
    created_at_ms: u64,
    updated_at_ms: u64,
    source_state: SourceState,
    resume: bool,
    tools_settled: bool,
    external_actions_settled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Model {
    #[serde(rename = "providerID")]
    provider_id: String,
    id: String,
    #[serde(default)]
    variant: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum SourceState {
    Interrupted,
    Completed,
    AwaitingInput,
    Uncertain,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Ledger {
    #[serde(default)]
    handoffs: BTreeMap<String, Transfer>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Transfer {
    manifest: Manifest,
    roots: BTreeMap<String, Root>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Root {
    /// Allocated and fsynced before the first HTTP admission attempt.
    input_id: Option<String>,
    last_user_id: Option<String>,
    #[serde(default)]
    accepted: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffReport {
    pub handoff_id: String,
    pub adopted: Vec<String>,
    pub resumed: Vec<String>,
    pub already_accepted: Vec<String>,
}

impl OpenCodeSupervisor {
    /// Validate the entire manifest before granting any new ownership. This
    /// never starts, stops, or restarts a server, and never resumes a turn.
    pub fn import_handoff(&self, path: &Path) -> Result<HandoffReport> {
        let manifest: Manifest = read_private(path)?.context("handoff manifest is missing")?;
        validate_manifest(&manifest)?;
        let _lock = StateLock::acquire(&self.lock_path)?;
        let mut record = self.required_live_record_locked()?;
        let mut ledger: Ledger = read_private(&self.state_dir.join(LEDGER))?.unwrap_or_default();
        check_endpoint(
            &manifest,
            &record,
            !ledger.handoffs.contains_key(&manifest.handoff_id),
        )?;
        if let Some(previous) = ledger.handoffs.get(&manifest.handoff_id) {
            if previous.manifest != manifest {
                bail!("handoff ID already belongs to a different manifest");
            }
            // A recorded transfer is the durable authority proof. Re-import
            // after execution must not compare old watermarks with new replies.
            for entry in &manifest.sessions {
                if !record.sessions.contains_key(&entry.id) {
                    let messages = self.validate_handoff_entry(&record, entry)?;
                    record
                        .sessions
                        .insert(entry.id.clone(), owned(entry, &messages));
                }
            }
            save_record(&self.record_path, &record)?;
            return Ok(HandoffReport {
                handoff_id: manifest.handoff_id,
                adopted: manifest.sessions.iter().map(|e| e.id.clone()).collect(),
                ..Default::default()
            });
        }
        if ledger.handoffs.is_empty() && !record.sessions.is_empty() {
            bail!("first handoff requires a new, empty supervisor ownership map");
        }
        let mut roots = BTreeMap::new();
        let mut adopted = BTreeMap::new();
        for entry in &manifest.sessions {
            if record.sessions.contains_key(&entry.id)
                || ledger
                    .handoffs
                    .values()
                    .any(|h| h.roots.contains_key(&entry.id))
            {
                bail!("session {} already has an ownership authority", entry.id);
            }
            let messages = self.validate_handoff_entry(&record, entry)?;
            adopted.insert(entry.id.clone(), owned(entry, &messages));
            roots.insert(
                entry.id.clone(),
                Root {
                    input_id: if entry.resume {
                        Some(new_input_id()?)
                    } else {
                        None
                    },
                    last_user_id: latest_user(&messages),
                    accepted: false,
                },
            );
        }
        let report = HandoffReport {
            handoff_id: manifest.handoff_id.clone(),
            adopted: manifest.sessions.iter().map(|e| e.id.clone()).collect(),
            ..Default::default()
        };
        // A crash between these two files is recoverable by re-importing the
        // identical manifest. No prompt can precede either durable write.
        ledger.handoffs.insert(
            manifest.handoff_id.clone(),
            Transfer {
                manifest: manifest.clone(),
                roots,
            },
        );
        write_private(&self.state_dir.join(LEDGER), &ledger)?;
        record.sessions.extend(adopted);
        save_record(&self.record_path, &record)?;
        Ok(report)
    }

    /// Resume only roots explicitly selected in a previously imported private
    /// manifest. Retrying reconciles durable input IDs before any POST.
    pub fn resume_handoff(&self, handoff_id: &str) -> Result<HandoffReport> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let record = self.required_live_record_locked()?;
        let path = self.state_dir.join(LEDGER);
        let mut ledger: Ledger = read_private(&path)?.unwrap_or_default();
        let transfer = ledger
            .handoffs
            .get(handoff_id)
            .context("handoff has not been imported")?
            .clone();
        check_endpoint(&transfer.manifest, &record, false)?;
        let mut report = HandoffReport {
            handoff_id: handoff_id.into(),
            ..Default::default()
        };
        for entry in transfer.manifest.sessions.iter().filter(|e| e.resume) {
            require_owned(&record, &entry.id)?;
            let root = &transfer.roots[&entry.id];
            let input = root
                .input_id
                .as_deref()
                .context("selected handoff root has no durable input ID")?;
            if root.accepted || self.handoff_input_exists(&record, &entry.id, input)? {
                ledger
                    .handoffs
                    .get_mut(handoff_id)
                    .unwrap()
                    .roots
                    .get_mut(&entry.id)
                    .unwrap()
                    .accepted = true;
                write_private(&path, &ledger)?;
                report.already_accepted.push(entry.id.clone());
                continue;
            }
            // Validate immediately before admission as well: an operator may
            // have answered, completed, or changed this session since import.
            self.validate_handoff_entry(&record, entry)?;
            let body = json!({"id":input,"parts":[{"type":"text","text":"continue"}]});
            self.request_empty(
                &record,
                "POST",
                &format!("/session/{}/prompt_async", url_path_segment(&entry.id)),
                Some(&body),
            )?;
            // If the ACK is lost, this write is skipped; the next invocation
            // finds the same input in the durable inbox/projected transcript.
            ledger
                .handoffs
                .get_mut(handoff_id)
                .unwrap()
                .roots
                .get_mut(&entry.id)
                .unwrap()
                .accepted = true;
            write_private(&path, &ledger)?;
            report.resumed.push(entry.id.clone());
        }
        Ok(report)
    }

    fn validate_handoff_entry(&self, record: &ServerRecord, entry: &Entry) -> Result<Value> {
        let session = self.request_json(
            record,
            "GET",
            &format!("/session/{}", url_path_segment(&entry.id)),
            None,
        )?;
        if session["id"] != entry.id
            || session["title"].as_str().unwrap_or("") != entry.title
            || session
                .pointer("/location/directory")
                .and_then(Value::as_str)
                != entry.directory.to_str()
            || session["agent"].as_str() != entry.agent.as_deref()
            || session.pointer("/time/created").and_then(Value::as_u64) != Some(entry.created_at_ms)
            || session.pointer("/time/updated").and_then(Value::as_u64) != Some(entry.updated_at_ms)
            || actual_model(&session)? != entry.model
        {
            bail!("handoff metadata/watermark mismatch for {}", entry.id);
        }
        if session
            .pointer("/time/archived")
            .is_some_and(|v| !v.is_null())
        {
            bail!("cannot adopt archived session {}", entry.id);
        }
        let statuses = self.request_json(record, "GET", "/session/status", None)?;
        if running_session_ids(&statuses).contains(&entry.id) {
            bail!("handoff session {} is executing", entry.id);
        }
        let mut messages = self.request_json(
            record,
            "GET",
            &format!(
                "/session/{}/message{}",
                url_path_segment(&entry.id),
                // Adoption needs only the watermark; long imported histories
                // exceed the HTTP body limit. Resumption still validates tools.
                if entry.resume { "" } else { "?limit=1" }
            ),
            None,
        )?;
        if !entry.resume {
            // Keep continuation protection's user watermark without loading
            // every historical message just to find that one ID.
            let users = self.request_json(
                record,
                "GET",
                &format!(
                    "/session/{}/message?limit=1&type=user",
                    url_path_segment(&entry.id)
                ),
                None,
            )?;
            if let Some(user) = users.as_array().and_then(|items| items.last()) {
                let rows = messages
                    .as_array_mut()
                    .context("invalid handoff transcript")?;
                if !rows
                    .iter()
                    .any(|item| item["info"]["id"] == user["info"]["id"])
                {
                    rows.insert(0, user.clone());
                }
            }
        }
        let newest = messages
            .as_array()
            .and_then(|a| a.last())
            .and_then(|m| m.pointer("/info/id"))
            .and_then(Value::as_str);
        if newest != entry.last_message_id.as_deref() {
            bail!("handoff transcript watermark mismatch for {}", entry.id);
        }
        if entry.resume {
            for route in [
                format!("/session/{}/form", url_path_segment(&entry.id)),
                format!("/session/{}/permission", url_path_segment(&entry.id)),
            ] {
                let requests = self.request_json(record, "GET", &route, None)?;
                if request_session_ids(&requests).contains(&entry.id) {
                    bail!("handoff session {} is waiting for user input", entry.id);
                }
            }
            if session.get("parentID").is_some_and(|v| !v.is_null()) {
                bail!("only top-level handoff roots may resume");
            }
            validate_resumable(entry, &session, &messages)?;
        }
        Ok(messages)
    }

    fn handoff_input_exists(
        &self,
        record: &ServerRecord,
        session: &str,
        input: &str,
    ) -> Result<bool> {
        let route = format!(
            "/session/{}/message/{}",
            url_path_segment(session),
            url_path_segment(input)
        );
        match self.request_json(record, "GET", &route, None) {
            Ok(message) => {
                if message["id"] != input
                    || message["type"] != "user"
                    || message["text"] != "continue"
                {
                    bail!("handoff continuation ID collides with different input");
                }
                return Ok(true);
            }
            Err(error)
                if error
                    .downcast_ref::<HttpStatusError>()
                    .is_some_and(|e| e.status == 404) => {}
            Err(error) => return Err(error),
        }
        let inbox = self.request_json(
            record,
            "GET",
            &format!("/session/{}/inbox", url_path_segment(session)),
            None,
        )?;
        for item in inbox
            .as_array()
            .context("V2 inbox did not return an array")?
        {
            if item["id"] == input {
                if item["type"] != "user"
                    || item.pointer("/payload/text").and_then(Value::as_str) != Some("continue")
                {
                    bail!("handoff inbox ID collides with different input");
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn handoff_protects_continuation(
        &self,
        record: &ServerRecord,
        id: &str,
    ) -> Result<bool> {
        let ledger: Ledger = read_private(&self.state_dir.join(LEDGER))?.unwrap_or_default();
        let Some(root) = ledger.handoffs.values().find_map(|h| h.roots.get(id)) else {
            return Ok(false);
        };
        let messages = self.request_json(
            record,
            "GET",
            &format!(
                "/session/{}/message?limit=1&type=user",
                url_path_segment(id)
            ),
            None,
        )?;
        let current = latest_user(&messages);
        Ok(current == root.last_user_id || current == root.input_id)
    }
}

fn owned(entry: &Entry, messages: &Value) -> OwnedSession {
    OwnedSession {
        id: entry.id.clone(),
        cwd: entry.directory.clone(),
        title: entry.title.clone(),
        summary: latest_assistant_summary(messages)
            .unwrap_or_else(|| "Adopted from a verified V2 handoff".into()),
        created_at_ms: entry.created_at_ms,
        updated_at_ms: entry.updated_at_ms,
        server_updated_ms: 0,
    }
}

fn latest_user(messages: &Value) -> Option<String> {
    messages
        .as_array()?
        .iter()
        .rev()
        .find(|m| m.pointer("/info/role").and_then(Value::as_str) == Some("user"))?
        .pointer("/info/id")?
        .as_str()
        .map(str::to_owned)
}

fn actual_model(session: &Value) -> Result<Option<Model>> {
    session
        .get("model")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::from_value(v.clone()).context("invalid V2 session model"))
        .transpose()
}

fn validate_manifest(manifest: &Manifest) -> Result<()> {
    if manifest.version != 1
        || manifest.handoff_id.is_empty()
        || manifest.handoff_id.len() > 128
        || manifest.handoff_id.chars().any(char::is_control)
        || manifest.sessions.is_empty()
    {
        bail!("invalid handoff manifest header");
    }
    let mut seen = BTreeSet::new();
    for entry in &manifest.sessions {
        if !entry.id.starts_with("ses_")
            || !seen.insert(&entry.id)
            || !entry.directory.is_absolute()
            || entry.id.len() > 512
            || entry.id.chars().any(char::is_control)
        {
            bail!("invalid or duplicate handoff session");
        }
        if entry.resume
            && (entry.source_state != SourceState::Interrupted
                || !entry.tools_settled
                || !entry.external_actions_settled)
        {
            bail!("only explicitly interrupted, settled handoff roots may resume");
        }
    }
    Ok(())
}

fn check_endpoint(manifest: &Manifest, record: &ServerRecord, first: bool) -> Result<()> {
    if record.api != ApiVersion::V2
        || manifest.endpoint.url != format!("http://127.0.0.1:{}", record.port)
        || (first && manifest.endpoint.pid != record.pid)
    {
        bail!("handoff target does not match the verified V2 endpoint/process");
    }
    Ok(())
}

fn validate_resumable(entry: &Entry, session: &Value, messages: &Value) -> Result<()> {
    if entry.source_state != SourceState::Interrupted
        || !entry.tools_settled
        || !entry.external_actions_settled
    {
        bail!("handoff root is not cleared for continuation");
    }
    if matches!(session["outcome"].as_str(), Some("succeeded" | "failed")) {
        bail!("handoff root already completed");
    }
    let messages = messages.as_array().context("invalid handoff transcript")?;
    let tail = messages.iter().rev().find(|m| {
        matches!(
            m.pointer("/info/type").and_then(Value::as_str),
            Some("assistant" | "idle" | "user" | "shell" | "compaction")
        )
    });
    let Some(tail) = tail else {
        bail!("handoff continuation requires an interrupted assistant turn");
    };
    if tail.pointer("/info/type").and_then(Value::as_str) != Some("assistant") {
        bail!("handoff root is not an interrupted assistant turn");
    }
    let error = tail.pointer("/info/error/type").and_then(Value::as_str);
    if tail
        .pointer("/info/time/completed")
        .is_some_and(|v| !v.is_null())
        && !matches!(error, Some("aborted" | "interrupted"))
    {
        bail!("handoff assistant turn already completed");
    }
    for message in messages
        .iter()
        .rev()
        .take_while(|m| m.pointer("/info/type").and_then(Value::as_str) != Some("user"))
    {
        for part in message["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["type"] == "tool")
        {
            if !matches!(
                part.pointer("/state/status").and_then(Value::as_str),
                Some("completed" | "error")
            ) || part.pointer("/state/error/type").and_then(Value::as_str)
                == Some("tool.interrupted")
            {
                bail!("handoff contains an unsettled tool outcome");
            }
        }
    }
    Ok(())
}

fn new_input_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| anyhow::anyhow!("cannot allocate handoff input ID: {e}"))?;
    let mut id = String::with_capacity(36);
    id.push_str("msg_");
    for byte in bytes {
        std::fmt::Write::write_fmt(&mut id, format_args!("{byte:02x}"))?;
    }
    Ok(id)
}

fn read_private<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let metadata = file.metadata()?;
    verify_current_owner(&metadata, "handoff file")?;
    verify_private_mode(&metadata, "handoff file")?;
    if !metadata.is_file() || metadata.len() > MAX_BODY_BYTES as u64 {
        bail!("invalid handoff file");
    }
    Ok(Some(serde_json::from_reader(
        file.take(MAX_BODY_BYTES as u64 + 1),
    )?))
}

fn write_private<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let temp = path.with_extension(format!("tmp-{}-{}", std::process::id(), new_input_id()?));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temp)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.sync_all()?;
        crate::fs_util::replace_file(&temp, path)?;
        #[cfg(unix)]
        File::open(path.parent().context("handoff has no state directory")?)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Entry {
        serde_json::from_value(json!({"id":"ses_root","title":"Frozen task","directory":std::env::temp_dir().join("handoff-work"),"agent":"build","model":{"providerID":"p","id":"m"},"lastMessageID":"msg_assistant","createdAtMs":1,"updatedAtMs":2,"sourceState":"interrupted","resume":true,"toolsSettled":true,"externalActionsSettled":true})).unwrap()
    }

    fn interrupted() -> Value {
        json!([{"info":{"id":"msg_user","role":"user","type":"user"},"parts":[]},
            {"info":{"id":"msg_assistant","role":"assistant","type":"assistant","time":{"completed":2},"error":{"type":"aborted"}},"parts":[{"type":"tool","state":{"status":"completed"}}]}])
    }

    #[test]
    fn handoff_requires_explicit_interrupted_settled_roots_and_unique_ids() {
        let mut manifest = Manifest {
            version: 1,
            handoff_id: "frozen".into(),
            endpoint: Endpoint {
                url: "http://127.0.0.1:1234".into(),
                pid: 123,
            },
            sessions: vec![entry()],
        };
        assert!(validate_manifest(&manifest).is_ok());
        manifest.sessions[0].external_actions_settled = false;
        assert!(validate_manifest(&manifest).is_err());
        manifest.sessions[0] = entry();
        manifest.sessions[0].source_state = SourceState::Completed;
        assert!(validate_manifest(&manifest).is_err());
        manifest.sessions[0].resume = false;
        assert!(validate_manifest(&manifest).is_ok());
        manifest.sessions.push(manifest.sessions[0].clone());
        assert!(validate_manifest(&manifest).is_err());
        let mut value = serde_json::to_value(entry()).unwrap();
        value["resuem"] = json!(true);
        assert!(serde_json::from_value::<Entry>(value).is_err());
    }

    #[test]
    fn handoff_never_blindly_resumes_completed_idle_or_uncertain_tool_outcomes() {
        let entry = entry();
        let mut messages = interrupted();
        assert!(validate_resumable(&entry, &json!({}), &messages).is_ok());
        assert!(validate_resumable(&entry, &json!({"outcome":"succeeded"}), &messages).is_err());
        messages[1]["parts"][0]["state"]["status"] = json!("running");
        assert!(validate_resumable(&entry, &json!({}), &messages).is_err());
        messages[1]["parts"][0]["state"] =
            json!({"status":"error","error":{"type":"tool.interrupted"}});
        assert!(validate_resumable(&entry, &json!({}), &messages).is_err());
        messages = interrupted();
        messages[1]["info"].as_object_mut().unwrap().remove("error");
        assert!(validate_resumable(&entry, &json!({}), &messages).is_err());
        messages = interrupted();
        messages
            .as_array_mut()
            .unwrap()
            .push(json!({"info":{"type":"idle"},"parts":[]}));
        assert!(validate_resumable(&entry, &json!({}), &messages).is_err());
    }

    #[test]
    fn continuation_id_survives_private_journal_reload_and_protocol_translation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handoff.json");
        let id = new_input_id().unwrap();
        let root = Root {
            input_id: Some(id.clone()),
            last_user_id: Some("msg_source".into()),
            accepted: false,
        };
        write_private(&path, &root).unwrap();
        let loaded: Root = read_private(&path).unwrap().unwrap();
        assert_eq!(loaded.input_id, Some(id.clone()));
        let (_, body) = v2_request(
            "/session/ses_root/prompt_async",
            Some(&json!({"id":id,"parts":[{"type":"text","text":"continue"}]})),
        )
        .unwrap();
        assert_eq!(body.unwrap()["id"], id);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(read_private::<Root>(&path).is_err());
        }
    }
}

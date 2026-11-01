//! The tools the voice's backend model calls, run against the dashboard.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::digest::{self, Handles, Info, Known, Seen};
use crate::domain::SessionState;

/// One message of a session, as the voice reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub role: String,
    pub text: String,
}

/// What Voice Mode needs from the dashboard. The real one wraps the control
/// hub and the latest session snapshot; tests use a fake.
pub trait VoiceHost: Send + Sync {
    fn infos(&self) -> Vec<Info>;
    fn recent_messages(&self, id: &str, count: usize) -> Result<Vec<Message>>;
    fn send(&self, id: &str, text: &str) -> Result<()>;
    /// Asks for fresh session data; the dashboard may be busy elsewhere.
    fn refresh(&self) {}
    fn interrupt(&self, id: &str) -> Result<()>;
    fn edit_last(&self, id: &str, text: &str) -> Result<()>;
    /// Starts a session in `cwd` and returns its id.
    fn start(&self, cwd: &Path, task: &str) -> Result<String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Until {
    Done,
    NeedsYou,
}

struct Watch {
    id: String,
    until: Until,
    was_busy: bool,
}

pub struct Tools {
    host: Arc<dyn VoiceHost>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    handles: Mutex<Handles>,
    seen: Mutex<Seen>,
    known: Mutex<Known>,
    watches: Mutex<Vec<Watch>>,
    /// The session last read, messaged or started: what "it" and "that one" mean.
    focus: Mutex<Option<String>>,
    /// The session on the developer's screen: selected in the dashboard, or open.
    viewing: Mutex<Option<String>>,
    /// Folders whose children a spoken name may refer to, such as `~/Code`.
    roots: Vec<PathBuf>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

const READ_DEFAULT: usize = 6;
const READ_MAX: usize = 20;
const READ_MESSAGE_CHARS: usize = 1200;
const READ_TOTAL_CHARS: usize = 6000;

impl Tools {
    pub fn new(host: Arc<dyn VoiceHost>) -> Self {
        let mut tools = Self::with_clock(host, Arc::new(now_ms));
        tools.roots = std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Code"))
            .into_iter()
            .collect();
        tools
    }

    pub fn with_clock(host: Arc<dyn VoiceHost>, clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        Self {
            host,
            clock,
            handles: Mutex::default(),
            seen: Mutex::default(),
            known: Mutex::default(),
            watches: Mutex::default(),
            focus: Mutex::default(),
            viewing: Mutex::default(),
            roots: Vec::new(),
        }
    }

    /// Runs one tool. Whatever happens, the answer is JSON the model can read.
    pub fn run(&self, name: &str, args: &Value) -> Value {
        let result = match name {
            "list_sessions" => self.list(args),
            "read_session" => self.read(args),
            "send_message" => self.send(args),
            "interrupt_session" => self.interrupt(args),
            "edit_last_message" => self.edit_last(args),
            "start_session" => self.start(args),
            "watch_session" => self.watch(args),
            other => Err(anyhow::anyhow!("there is no tool called {other}")),
        };
        result.unwrap_or_else(|error| json!({"error": format!("{error:#}")}))
    }

    fn text(args: &Value, key: &str) -> Result<String> {
        match args.get(key).and_then(Value::as_str).map(str::trim) {
            Some(text) if !text.is_empty() => Ok(text.to_owned()),
            _ => bail!("{key} is required"),
        }
    }

    fn resolve(&self, args: &Value) -> Result<String> {
        let handle = Self::text(args, "id")?;
        let id = self
            .handles
            .lock()
            .unwrap()
            .id(&handle)
            .map(str::to_owned)
            .ok_or_else(|| {
                anyhow::anyhow!("{handle} is not a session id; call list_sessions first")
            })?;
        *self.focus.lock().unwrap() = Some(id.clone());
        Ok(id)
    }

    fn list(&self, args: &Value) -> Result<Value> {
        let infos = self.host.infos();
        let now = (self.clock)();
        let query = args.get("query").and_then(Value::as_str);
        let all = args.get("scope").and_then(Value::as_str) == Some("all");
        let (rows, parked) = if all || query.is_some_and(|query| !query.trim().is_empty()) {
            (digest::search(&infos, query), digest::Parked::default())
        } else {
            digest::recent(&infos, now)
        };
        let viewing = self.viewing.lock().unwrap().clone();
        let mut rows = rows;
        // The session on screen is always there, whether or not it is recent.
        if let Some(on_screen) = viewing
            .as_ref()
            .and_then(|id| infos.iter().find(|info| &info.id == id))
        {
            if !rows.iter().any(|info| info.id == on_screen.id) {
                rows.insert(0, on_screen);
            }
        }
        let mut handles = self.handles.lock().unwrap();
        let mut seen = self.seen.lock().unwrap();
        let sessions: Vec<Value> = rows
            .iter()
            .map(|info| {
                let mut row = seen.row(info, &handles.handle(&info.id), now);
                if viewing.as_deref() == Some(info.id.as_str()) {
                    row["on_screen"] = json!(true);
                }
                row
            })
            .collect();
        let mut out = json!({ "sessions": sessions });
        if parked.count > 0 {
            out["parked"] = json!(parked.count);
            out["parked_needing_you"] = json!(parked.needing_you);
            out["note"] =
                json!("parked sessions are older ones; use scope all or a query to find one");
        }
        Ok(out)
    }

    fn read(&self, args: &Value) -> Result<Value> {
        let id = self.resolve(args)?;
        let count = args
            .get("last_n")
            .and_then(Value::as_u64)
            .map_or(READ_DEFAULT, |n| (n as usize).clamp(1, READ_MAX));
        let messages = self.host.recent_messages(&id, count)?;
        let mut total = 0;
        let mut kept: Vec<Value> = Vec::new();
        for message in &messages {
            let text: String = message.text.chars().take(READ_MESSAGE_CHARS).collect();
            total += text.len();
            if total > READ_TOTAL_CHARS {
                break;
            }
            kept.push(json!({"role": message.role, "text": text}));
        }
        let messages = kept;
        if let Some(info) = self.host.infos().iter().find(|info| info.id == id) {
            self.seen.lock().unwrap().told(&id, &info.tail);
        }
        Ok(json!({ "messages": messages }))
    }

    fn send(&self, args: &Value) -> Result<Value> {
        let id = self.resolve(args)?;
        self.host.send(&id, &Self::text(args, "text")?)?;
        Ok(json!({"ok": true, "note": "sent; a busy session queues it"}))
    }

    fn interrupt(&self, args: &Value) -> Result<Value> {
        let id = self.resolve(args)?;
        self.host.interrupt(&id)?;
        Ok(json!({"ok": true}))
    }

    fn edit_last(&self, args: &Value) -> Result<Value> {
        let id = self.resolve(args)?;
        self.host.edit_last(&id, &Self::text(args, "text")?)?;
        Ok(
            json!({"ok": true, "note": "stopped it if it was running, rewound to before the last message and sent the corrected one"}),
        )
    }

    fn start(&self, args: &Value) -> Result<Value> {
        let folder = Self::text(args, "folder")?;
        let task = Self::text(args, "task")?;
        let cwd = self.resolve_folder(&folder)?;
        let id = self.host.start(&cwd, &task)?;
        let handle = self.handles.lock().unwrap().handle(&id);
        *self.focus.lock().unwrap() = Some(id);
        Ok(json!({
            "ok": true,
            "id": handle,
            "folder": cwd.file_name().map(|name| name.to_string_lossy().into_owned()),
            "note": "started; it shows up in list_sessions in a few seconds",
        }))
    }

    fn watch(&self, args: &Value) -> Result<Value> {
        let id = self.resolve(args)?;
        let until = match args.get("until").and_then(Value::as_str) {
            Some("needs_you") => Until::NeedsYou,
            _ => Until::Done,
        };
        let infos = self.host.infos();
        let info = infos.iter().find(|info| info.id == id);
        match (info, until) {
            (Some(info), Until::Done) if !info.busy() => {
                return Ok(
                    json!({"already": true, "note": format!("it is already {}", info.state_word())}),
                );
            }
            (Some(info), Until::NeedsYou) if info.needs_you() => {
                return Ok(json!({"already": true, "note": "it already needs the developer"}));
            }
            _ => {}
        }
        let mut watches = self.watches.lock().unwrap();
        watches.retain(|watch| !(watch.id == id && watch.until == until));
        watches.push(Watch {
            id,
            until,
            was_busy: info.is_some_and(Info::busy),
        });
        Ok(json!({"ok": true, "note": "the developer will hear when it happens"}))
    }

    /// Turns a spoken folder into a directory.
    fn resolve_folder(&self, folder: &str) -> Result<PathBuf> {
        let infos = self.host.infos();
        let spoken = folder.trim();
        if matches!(
            spoken.to_lowercase().as_str(),
            "same" | "here" | "this" | "this repository" | "same folder"
        ) {
            let focus = self.focus.lock().unwrap().clone();
            let info = focus
                .and_then(|id| infos.iter().find(|info| info.id == id))
                .or_else(|| infos.iter().max_by_key(|info| info.human_ms()));
            return match info {
                Some(info) => Ok(info.cwd.clone()),
                None => bail!("there is no session to take the folder from"),
            };
        }
        if spoken.starts_with('/') || spoken.starts_with('~') {
            let path = match spoken.strip_prefix("~/") {
                Some(rest) => {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest)
                }
                None => PathBuf::from(spoken),
            };
            if path.is_dir() {
                return Ok(path);
            }
            bail!("{spoken} is not a folder");
        }
        // "agent view" and "agentview" are the same folder.
        let squash = |text: &str| {
            text.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        };
        let wanted = squash(spoken);
        if wanted.is_empty() {
            bail!("no folder matches {spoken}");
        }
        let mut folders: HashSet<PathBuf> = infos.iter().map(|info| info.cwd.clone()).collect();
        for root in &self.roots {
            if let Ok(entries) = std::fs::read_dir(root) {
                folders.extend(
                    entries
                        .flatten()
                        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                        .map(|entry| entry.path())
                        .filter(|path| path.is_dir()),
                );
            }
        }
        let name = |path: &Path| {
            path.file_name()
                .map(|n| squash(&n.to_string_lossy()))
                .unwrap_or_default()
        };
        let mut exact: Vec<&PathBuf> = folders.iter().filter(|path| name(path) == wanted).collect();
        exact.sort();
        // One folder used by several sessions is one folder.
        exact.dedup();
        match exact.len() {
            1 => return Ok(exact[0].clone()),
            0 => {}
            _ => bail!(
                "more than one folder is called {spoken}: {}",
                exact
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
        let mut partial: Vec<&PathBuf> = folders
            .iter()
            .filter(|path| name(path).contains(&wanted))
            .collect();
        partial.sort();
        match partial.len() {
            1 => Ok(partial[0].clone()),
            0 => bail!("no folder matches {spoken}"),
            _ => bail!(
                "{spoken} could be {}",
                partial
                    .iter()
                    .map(|p| p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// Spoken lines for watches that fired, and quiet notes on what changed.
    pub fn poll(&self) -> Polled {
        let infos = self.host.infos();
        let mut polled = Polled::default();
        let mut watches = self.watches.lock().unwrap();
        watches.retain_mut(|watch| {
            let Some(info) = infos.iter().find(|info| info.id == watch.id) else {
                return true;
            };
            let fired = match watch.until {
                Until::Done => {
                    let busy = info.busy();
                    let finished = watch.was_busy && !busy;
                    watch.was_busy = busy;
                    finished
                }
                Until::NeedsYou => info.needs_you(),
            };
            if fired {
                let said = if info.tail.is_empty() {
                    String::new()
                } else {
                    format!(" It ends: {}", info.tail)
                };
                let line = match (watch.until, info.state) {
                    (Until::NeedsYou, _) | (_, SessionState::NeedsInput) => {
                        format!("\"{}\" now needs the developer.{said}", info.title)
                    }
                    _ => format!("\"{}\" finished.{said}", info.title),
                };
                self.seen.lock().unwrap().told(&info.id, &info.tail);
                polled.spoken.push(line);
            }
            !fired
        });
        let spoken_ids: HashSet<&str> = watches.iter().map(|w| w.id.as_str()).collect();
        let _ = spoken_ids;
        let recent: Vec<Info> = digest::recent(&infos, (self.clock)())
            .0
            .into_iter()
            .cloned()
            .collect();
        for change in self.known.lock().unwrap().changes(&recent) {
            if polled.spoken.iter().any(|line| line.contains(&change.text)) {
                continue;
            }
            polled.quiet.push(change.text);
        }
        polled
    }

    /// Whether anything is being watched, which keeps a quiet call open.
    pub fn watching(&self) -> bool {
        !self.watches.lock().unwrap().is_empty()
    }

    pub fn notes(&self) -> String {
        let infos = self.host.infos();
        let mut notes = digest::notes(&infos, (self.clock)());
        if let Some(line) = self.viewing_line(&infos) {
            notes.push('\n');
            notes.push_str(&line);
        }
        notes
    }

    fn viewing_line(&self, infos: &[Info]) -> Option<String> {
        let id = self.viewing.lock().unwrap().clone()?;
        let info = infos.iter().find(|info| info.id == id)?;
        Some(format!(
            "The developer is looking at \"{}\" ({}) right now: \"this\", \"this one\" and \"it\" mean that session unless they name another.",
            info.title,
            info.folder()
        ))
    }

    /// The session on screen changed. Returns what the voice should be told,
    /// when it differs from what it knew.
    pub fn set_viewing(&self, id: Option<String>) -> Option<String> {
        {
            let mut viewing = self.viewing.lock().unwrap();
            if *viewing == id {
                return None;
            }
            *viewing = id.clone();
        }
        if let Some(id) = &id {
            *self.focus.lock().unwrap() = Some(id.clone());
        }
        let infos = self.host.infos();
        match id {
            Some(_) => self
                .viewing_line(&infos)
                .map(|line| line.replace("is looking at", "is now looking at")),
            None => Some("The developer is not looking at a particular session now.".into()),
        }
    }

    /// Records what the sessions look like now without reporting changes.
    pub fn prime(&self) {
        let infos = self.host.infos();
        let recent: Vec<Info> = digest::recent(&infos, (self.clock)())
            .0
            .into_iter()
            .cloned()
            .collect();
        self.known.lock().unwrap().changes(&recent);
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Polled {
    /// Said aloud: a session the developer asked about finished.
    pub spoken: Vec<String>,
    /// Told to the model without saying it.
    pub quiet: Vec<String>,
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;

    #[derive(Default)]
    pub struct FakeHost {
        pub infos: Mutex<Vec<Info>>,
        pub log: Mutex<Vec<String>>,
        pub fail: Mutex<Option<String>>,
    }

    impl FakeHost {
        pub fn with(infos: Vec<Info>) -> Arc<Self> {
            Arc::new(Self {
                infos: Mutex::new(infos),
                ..Self::default()
            })
        }

        fn record(&self, line: String) -> Result<()> {
            if let Some(error) = self.fail.lock().unwrap().clone() {
                bail!("{error}");
            }
            self.log.lock().unwrap().push(line);
            Ok(())
        }
    }

    impl VoiceHost for FakeHost {
        fn infos(&self) -> Vec<Info> {
            self.infos.lock().unwrap().clone()
        }
        fn recent_messages(&self, id: &str, count: usize) -> Result<Vec<Message>> {
            self.record(format!("read {id} {count}"))?;
            Ok(vec![
                Message {
                    role: "user".into(),
                    text: "Run the benchmark.".into(),
                },
                Message {
                    role: "assistant".into(),
                    text: "x".repeat(3000),
                },
            ])
        }
        fn send(&self, id: &str, text: &str) -> Result<()> {
            self.record(format!("send {id} {text}"))
        }
        fn interrupt(&self, id: &str) -> Result<()> {
            self.record(format!("interrupt {id}"))
        }
        fn edit_last(&self, id: &str, text: &str) -> Result<()> {
            self.record(format!("edit {id} {text}"))
        }
        fn start(&self, cwd: &Path, task: &str) -> Result<String> {
            self.record(format!("start {} {task}", cwd.display()))?;
            Ok("opencode:host:ses_new".into())
        }
    }

    pub fn info(id: &str, title: &str, folder: &str, state: SessionState) -> Info {
        Info {
            id: id.into(),
            title: title.into(),
            cwd: PathBuf::from(format!("/work/{folder}")),
            state,
            updated_ms: 1_000_000,
            last_user_ms: 1_000_000,
            tail: String::new(),
            doing: None,
            subagents: 0,
            paused: false,
            top_level: true,
            can_message: true,
            why: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    fn tools(infos: Vec<Info>) -> (Arc<FakeHost>, Tools) {
        let host = FakeHost::with(infos);
        let tools = Tools::with_clock(host.clone(), Arc::new(|| 1_100_000));
        (host, tools)
    }

    fn two() -> Vec<Info> {
        vec![
            info(
                "opencode:host:a",
                "semantic search",
                "arca",
                SessionState::NeedsInput,
            ),
            info(
                "opencode:host:b",
                "voice mode",
                "agentview",
                SessionState::Working,
            ),
        ]
    }

    #[test]
    fn listing_hands_out_short_ids_that_later_calls_accept() {
        let (host, tools) = tools(two());
        let listed = tools.run("list_sessions", &json!({}));
        let first = listed["sessions"][0]["id"].as_str().unwrap().to_owned();
        assert_eq!(first, "s1");
        tools.run("send_message", &json!({"id": first, "text": "go ahead"}));
        assert_eq!(
            host.log.lock().unwrap().as_slice(),
            ["send opencode:host:a go ahead"]
        );
    }

    #[test]
    fn an_id_never_listed_is_refused_with_a_way_forward() {
        let (_, tools) = tools(two());
        let result = tools.run("send_message", &json!({"id": "s7", "text": "x"}));
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains("call list_sessions first"));
    }

    #[test]
    fn a_host_failure_comes_back_as_an_error_the_model_can_say() {
        let (host, tools) = tools(two());
        tools.run("list_sessions", &json!({}));
        *host.fail.lock().unwrap() = Some("needs recovery in OpenCode".into());
        let result = tools.run("interrupt_session", &json!({"id": "s1"}));
        assert_eq!(result["error"], "needs recovery in OpenCode");
    }

    #[test]
    fn reading_trims_long_messages_and_defaults_to_six() {
        let (host, tools) = tools(two());
        tools.run("list_sessions", &json!({}));
        let read = tools.run("read_session", &json!({"id": "s1"}));
        assert_eq!(host.log.lock().unwrap()[0], "read opencode:host:a 6");
        assert_eq!(
            read["messages"][1]["text"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            1200
        );
        tools.run("read_session", &json!({"id": "s1", "last_n": 500}));
        assert_eq!(host.log.lock().unwrap()[1], "read opencode:host:a 20");
    }

    #[test]
    fn a_session_read_is_not_summarised_again_in_the_next_listing() {
        let mut infos = two();
        infos[0].tail = "Rerun the benchmark?".into();
        let (_, tools) = tools(infos);
        tools.run("list_sessions", &json!({}));
        let again = tools.run("list_sessions", &json!({}));
        assert_eq!(
            again["sessions"][0]["last_said"],
            "(unchanged since you last heard it)"
        );
    }

    #[test]
    fn a_query_searches_every_session() {
        let mut infos = two();
        for n in 0..12 {
            infos.push(info(
                &format!("opencode:host:x{n}"),
                "filler",
                "misc",
                SessionState::Completed,
            ));
        }
        infos[1].last_user_ms = 0;
        infos[1].updated_ms = 5;
        let (_, tools) = tools(infos);
        let found = tools.run("list_sessions", &json!({"query": "voice"}));
        assert_eq!(found["sessions"][0]["title"], "voice mode");
        assert!(found.get("parked").is_none());
    }

    #[test]
    fn same_folder_means_the_session_just_discussed() {
        let (host, tools) = tools(two());
        tools.run("list_sessions", &json!({}));
        tools.run("read_session", &json!({"id": "s2"}));
        let started = tools.run(
            "start_session",
            &json!({"folder": "same", "task": "fix the bug"}),
        );
        assert_eq!(started["ok"], true);
        assert_eq!(
            host.log.lock().unwrap().last().unwrap(),
            "start /work/agentview fix the bug"
        );
        // The new session can be corrected at once, before the dashboard lists it.
        let handle = started["id"].as_str().unwrap().to_owned();
        tools.run(
            "edit_last_message",
            &json!({"id": handle, "text": "make it about Y"}),
        );
        assert_eq!(
            host.log.lock().unwrap().last().unwrap(),
            "edit opencode:host:ses_new make it about Y"
        );
    }

    #[test]
    fn a_spoken_folder_name_matches_a_known_folder_or_asks_which() {
        let mut infos = two();
        infos.push(info(
            "opencode:host:c",
            "other",
            "agentview-wt",
            SessionState::Completed,
        ));
        let (host, tools) = tools(infos);
        let exact = tools.run("start_session", &json!({"folder": "ARCA", "task": "look"}));
        assert_eq!(exact["ok"], true);
        assert_eq!(
            host.log.lock().unwrap().last().unwrap(),
            "start /work/arca look"
        );
        let clash = tools.run(
            "start_session",
            &json!({"folder": "agentview", "task": "look"}),
        );
        assert_eq!(clash["ok"], true, "an exact name beats a partial one");
        let missing = tools.run(
            "start_session",
            &json!({"folder": "nowhere-at-all", "task": "look"}),
        );
        assert!(missing["error"]
            .as_str()
            .unwrap()
            .contains("no folder matches"));
    }

    #[test]
    fn the_session_on_screen_is_marked_and_always_listed() {
        let mut infos = two();
        for n in 0..12 {
            infos.push(info(
                &format!("opencode:host:x{n}"),
                "filler",
                "misc",
                SessionState::Completed,
            ));
        }
        // The on-screen one is very old, so it is not "recent".
        infos[0].last_user_ms = 1;
        infos[0].updated_ms = 1;
        let (_, tools) = tools(infos);
        assert!(tools.set_viewing(Some("opencode:host:a".into())).is_some());
        let listed = tools.run("list_sessions", &json!({}));
        let on_screen: Vec<&Value> = listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["on_screen"] == true)
            .collect();
        assert_eq!(on_screen.len(), 1);
        assert_eq!(on_screen[0]["title"], "semantic search");
        assert!(tools
            .notes()
            .contains("looking at \"semantic search\" (arca) right now"));
        // "Start one in the same folder" means the folder on screen.
        let started = tools.run("start_session", &json!({"folder": "same", "task": "go"}));
        assert_eq!(started["folder"], "arca");
    }

    #[test]
    fn a_change_of_session_on_screen_is_reported_once() {
        let (_, tools) = tools(two());
        assert!(tools
            .set_viewing(Some("opencode:host:a".into()))
            .unwrap()
            .contains("looking at"));
        assert_eq!(tools.set_viewing(Some("opencode:host:a".into())), None);
        let moved = tools.set_viewing(Some("opencode:host:b".into())).unwrap();
        assert!(moved.contains("now looking at \"voice mode\""));
        assert!(tools
            .set_viewing(None)
            .unwrap()
            .contains("not looking at a particular session"));
    }

    #[test]
    fn spaces_and_dashes_in_a_spoken_folder_do_not_matter() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("voice-scratch")).unwrap();
        std::fs::create_dir(root.path().join(".hidden")).unwrap();
        let (_, mut tools) = tools(two());
        tools.roots = vec![root.path().to_owned()];
        let found = tools.run(
            "start_session",
            &json!({"folder": "voice scratch", "task": "hello"}),
        );
        assert_eq!(found["ok"], true, "{found}");
        // A session's own folder is found the same way.
        let known = tools.run(
            "start_session",
            &json!({"folder": "Agent View", "task": "hello"}),
        );
        assert_eq!(known["ok"], true, "{known}");
    }

    #[test]
    fn a_watch_on_a_busy_session_speaks_once_when_it_finishes() {
        let (host, tools) = tools(two());
        tools.run("list_sessions", &json!({}));
        let watched = tools.run("watch_session", &json!({"id": "s2"}));
        assert_eq!(watched["ok"], true);
        assert!(tools.watching());
        assert!(tools.poll().spoken.is_empty());
        {
            let mut infos = host.infos.lock().unwrap();
            infos[1].state = SessionState::Completed;
            infos[1].tail = "Merged.".into();
        }
        let polled = tools.poll();
        assert_eq!(polled.spoken.len(), 1);
        assert!(polled.spoken[0].contains("voice mode") && polled.spoken[0].contains("Merged."));
        assert!(!tools.watching());
        assert!(tools.poll().spoken.is_empty());
    }

    #[test]
    fn watching_an_idle_session_says_so_instead_of_waiting_forever() {
        let (_, tools) = tools(vec![info(
            "opencode:host:a",
            "t",
            "f",
            SessionState::Completed,
        )]);
        tools.run("list_sessions", &json!({}));
        let result = tools.run("watch_session", &json!({"id": "s1"}));
        assert_eq!(result["already"], true);
        assert!(!tools.watching());
    }

    #[test]
    fn a_session_asking_for_the_developer_is_reported_to_the_watcher() {
        let (host, tools) = tools(two());
        tools.run("list_sessions", &json!({}));
        tools.run("watch_session", &json!({"id": "s2", "until": "needs_you"}));
        host.infos.lock().unwrap()[1].state = SessionState::NeedsInput;
        assert!(tools.poll().spoken[0].contains("needs the developer"));
    }
}

//! The tools the voice's backend model calls, run against the dashboard.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::digest::{self, Handles, Info, Known, Seen};
use crate::domain::SessionState;

/// One exchange of a session, as the voice reads it: what the developer
/// asked and how the agent ended its turn. Tool calls and scripts are left out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub you: String,
    pub it: String,
}

/// A session found by searching everything OpenCode remembers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryHit {
    pub id: String,
    pub title: String,
    pub cwd: PathBuf,
    pub updated_ms: u64,
    /// Where the words matched, when they matched inside the conversation.
    pub snippet: String,
}

/// What Voice Mode needs from the dashboard. The real one wraps the control
/// hub and the latest session snapshot; tests use a fake.
pub trait VoiceHost: Send + Sync {
    fn infos(&self) -> Vec<Info>;
    fn recent_turns(&self, id: &str, turns: usize) -> Result<Vec<Turn>>;
    /// Sessions of the whole history matching `query`, best first, and a note
    /// when the answer may be incomplete.
    fn search_history(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<(Vec<HistoryHit>, Option<String>)>;
    /// Stops what the session is doing and removes it from Agent View. Its
    /// history stays and `search_history` still finds it.
    fn close(&self, id: &str) -> Result<String>;
    /// Sends the final message of a draft the developer started typing into the
    /// session's input box, and empties the box.
    fn send_draft(&self, id: &str, text: &str) -> Result<()>;
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
    registered: u64,
    /// The session was just given work and may not show as busy yet.
    expects_busy: bool,
}

/// How long after a message the session may still show as idle.
const SETTLE_MS: u64 = 60_000;
/// How long a watch waits to see work start before it calls it already done.
const NEVER_BUSY_MS: u64 = 45_000;

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
    /// When recent `read_session` calls ran, to stop one request reading everything.
    reads: Mutex<Vec<u64>>,
    /// When each session was last given work by a tool.
    touched: Mutex<std::collections::HashMap<String, u64>>,
    /// Folders whose children a spoken name may refer to, such as `~/Code`.
    roots: Vec<PathBuf>,
    /// What was typed into the on-screen session's input box when the call
    /// started, and which session that is.
    draft: Mutex<Option<(String, String)>>,
}

/// Text cut to `max` characters, keeping the start.
fn clip_start(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}…")
}

/// Text cut to `max` characters, keeping the end, where a turn's result or
/// question is.
fn clip_end(text: &str, max: usize) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let tail: String = text.chars().skip(count - max).collect();
    format!("…{tail}")
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// Sessions read in a row before the backend is told to answer from what it has.
const READS_PER_WINDOW: usize = 5;
const READ_WINDOW_MS: u64 = 30_000;
const READ_DEFAULT: usize = 3;
const READ_MAX: usize = 10;
/// What the developer asked: the start says what it was.
const READ_YOU_CHARS: usize = 700;
/// How a turn ended: the end holds the result or the question.
const READ_IT_CHARS: usize = 1800;
const READ_TOTAL_CHARS: usize = 7000;
const SEARCH_LIMIT: usize = 8;
const MATCH_CHARS: usize = 240;

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
            reads: Mutex::default(),
            touched: Mutex::default(),
            roots: Vec::new(),
            draft: Mutex::default(),
        }
    }

    /// The draft in the input box of the session on screen, or none.
    pub fn set_draft(&self, text: Option<String>) {
        let session = self.viewing.lock().unwrap().clone();
        *self.draft.lock().unwrap() = text
            .filter(|text| !text.trim().is_empty())
            .zip(session)
            .map(|(text, session)| (session, text));
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
            "search_history" => self.search(args),
            "close_session" => self.close(args),
            "send_draft" => self.send_draft(args),
            "watch_session" => self.watch(args),
            "end_call" => Ok(
                json!({"ok": true, "note": "the call ends in a few seconds: say a short goodbye"}),
            ),
            other => Err(anyhow::anyhow!("there is no tool called {other}")),
        };
        result.unwrap_or_else(|error| json!({"error": format!("{error:#}")}))
    }

    fn touch(&self, id: &str) {
        self.touched
            .lock()
            .unwrap()
            .insert(id.to_owned(), (self.clock)());
    }

    fn text(args: &Value, key: &str) -> Result<String> {
        match args.get(key).and_then(Value::as_str).map(str::trim) {
            Some(text) if !text.is_empty() => Ok(text.to_owned()),
            _ => bail!("{key} is required"),
        }
    }

    /// The session an `id` argument means: a short id from this call, "this" for
    /// the one on screen, "it" for the one last discussed, or part of a title.
    fn resolve(&self, args: &Value) -> Result<String> {
        let given = Self::text(args, "id")?;
        let known = self.handles.lock().unwrap().id(&given).map(str::to_owned);
        let id = match known {
            Some(id) => id,
            None => self.resolve_by_words(&given)?,
        };
        *self.focus.lock().unwrap() = Some(id.clone());
        Ok(id)
    }

    fn resolve_by_words(&self, given: &str) -> Result<String> {
        let words = given.trim().to_lowercase();
        let remembered = |slot: &Mutex<Option<String>>| slot.lock().unwrap().clone();
        match words.as_str() {
            "this" | "this session" | "current" | "on_screen" | "on screen" => {
                if let Some(id) = remembered(&self.viewing) {
                    return Ok(id);
                }
            }
            "it" | "that" | "that one" | "last" => {
                if let Some(id) = remembered(&self.focus) {
                    return Ok(id);
                }
            }
            _ => {}
        }
        let infos = self.host.infos();
        let found: Vec<&Info> = infos
            .iter()
            .filter(|info| !words.is_empty() && info.title.to_lowercase().contains(&words))
            .collect();
        match found.as_slice() {
            [one] => {
                self.handles.lock().unwrap().handle(&one.id);
                Ok(one.id.clone())
            }
            [] => bail!("{given} is not a session id or a session title; call list_sessions first"),
            many => bail!(
                "more than one session matches {given}: {}",
                many.iter()
                    .map(|info| format!("\"{}\"", info.title))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
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
        let searching = all || query.is_some_and(|query| !query.trim().is_empty());
        // The session on screen is always in the default list, recent or not.
        if let Some(on_screen) = viewing
            .as_ref()
            .filter(|_| !searching)
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
        {
            let now = (self.clock)();
            let mut reads = self.reads.lock().unwrap();
            reads.retain(|at| now.saturating_sub(*at) < READ_WINDOW_MS);
            if reads.len() >= READS_PER_WINDOW {
                bail!("enough sessions have been read for one request: answer from what you have; list_sessions already carries what each session last said");
            }
            reads.push(now);
        }
        let id = self.resolve(args)?;
        let count = args
            .get("turns")
            .or_else(|| args.get("last_n"))
            .and_then(Value::as_u64)
            .map_or(READ_DEFAULT, |n| (n as usize).clamp(1, READ_MAX));
        let turns = self.host.recent_turns(&id, count)?;
        // The newest turns matter most: when the total is too long, the oldest go.
        let mut kept: Vec<Value> = Vec::new();
        let mut total = 0;
        for turn in turns.iter().rev() {
            let you = clip_start(&turn.you, READ_YOU_CHARS);
            let it = clip_end(&turn.it, READ_IT_CHARS);
            total += you.len() + it.len();
            if total > READ_TOTAL_CHARS && !kept.is_empty() {
                break;
            }
            kept.push(json!({"you": you, "it": it}));
        }
        kept.reverse();
        let infos = self.host.infos();
        let info = infos.iter().find(|info| info.id == id);
        if let Some(info) = info {
            self.seen.lock().unwrap().told(&id, &info.tail);
        }
        let mut out = json!({ "turns": kept });
        if let Some(info) = info {
            out["title"] = json!(info.title);
            if info.busy() {
                out["still_working"] =
                    json!("the last turn is not finished: its \"it\" is only what it said so far");
            }
        }
        if kept.is_empty() {
            out["note"] = json!("nothing has been said in this session yet");
        }
        Ok(out)
    }

    fn search(&self, args: &Value) -> Result<Value> {
        let query = Self::text(args, "query")?;
        let (hits, note) = self.host.search_history(&query, SEARCH_LIMIT)?;
        let now = (self.clock)();
        let mut handles = self.handles.lock().unwrap();
        let results: Vec<Value> = hits
            .iter()
            .map(|hit| {
                let mut row = json!({
                    "id": handles.handle(&hit.id),
                    "title": hit.title,
                    "folder": hit.cwd.file_name().map(|name| name.to_string_lossy().into_owned()),
                    "last_active": digest::ago(now, hit.updated_ms),
                });
                if !hit.snippet.trim().is_empty() {
                    row["matched"] = json!(clip_start(
                        &hit.snippet.split_whitespace().collect::<Vec<_>>().join(" "),
                        MATCH_CHARS
                    ));
                }
                row
            })
            .collect();
        let mut out = json!({ "sessions": results });
        if let Some(note) = note {
            out["note"] = json!(note);
        } else if hits.is_empty() {
            out["note"] = json!("nothing in the history matches; try other words from the conversation or its title");
        }
        Ok(out)
    }

    fn send_draft(&self, args: &Value) -> Result<Value> {
        let text = Self::text(args, "text")?;
        let Some((session, _)) = self.draft.lock().unwrap().clone() else {
            bail!("the developer did not start a message before this call: use send_message");
        };
        self.host.send_draft(&session, &text)?;
        *self.draft.lock().unwrap() = None;
        self.touch(&session);
        Ok(json!({"ok": true, "note": "sent to the session and the input box is empty again"}))
    }

    fn close(&self, args: &Value) -> Result<Value> {
        let id = self.resolve(args)?;
        let done = self.host.close(&id)?;
        self.watches.lock().unwrap().retain(|watch| watch.id != id);
        Ok(json!({"ok": true, "note": done}))
    }

    fn send(&self, args: &Value) -> Result<Value> {
        let id = self.resolve(args)?;
        self.host.send(&id, &Self::text(args, "text")?)?;
        self.touch(&id);
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
        self.touch(&id);
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
        self.touch(&id);
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
        let now = (self.clock)();
        let just_given_work = self
            .touched
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(|at| now.saturating_sub(*at) < SETTLE_MS);
        match (info, until) {
            (Some(info), Until::Done) if !info.busy() && !just_given_work => {
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
            registered: now,
            expects_busy: just_given_work,
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
        let now = (self.clock)();
        let mut watches = self.watches.lock().unwrap();
        watches.retain_mut(|watch| {
            let Some(info) = infos.iter().find(|info| info.id == watch.id) else {
                return true;
            };
            let fired = match watch.until {
                Until::Done => {
                    let busy = info.busy();
                    watch.was_busy |= busy;
                    // Work given a moment ago can start and end between two looks.
                    let never_seen_busy = watch.expects_busy
                        && !watch.was_busy
                        && !busy
                        && now.saturating_sub(watch.registered) >= NEVER_BUSY_MS;
                    (watch.was_busy && !busy) || never_seen_busy
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
                self.known.lock().unwrap().record(info);
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

    /// Whether a tool only looks and never changes anything.
    pub fn is_read_only(name: &str) -> bool {
        matches!(name, "list_sessions" | "read_session" | "search_history")
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
        if let Some((session, text)) = self.draft.lock().unwrap().clone() {
            let title = infos
                .iter()
                .find(|info| info.id == session)
                .map_or("the session on screen".to_owned(), |info| {
                    format!("\"{}\"", info.title)
                });
            notes.push_str(&format!(
                "\nBefore the call the developer had started typing this into {title}'s input box: \"{}\". They may want to think it through with you. When they approve it, send the whole message with send_draft: their draft plus what you agreed, in their words.",
                clip_start(&text, 1500)
            ));
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
        pub hits: Mutex<Vec<HistoryHit>>,
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
        fn recent_turns(&self, id: &str, turns: usize) -> Result<Vec<Turn>> {
            self.record(format!("read {id} {turns}"))?;
            Ok(vec![
                Turn {
                    you: "Run the benchmark.".into(),
                    it: "x".repeat(3000),
                },
                Turn {
                    you: "y".repeat(2000),
                    it: "Done. Want me to merge it?".into(),
                },
            ])
        }
        fn search_history(
            &self,
            query: &str,
            limit: usize,
        ) -> Result<(Vec<HistoryHit>, Option<String>)> {
            self.record(format!("search {query} {limit}"))?;
            Ok((self.hits.lock().unwrap().clone(), None))
        }
        fn send_draft(&self, id: &str, text: &str) -> Result<()> {
            self.record(format!("send_draft {id} {text}"))
        }
        fn close(&self, id: &str) -> Result<String> {
            self.record(format!("close {id}"))?;
            Ok("stopped it and removed it from Agent View".into())
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
    fn a_session_can_be_named_by_its_title_this_or_it() {
        let (host, tools) = tools(two());
        tools.run("send_message", &json!({"id": "VOICE mode", "text": "go"}));
        assert_eq!(
            host.log.lock().unwrap().last().unwrap(),
            "send opencode:host:b go"
        );
        tools.run("interrupt_session", &json!({"id": "it"}));
        assert_eq!(
            host.log.lock().unwrap().last().unwrap(),
            "interrupt opencode:host:b"
        );
        tools.set_viewing(Some("opencode:host:a".into()));
        tools.run("interrupt_session", &json!({"id": "this"}));
        assert_eq!(
            host.log.lock().unwrap().last().unwrap(),
            "interrupt opencode:host:a"
        );
        let none = tools.run("interrupt_session", &json!({"id": "dentist"}));
        assert!(none["error"]
            .as_str()
            .unwrap()
            .contains("not a session id or a session title"));
    }

    #[test]
    fn a_title_that_matches_two_sessions_asks_which() {
        let mut infos = two();
        infos.push(info(
            "opencode:host:c",
            "voice notes",
            "misc",
            SessionState::Completed,
        ));
        let (_, tools) = tools(infos);
        let result = tools.run("read_session", &json!({"id": "voice"}));
        let error = result["error"].as_str().unwrap();
        assert!(
            error.contains("more than one session")
                && error.contains("voice mode")
                && error.contains("voice notes")
        );
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
    fn reading_gives_what_was_asked_and_how_each_turn_ended() {
        let (host, tools) = tools(two());
        tools.run("list_sessions", &json!({}));
        let read = tools.run("read_session", &json!({"id": "s1"}));
        assert_eq!(host.log.lock().unwrap()[0], "read opencode:host:a 3");
        // The request keeps its start, a long ending keeps its end.
        let first = &read["turns"][0];
        assert_eq!(first["you"], "Run the benchmark.");
        let it = first["it"].as_str().unwrap();
        assert_eq!(it.chars().count(), READ_IT_CHARS + 1);
        assert!(it.starts_with('…'));
        let second = &read["turns"][1];
        assert_eq!(
            second["you"].as_str().unwrap().chars().count(),
            READ_YOU_CHARS + 1
        );
        assert_eq!(second["it"], "Done. Want me to merge it?");
        assert!(read.get("messages").is_none());
        tools.run("read_session", &json!({"id": "s1", "turns": 500}));
        assert_eq!(host.log.lock().unwrap()[1], "read opencode:host:a 10");
        tools.run("read_session", &json!({"id": "s1", "turns": 0}));
        assert_eq!(host.log.lock().unwrap()[2], "read opencode:host:a 1");
    }

    #[test]
    fn a_session_still_working_says_its_last_turn_is_unfinished() {
        let (_, tools) = tools(two());
        tools.run("list_sessions", &json!({}));
        let working = tools.run("read_session", &json!({"id": "s2"}));
        assert!(working["still_working"].is_string());
        let waiting = tools.run("read_session", &json!({"id": "s1"}));
        assert!(waiting.get("still_working").is_none());
    }

    #[test]
    fn searching_the_history_hands_out_ids_the_other_tools_accept() {
        let (host, tools) = tools(two());
        host.hits.lock().unwrap().push(HistoryHit {
            id: "opencode:host:ses_old".into(),
            title: "library digitization pilot".into(),
            cwd: PathBuf::from("/work/digitalization"),
            updated_ms: 1_000_000,
            snippet: "the  pilot\nwith tokens".into(),
        });
        let found = tools.run("search_history", &json!({"query": "digitization pilot"}));
        assert_eq!(host.log.lock().unwrap()[0], "search digitization pilot 8");
        let row = &found["sessions"][0];
        assert_eq!(row["title"], "library digitization pilot");
        assert_eq!(row["folder"], "digitalization");
        assert_eq!(row["matched"], "the pilot with tokens");
        let handle = row["id"].as_str().unwrap();
        tools.run("send_message", &json!({"id": handle, "text": "continue"}));
        assert_eq!(
            host.log.lock().unwrap()[1],
            "send opencode:host:ses_old continue"
        );
        host.hits.lock().unwrap().clear();
        let none = tools.run("search_history", &json!({"query": "zebra"}));
        assert!(none["note"].as_str().unwrap().contains("nothing"));
        assert!(tools.run("search_history", &json!({}))["error"].is_string());
    }

    #[test]
    fn closing_a_session_stops_it_and_drops_its_watch() {
        let (host, tools) = tools(two());
        tools.run("list_sessions", &json!({}));
        tools.run("watch_session", &json!({"id": "s2"}));
        assert!(tools.watching());
        let closed = tools.run("close_session", &json!({"id": "s2"}));
        assert_eq!(closed["ok"], true);
        assert_eq!(
            host.log.lock().unwrap().last().unwrap(),
            "close opencode:host:b"
        );
        assert!(!tools.watching());
    }

    #[test]
    fn one_request_cannot_read_every_session() {
        let (_, tools) = tools(two());
        tools.run("list_sessions", &json!({}));
        for _ in 0..READS_PER_WINDOW {
            assert!(tools
                .run("read_session", &json!({"id": "s1"}))
                .get("turns")
                .is_some());
        }
        let refused = tools.run("read_session", &json!({"id": "s2"}));
        assert!(refused["error"]
            .as_str()
            .unwrap()
            .contains("enough sessions have been read"));
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

    fn timed(infos: Vec<Info>) -> (Arc<FakeHost>, Tools, Arc<std::sync::atomic::AtomicU64>) {
        let host = FakeHost::with(infos);
        let clock = Arc::new(std::sync::atomic::AtomicU64::new(1_100_000));
        let reader = clock.clone();
        let tools = Tools::with_clock(
            host.clone(),
            Arc::new(move || reader.load(std::sync::atomic::Ordering::SeqCst)),
        );
        (host, tools, clock)
    }

    #[test]
    fn a_watch_right_after_a_message_waits_for_the_work_it_started() {
        let (host, tools, _) = timed(vec![info(
            "opencode:host:a",
            "haiku",
            "f",
            SessionState::Completed,
        )]);
        tools.run("list_sessions", &json!({}));
        tools.run("send_message", &json!({"id": "s1", "text": "one more"}));
        // The dashboard has not seen the session start working yet.
        let watched = tools.run("watch_session", &json!({"id": "s1"}));
        assert_eq!(watched["ok"], true, "{watched}");
        assert!(tools.poll().spoken.is_empty());
        host.infos.lock().unwrap()[0].state = SessionState::Working;
        assert!(tools.poll().spoken.is_empty());
        {
            let mut infos = host.infos.lock().unwrap();
            infos[0].state = SessionState::Completed;
            infos[0].tail = "Wrote it.".into();
        }
        let polled = tools.poll();
        assert_eq!(polled.spoken.len(), 1);
        assert!(polled.spoken[0].contains("Wrote it."));
    }

    #[test]
    fn a_watch_that_never_sees_work_still_ends_after_a_while() {
        let (_, tools, clock) = timed(vec![info(
            "opencode:host:a",
            "haiku",
            "f",
            SessionState::Completed,
        )]);
        tools.run("list_sessions", &json!({}));
        tools.run("send_message", &json!({"id": "s1", "text": "quick"}));
        tools.run("watch_session", &json!({"id": "s1"}));
        assert!(tools.poll().spoken.is_empty());
        clock.fetch_add(NEVER_BUSY_MS + 1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(tools.poll().spoken.len(), 1);
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

//! What the voice is told about sessions: which ones are "recent", how a row
//! reads, and what changed since the last time the model heard about them.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::domain::SessionState;

/// How many sessions `list_sessions` returns by default.
pub const RECENT_BASE: usize = 10;
/// Busy or waiting sessions added to the recent ones, up to this many in all.
pub const RECENT_MAX: usize = 14;
/// A session the person last wrote to longer ago than this is parked, however busy.
const ACTIVE_WINDOW_MS: u64 = 24 * 3_600_000;
const ALL_LIMIT: usize = 40;

/// Everything the voice reads about one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    /// The dashboard's row id, such as `opencode:host:ses_abc`.
    pub id: String,
    pub title: String,
    pub cwd: PathBuf,
    pub state: SessionState,
    pub updated_ms: u64,
    /// Zero when the messages read did not include one of theirs.
    pub last_user_ms: u64,
    /// How the last assistant message ends.
    pub tail: String,
    /// A background command or other work keeping the session busy.
    pub doing: Option<String>,
    pub subagents: usize,
    pub paused: bool,
    /// Not a session another session started.
    pub top_level: bool,
    pub can_message: bool,
    /// Why a session that needs the person needs them, in the provider's words.
    pub why: Option<String>,
}

impl Info {
    /// When the person last worked in this session. The agent's own activity
    /// is no stand-in: a long background run keeps moving it.
    pub fn human_ms(&self) -> u64 {
        if self.last_user_ms > 0 {
            self.last_user_ms
        } else {
            self.updated_ms
        }
    }

    pub fn busy(&self) -> bool {
        self.state == SessionState::Working || self.doing.is_some() || self.subagents > 0
    }

    pub fn needs_you(&self) -> bool {
        self.state == SessionState::NeedsInput
    }

    pub fn folder(&self) -> String {
        self.cwd
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.cwd.display().to_string())
    }

    pub fn state_word(&self) -> &'static str {
        match self.state {
            SessionState::Working => "working",
            SessionState::NeedsInput => "needs_you",
            SessionState::Completed | SessionState::ReadyForReview => "done",
            SessionState::Unknown => "unknown",
        }
    }
}

/// The sessions behind the ones listed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Parked {
    pub count: usize,
    pub needing_you: usize,
}

/// The sessions the person is working in: the most recent by their own last
/// message, plus busy or waiting ones they touched in the last day. Paused
/// sessions and ones another session started are left out.
pub fn recent(infos: &[Info], now_ms: u64) -> (Vec<&Info>, Parked) {
    let mut candidates: Vec<&Info> = infos
        .iter()
        .filter(|info| info.top_level && !info.paused)
        .collect();
    candidates.sort_by(|a, b| b.human_ms().cmp(&a.human_ms()).then(a.id.cmp(&b.id)));
    let mut shown: Vec<&Info> = candidates.iter().take(RECENT_BASE).copied().collect();
    for info in candidates.iter().skip(RECENT_BASE) {
        if shown.len() >= RECENT_MAX {
            break;
        }
        let touched = now_ms.saturating_sub(info.human_ms()) <= ACTIVE_WINDOW_MS;
        if touched && (info.busy() || info.needs_you()) {
            shown.push(info);
        }
    }
    let parked: Vec<&&Info> = candidates
        .iter()
        .filter(|info| !shown.iter().any(|shown| shown.id == info.id))
        .collect();
    let parked = Parked {
        count: parked.len(),
        needing_you: parked.iter().filter(|info| info.needs_you()).count(),
    };
    (shown, parked)
}

/// Every session, newest first, or the ones whose title or folder holds `query`.
pub fn search<'a>(infos: &'a [Info], query: Option<&str>) -> Vec<&'a Info> {
    let query = query
        .map(|query| query.trim().to_lowercase())
        .filter(|query| !query.is_empty());
    let mut found: Vec<&Info> = infos
        .iter()
        .filter(|info| {
            query.as_ref().map_or(true, |query| {
                info.title.to_lowercase().contains(query.as_str())
                    || info
                        .cwd
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(query.as_str())
            })
        })
        .collect();
    found.sort_by(|a, b| b.human_ms().cmp(&a.human_ms()).then(a.id.cmp(&b.id)));
    found.truncate(ALL_LIMIT);
    found
}

/// "just now", "12 min ago", "4 h ago", "2 days ago".
pub fn ago(now_ms: u64, then_ms: u64) -> String {
    if then_ms == 0 {
        return "unknown".into();
    }
    let seconds = now_ms.saturating_sub(then_ms) / 1000;
    match seconds {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", seconds / 60),
        3600..=86_399 => format!("{} h ago", seconds / 3600),
        _ => format!("{} days ago", seconds / 86_400),
    }
}

/// Short names the model uses in tool calls, kept for the whole call.
#[derive(Default)]
pub struct Handles {
    by_id: HashMap<String, String>,
    by_handle: HashMap<String, String>,
}

impl Handles {
    pub fn handle(&mut self, id: &str) -> String {
        if let Some(handle) = self.by_id.get(id) {
            return handle.clone();
        }
        let handle = format!("s{}", self.by_id.len() + 1);
        self.by_id.insert(id.to_owned(), handle.clone());
        self.by_handle.insert(handle.clone(), id.to_owned());
        handle
    }

    pub fn id(&self, handle: &str) -> Option<&str> {
        self.by_handle.get(handle.trim()).map(String::as_str)
    }
}

/// What the model has been told of each session's last message, so a row says
/// it again only when it changed.
#[derive(Default)]
pub struct Seen {
    tails: HashMap<String, String>,
}

impl Seen {
    pub fn row(&mut self, info: &Info, handle: &str, now_ms: u64) -> Value {
        let mut row = json!({
            "id": handle,
            "title": info.title,
            "folder": info.folder(),
            "state": info.state_word(),
            "agent_last_active": ago(now_ms, info.updated_ms),
        });
        if info.last_user_ms > 0 {
            row["you_last_wrote"] = json!(ago(now_ms, info.last_user_ms));
        }
        if let Some(doing) = &info.doing {
            row["doing"] = json!(format!("waiting on a background command: {doing}"));
        }
        if info.subagents > 0 {
            row["sub_agents_running"] = json!(info.subagents);
        }
        if let (true, Some(why)) = (info.needs_you(), &info.why) {
            row["needs_you_because"] = json!(why);
        }
        if !info.can_message {
            row["cannot_message"] = json!(true);
        }
        if !info.tail.is_empty() {
            if self.tails.get(&info.id) == Some(&info.tail) {
                row["last_said"] = json!("(unchanged since you last heard it)");
            } else {
                row["last_said"] = json!(info.tail);
                self.tails.insert(info.id.clone(), info.tail.clone());
            }
        }
        row
    }

    /// The model has been told this tail by some other route.
    pub fn told(&mut self, id: &str, tail: &str) {
        self.tails.insert(id.to_owned(), tail.to_owned());
    }
}

/// A line the model hears when something changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub id: String,
    pub text: String,
}

/// What each session looked like when the model last heard of it.
#[derive(Default)]
pub struct Known {
    seen: HashMap<String, (SessionState, String)>,
}

impl Known {
    /// The model has been told how this session looks now: no change to report.
    pub fn record(&mut self, info: &Info) {
        self.seen
            .insert(info.id.clone(), (info.state, info.tail.clone()));
    }

    /// Records the current state of `infos`. The first look at a session only
    /// records it; after that a changed state, or a changed ending of a
    /// finished session, is a change.
    pub fn changes(&mut self, infos: &[Info]) -> Vec<Change> {
        let mut changes = Vec::new();
        for info in infos {
            let now = (info.state, info.tail.clone());
            match self.seen.get(&info.id) {
                None => {}
                // A session starting work is not news; finishing or needing them is.
                Some((state, _))
                    if *state != info.state
                        && matches!(
                            info.state,
                            SessionState::NeedsInput
                                | SessionState::Completed
                                | SessionState::ReadyForReview
                        ) =>
                {
                    changes.push(Change {
                        id: info.id.clone(),
                        text: describe(info, Some(*state)),
                    });
                }
                Some((_, tail)) if *tail != info.tail && info.state != SessionState::Working => {
                    changes.push(Change {
                        id: info.id.clone(),
                        text: describe(info, None),
                    });
                }
                Some(_) => {}
            }
            self.seen.insert(info.id.clone(), now);
        }
        changes
    }
}

fn describe(info: &Info, before: Option<SessionState>) -> String {
    let said = if info.tail.is_empty() {
        String::new()
    } else {
        format!(" It ends: {}", info.tail)
    };
    let title = &info.title;
    match info.state {
        SessionState::Working => format!("\"{title}\" started working."),
        SessionState::NeedsInput => format!("\"{title}\" now needs the developer.{said}"),
        SessionState::Completed | SessionState::ReadyForReview => {
            if before == Some(SessionState::Working) || before.is_none() {
                format!("\"{title}\" finished.{said}")
            } else {
                format!("\"{title}\" is done.{said}")
            }
        }
        SessionState::Unknown => format!("\"{title}\" changed state."),
    }
}

/// The status notes the talker starts with.
pub fn notes(infos: &[Info], now_ms: u64) -> String {
    let (shown, parked) = recent(infos, now_ms);
    if shown.is_empty() {
        return "There are no recent sessions.".into();
    }
    let mut lines = vec![format!("{} recent sessions:", shown.len())];
    for info in shown {
        lines.push(format!(
            "- \"{}\" ({}): {}{}",
            info.title,
            info.folder(),
            info.state_word(),
            info.doing
                .as_ref()
                .map(|doing| format!(", background command: {doing}"))
                .unwrap_or_default()
        ));
    }
    if parked.count > 0 {
        lines.push(format!(
            "{} older sessions are parked{}.",
            parked.count,
            if parked.needing_you > 0 {
                format!(", {} of them need the developer", parked.needing_you)
            } else {
                String::new()
            }
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3_600_000;
    const NOW: u64 = 1_000 * HOUR;

    fn info(id: &str, state: SessionState, wrote_hours_ago: u64) -> Info {
        Info {
            id: id.into(),
            title: format!("title {id}"),
            cwd: PathBuf::from(format!("/work/{id}")),
            state,
            updated_ms: NOW - wrote_hours_ago * HOUR,
            last_user_ms: NOW - wrote_hours_ago * HOUR,
            tail: String::new(),
            doing: None,
            subagents: 0,
            paused: false,
            top_level: true,
            can_message: true,
            why: None,
        }
    }

    #[test]
    fn recent_takes_the_latest_by_the_persons_own_last_message() {
        let mut infos: Vec<Info> = (0..14)
            .map(|n| info(&format!("a{n:02}"), SessionState::Completed, n))
            .collect();
        // The agent's own activity must not decide: this one was bumped just now.
        infos[13].updated_ms = NOW;
        let (shown, parked) = recent(&infos, NOW);
        assert_eq!(shown.len(), RECENT_BASE);
        assert!(shown.iter().all(|info| info.id != "a13"));
        assert_eq!(
            parked,
            Parked {
                count: 4,
                needing_you: 0
            }
        );
    }

    #[test]
    fn a_waiting_or_busy_session_touched_today_gets_in_but_an_old_one_does_not() {
        let mut infos: Vec<Info> = (0..12)
            .map(|n| info(&format!("a{n:02}"), SessionState::Completed, n))
            .collect();
        infos.push(info("needs", SessionState::NeedsInput, 20));
        infos.push(info("running", SessionState::Working, 30));
        let (shown, parked) = recent(&infos, NOW);
        assert!(shown.iter().any(|info| info.id == "needs"));
        assert!(shown.iter().all(|info| info.id != "running"));
        assert_eq!(parked.count, infos.len() - shown.len());
    }

    #[test]
    fn parked_sessions_that_need_the_person_are_counted() {
        let mut infos: Vec<Info> = (0..10)
            .map(|n| info(&format!("a{n:02}"), SessionState::Completed, n))
            .collect();
        infos.push(info("old", SessionState::NeedsInput, 90));
        let (_, parked) = recent(&infos, NOW);
        assert_eq!(
            parked,
            Parked {
                count: 1,
                needing_you: 1
            }
        );
    }

    #[test]
    fn paused_and_child_sessions_are_left_out() {
        let mut paused = info("paused", SessionState::Working, 0);
        paused.paused = true;
        let mut child = info("child", SessionState::Working, 0);
        child.top_level = false;
        let infos = vec![paused, child, info("keep", SessionState::Completed, 1)];
        let (shown, parked) = recent(&infos, NOW);
        assert_eq!(shown.len(), 1);
        assert_eq!(parked.count, 0);
    }

    #[test]
    fn a_session_without_a_message_of_theirs_falls_back_to_its_activity() {
        let mut quiet = info("quiet", SessionState::Completed, 50);
        quiet.last_user_ms = 0;
        quiet.updated_ms = NOW - HOUR;
        assert_eq!(quiet.human_ms(), NOW - HOUR);
    }

    #[test]
    fn search_matches_title_or_folder_across_everything() {
        let mut infos = vec![
            info("alpha", SessionState::Completed, 99),
            info("beta", SessionState::Completed, 1),
        ];
        infos[0].title = "Semantic search benchmark".into();
        assert_eq!(search(&infos, Some("semantic"))[0].id, "alpha");
        assert_eq!(search(&infos, Some("work/beta"))[0].id, "beta");
        assert_eq!(search(&infos, None).len(), 2);
    }

    #[test]
    fn ages_read_the_way_a_person_says_them() {
        assert_eq!(ago(NOW, NOW - 5_000), "just now");
        assert_eq!(ago(NOW, NOW - 12 * 60_000), "12 min ago");
        assert_eq!(ago(NOW, NOW - 4 * HOUR), "4 h ago");
        assert_eq!(ago(NOW, NOW - 50 * HOUR), "2 days ago");
        assert_eq!(ago(NOW, 0), "unknown");
    }

    #[test]
    fn a_tail_is_repeated_only_when_it_changed() {
        let mut seen = Seen::default();
        let mut one = info("a", SessionState::Completed, 1);
        one.tail = "The tests pass.".into();
        let first = seen.row(&one, "s1", NOW);
        assert_eq!(first["last_said"], "The tests pass.");
        let second = seen.row(&one, "s1", NOW);
        assert_eq!(second["last_said"], "(unchanged since you last heard it)");
        one.tail = "Now rerunning.".into();
        assert_eq!(seen.row(&one, "s1", NOW)["last_said"], "Now rerunning.");
    }

    #[test]
    fn rows_say_what_a_session_is_doing_and_when_it_cannot_be_messaged() {
        let mut one = info("a", SessionState::Working, 1);
        one.doing = Some("CI on PR 8".into());
        one.subagents = 2;
        one.can_message = false;
        let row = Seen::default().row(&one, "s1", NOW);
        assert_eq!(row["doing"], "waiting on a background command: CI on PR 8");
        assert_eq!(row["sub_agents_running"], 2);
        assert_eq!(row["cannot_message"], true);
    }

    #[test]
    fn a_session_needing_the_person_says_why() {
        let mut one = info("a", SessionState::NeedsInput, 1);
        one.why = Some("its last turn failed".into());
        assert_eq!(
            Seen::default().row(&one, "s1", NOW)["needs_you_because"],
            "its last turn failed"
        );
        one.state = SessionState::Completed;
        assert!(Seen::default()
            .row(&one, "s1", NOW)
            .get("needs_you_because")
            .is_none());
    }

    #[test]
    fn handles_are_stable_and_resolve_back() {
        let mut handles = Handles::default();
        let a = handles.handle("opencode:host:ses_a");
        let b = handles.handle("opencode:host:ses_b");
        assert_eq!((a.as_str(), b.as_str()), ("s1", "s2"));
        assert_eq!(handles.handle("opencode:host:ses_a"), "s1");
        assert_eq!(handles.id("s2"), Some("opencode:host:ses_b"));
        assert_eq!(handles.id("s9"), None);
    }

    #[test]
    fn changes_start_after_the_first_look_and_report_finishing_and_questions() {
        let mut known = Known::default();
        let mut one = info("a", SessionState::Working, 1);
        assert!(known.changes(&[one.clone()]).is_empty());
        assert!(known.changes(&[one.clone()]).is_empty());
        one.state = SessionState::Completed;
        one.tail = "Merged.".into();
        let changes = known.changes(&[one.clone()]);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].text.contains("finished") && changes[0].text.contains("Merged."));
        one.state = SessionState::NeedsInput;
        one.tail = "Rerun it?".into();
        assert!(known.changes(&[one])[0]
            .text
            .contains("needs the developer"));
    }

    #[test]
    fn a_session_starting_work_is_not_news() {
        let mut known = Known::default();
        let mut one = info("a", SessionState::Completed, 1);
        known.changes(&[one.clone()]);
        one.state = SessionState::Working;
        assert!(known.changes(&[one]).is_empty());
    }

    #[test]
    fn the_status_notes_fit_a_pushed_update() {
        let infos: Vec<Info> = (0..14)
            .map(|n| info(&format!("a{n:02}"), SessionState::Completed, n))
            .collect();
        let notes = notes(&infos, NOW);
        assert!(notes.starts_with("10 recent sessions:"));
        assert!(notes.contains("4 older sessions are parked"));
        assert!(notes.len() < 1800);
    }
}

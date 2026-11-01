//! Voice Mode's owner: starts and ends calls, relays events between the voice
//! page and GPT-Live, and runs the tools the backend model asks for.

use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::browser::{self, Mode};
use super::call::{self, Action, Call, Line};
use super::config::{self, Settings};
use super::log::{clip, log};
use super::server::{self, PageSink};
use super::tools::{Tools, VoiceHost};

/// Time the voice page gets to connect by itself before a tab is opened.
const PAGE_GRACE: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_millis(2500)
};
/// Time a call may wait for a page, or for OpenAI, before it is given up.
const START_TIMEOUT: Duration = Duration::from_secs(45);
const POLL_EVERY: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Connecting,
    Live,
}

static VIEWING: Mutex<Option<String>> = Mutex::new(None);

/// The session the developer is looking at: selected in the dashboard, or open
/// in its own view. A call in progress is told when it changes.
pub fn set_viewing(id: Option<String>) {
    *VIEWING.lock().unwrap_or_else(|p| p.into_inner()) = id;
}

fn viewing() -> Option<String> {
    VIEWING.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// What the dashboard shows about the call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelStatus {
    pub phase: Phase,
    pub seconds: u64,
    pub lines: Vec<Line>,
    /// Why the last call ended, or something the person should do.
    pub notice: Option<String>,
    /// The notice only says an ordinary end ("You ended the call"): nothing to act on.
    pub quiet: bool,
}

impl PanelStatus {
    pub fn idle() -> Self {
        Self {
            phase: Phase::Idle,
            seconds: 0,
            lines: Vec::new(),
            notice: None,
            quiet: false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Toggle {
    Started,
    Ended,
    NeedKey,
    Failed(String),
}

/// What creating the OpenAI session returns.
pub struct Created {
    pub session_id: String,
    pub sdp: String,
}

struct Page {
    id: u64,
    outgoing: Sender<String>,
    ready: bool,
    /// Started by agentview itself (`?hidden`), not a tab somebody left open.
    hidden: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Waiting for a page to connect.
    Waiting,
    /// The page has the call; waiting for the OpenAI session.
    Negotiating,
    Live,
}

struct ActiveCall {
    stage: Stage,
    since: Instant,
    live_since: Option<Instant>,
    opened_browser: bool,
    /// The hidden browser holding the microphone, when agentview started one.
    browser: Option<std::process::Child>,
    last_heard: Instant,
    last_poll: Instant,
    polls: u32,
    /// What the page last said it was doing, and since when.
    page_stage: Option<(String, Instant)>,
    warned: bool,
    call: Call,
    tools: Arc<Tools>,
}

#[derive(Default)]
struct State {
    server: Option<std::result::Result<u16, String>>,
    /// The page that holds the call's microphone.
    page: Option<Page>,
    /// Pages that connected and have not said hello yet.
    candidates: Vec<Page>,
    pages: u64,
    active: Option<ActiveCall>,
    notice: Option<String>,
    quiet: bool,
}

/// Gets a browser onto the voice page; a hidden one comes back to be stopped later.
type Launcher = Box<dyn Fn(&str) -> Option<std::process::Child> + Send + Sync>;
type KeySource = Box<dyn Fn() -> Option<String> + Send + Sync>;
type Creator = Box<dyn Fn(&str, &Value) -> Result<Created> + Send + Sync>;

struct Inner {
    host: Arc<dyn VoiceHost>,
    settings: Settings,
    port: u16,
    state: Mutex<State>,
    launch: Launcher,
    key: KeySource,
    create: Creator,
}

#[derive(Clone)]
pub struct VoiceService {
    inner: Arc<Inner>,
}

impl VoiceService {
    pub fn new(host: Arc<dyn VoiceHost>) -> Self {
        let settings = Settings::load();
        let launch = launcher(&settings);
        Self::with_parts(
            host,
            settings,
            config::port(),
            launch,
            Box::new(config::api_key),
            Box::new(create_session),
        )
    }

    pub(crate) fn with_parts(
        host: Arc<dyn VoiceHost>,
        settings: Settings,
        port: u16,
        launch: Launcher,
        key: KeySource,
        create: Creator,
    ) -> Self {
        let service = Self {
            inner: Arc::new(Inner {
                host,
                settings,
                port,
                state: Mutex::default(),
                launch,
                key,
                create,
            }),
        };
        let weak = Arc::downgrade(&service.inner);
        thread::spawn(move || {
            while let Some(inner) = weak.upgrade() {
                inner.tick();
                drop(inner);
                thread::sleep(Duration::from_millis(500));
            }
        });
        service
    }

    pub fn has_key(&self) -> bool {
        (self.inner.key)().is_some()
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.inner.port)
    }

    /// Starts a call, or ends the one in progress.
    pub fn toggle(&self) -> Toggle {
        if self.inner.lock().active.is_some() {
            self.inner.finish_quietly("You ended the call.", true);
            return Toggle::Ended;
        }
        if (self.inner.key)().is_none() {
            return Toggle::NeedKey;
        }
        match self.inner.start() {
            Ok(()) => Toggle::Started,
            Err(error) => Toggle::Failed(format!("{error:#}")),
        }
    }

    pub fn status(&self) -> PanelStatus {
        let state = self.inner.lock();
        match &state.active {
            None => PanelStatus {
                notice: state.notice.clone(),
                quiet: state.quiet,
                ..PanelStatus::idle()
            },
            Some(active) => PanelStatus {
                phase: if active.stage == Stage::Live {
                    Phase::Live
                } else {
                    Phase::Connecting
                },
                seconds: active
                    .live_since
                    .map_or(0, |since| since.elapsed().as_secs()),
                lines: active.call.lines(),
                notice: state.notice.clone(),
                quiet: state.quiet,
            },
        }
    }

    /// Ends any call when the dashboard closes.
    pub fn shutdown(&self) {
        // The browser stops now, not after the usual moment: this process is ending.
        let browser = self
            .inner
            .lock()
            .active
            .as_mut()
            .and_then(|active| active.browser.take());
        if self.inner.lock().active.is_some() {
            self.inner.finish("agentview closed.", true);
        }
        if let Some(child) = browser {
            browser::stop(child);
        }
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn send_page(state: &State, message: Value) {
        if message["type"] == "send" {
            let event = &message["event"];
            let kind = event["type"].as_str().unwrap_or_default();
            // Answers to tool calls are logged with the tool; the rest says what the voice was told.
            if !kind.starts_with("response.") {
                log(
                    "send",
                    json!({"type": kind, "content": clip(event["content"].as_str().unwrap_or_default(), 300)}),
                );
            }
        }
        if let Some(page) = &state.page {
            let _ = page.outgoing.send(message.to_string());
        }
    }

    fn start(self: &Arc<Self>) -> Result<()> {
        let mut state = self.lock();
        if state.server.is_none() {
            let sink: Arc<dyn PageSink> = Arc::new(Sink(self.clone()));
            state.server = Some(server::serve(self.port, sink).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AddrInUse {
                    format!(
                        "port {} is in use; is voice already running in another agentview?",
                        self.port
                    )
                } else {
                    format!("could not start the voice server: {error}")
                }
            }));
        }
        if let Some(Err(error)) = &state.server {
            let error = error.clone();
            state.server = None;
            bail!(error);
        }
        let tools = Arc::new(Tools::new(self.host.clone()));
        tools.set_viewing(viewing());
        tools.prime();
        let now = Instant::now();
        state.notice = None;
        state.quiet = false;
        state.active = Some(ActiveCall {
            stage: Stage::Waiting,
            since: now,
            live_since: None,
            opened_browser: false,
            browser: None,
            last_heard: now,
            last_poll: now,
            polls: 0,
            page_stage: None,
            warned: false,
            call: Call::default(),
            tools,
        });
        config::mark_hint_seen();
        super::log::start_call();
        log("start", json!({"browser": self.settings.browser}));
        // A hidden browser starts at once. A tab in your own browser is only
        // opened after a pinned one has had a moment to connect.
        if state.page.is_none() && Mode::parse(&self.settings.browser) == Mode::Hidden {
            let url = format!("http://127.0.0.1:{}/", self.port);
            if let Some(active) = state.active.as_mut() {
                active.opened_browser = true;
                active.browser = (self.launch)(&url);
            }
        }
        if let Some(page) = &state.page {
            if !page.ready {
                state.notice = Some(MIC_NOTICE.into());
            }
            Self::begin_negotiation(&mut state);
        }
        Ok(())
    }

    fn begin_negotiation(state: &mut State) {
        if let Some(active) = state
            .active
            .as_mut()
            .filter(|active| active.stage == Stage::Waiting)
        {
            active.stage = Stage::Negotiating;
            active.since = Instant::now();
        } else {
            return;
        }
        Self::send_page(state, json!({"type": "start"}));
    }

    /// Ends the call: tells OpenAI to close, then the page to hang up.
    /// An ordinary end the person asked for: it is noted, not announced.
    fn finish_quietly(&self, reason: &str, notify_page: bool) {
        self.finish(reason, notify_page);
        self.lock().quiet = true;
    }

    fn finish(&self, reason: &str, notify_page: bool) {
        let mut state = self.lock();
        let Some(mut active) = state.active.take() else {
            return;
        };
        state.quiet = false;
        state.notice = Some(reason.to_owned());
        log("end", json!({"reason": reason}));
        if let Some(child) = active.browser.take() {
            // Give OpenAI's close a moment to go out before the browser stops.
            let wait = if notify_page { 1500 } else { 0 };
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(wait));
                browser::stop(child);
            });
        }
        let seconds = active.call.usage_seconds;
        if notify_page {
            Self::send_page(&state, json!({"type": "send", "event": call::close()}));
            if let Some(page) = &state.page {
                let outgoing = page.outgoing.clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(1200));
                    let _ = outgoing.send(json!({"type": "end"}).to_string());
                });
            }
        }
        if seconds > 0 {
            state.notice = Some(format!(
                "{reason} {}:{:02} billed.",
                seconds / 60,
                seconds % 60
            ));
        }
    }

    fn tick(&self) {
        let mut state = self.lock();
        let Some(active) = state.active.as_mut() else {
            return;
        };
        if let Some(child) = active.browser.as_mut() {
            if matches!(child.try_wait(), Ok(Some(_))) {
                active.browser = None;
                drop(state);
                self.finish("The voice browser closed unexpectedly.", false);
                return;
            }
        }
        let idle_limit = Duration::from_secs(self.settings.idle_minutes * 60);
        let max = Duration::from_secs(self.settings.max_minutes * 60);
        match active.stage {
            Stage::Waiting => {
                if active.since.elapsed() >= PAGE_GRACE && !active.opened_browser {
                    active.opened_browser = true;
                    active.browser = (self.launch)(&format!("http://127.0.0.1:{}/", self.port));
                }
                if active.since.elapsed() >= START_TIMEOUT {
                    let url = format!("http://127.0.0.1:{}/", self.port);
                    drop(state);
                    self.finish(
                        &format!("No voice tab connected. Open {url} in your browser."),
                        false,
                    );
                }
            }
            Stage::Negotiating => {
                let waiting_for_mic = active
                    .page_stage
                    .as_ref()
                    .filter(|(stage, _)| stage == "microphone")
                    .map(|(_, since)| since.elapsed());
                let timed_out = active.since.elapsed() >= START_TIMEOUT;
                let no_stage = active.page_stage.is_none();
                if waiting_for_mic.is_some_and(|waited| waited >= MIC_PROMPT_WAIT) {
                    state.notice = Some(MIC_WAIT_NOTICE.into());
                }
                if timed_out {
                    let reason = if waiting_for_mic.is_some() {
                        "The microphone was never allowed. On a Mac, allow your terminal or the browser under System Settings, Privacy & Security, Microphone, then try again."
                    } else if no_stage {
                        "The voice page never started the call."
                    } else {
                        "OpenAI did not answer the call."
                    };
                    drop(state);
                    self.finish(reason, true);
                }
            }
            Stage::Live => {
                // A goodbye ends the call once the voice has answered it.
                if active.call.farewell
                    && Instant::now().duration_since(active.last_heard) >= Duration::from_secs(5)
                {
                    drop(state);
                    self.finish_quietly("Ended at your request.", true);
                    return;
                }
                let tools = active.tools.clone();
                let now = Instant::now();
                let elapsed = active
                    .live_since
                    .map_or(Duration::ZERO, |since| since.elapsed());
                if elapsed >= max {
                    drop(state);
                    self.finish(
                        &format!("Reached the {} minute limit.", self.settings.max_minutes),
                        true,
                    );
                    return;
                }
                if max.saturating_sub(elapsed) <= Duration::from_secs(300) && !active.warned {
                    active.warned = true;
                    Self::send_page(
                        &state,
                        json!({"type": "send", "event": call::instruction("Tell the developer, in one short sentence, that this call ends in about five minutes.")}),
                    );
                    return;
                }
                if now.duration_since(active.last_heard) >= idle_limit && !tools.watching() {
                    drop(state);
                    self.finish(
                        &format!(
                            "Ended after {} minutes of silence.",
                            self.settings.idle_minutes
                        ),
                        true,
                    );
                    return;
                }
                if now.duration_since(active.last_poll) >= POLL_EVERY {
                    active.last_poll = now;
                    active.polls += 1;
                    let refresh = active.polls % 4 == 0;
                    drop(state);
                    // The host and the tools can take a moment: not under the lock.
                    if refresh {
                        // The dashboard may be showing a session in its own view
                        // and not refreshing: ask for fresh data.
                        self.host.refresh();
                    }
                    let polled = tools.poll();
                    let looking = tools.set_viewing(viewing());
                    let state = self.lock();
                    if let Some(note) = looking {
                        Self::send_page(
                            &state,
                            json!({"type": "send", "event": call::thinking(&note)}),
                        );
                    }
                    for line in &polled.spoken {
                        Self::send_page(
                            &state,
                            json!({"type": "send", "event": call::commentary(line)}),
                        );
                    }
                    if !polled.quiet.is_empty() {
                        let note = format!(
                            "Background for when they ask what is going on; do not bring it up unprompted: {}",
                            polled.quiet.join(" ")
                        );
                        Self::send_page(
                            &state,
                            json!({"type": "send", "event": call::thinking(&note)}),
                        );
                    }
                    if !polled.spoken.is_empty() {
                        drop(state);
                        if let Some(active) = self.lock().active.as_mut() {
                            active.last_heard = Instant::now();
                        }
                    }
                }
            }
        }
    }

    fn on_offer(self: &Arc<Self>, sdp: String) {
        let tools = {
            let state = self.lock();
            let Some(active) = state
                .active
                .as_ref()
                .filter(|active| active.stage == Stage::Negotiating)
            else {
                return;
            };
            active.tools.clone()
        };
        let this = self.clone();
        thread::spawn(move || {
            let notes = tools.notes();
            let result = (this.key)()
                .context("there is no OpenAI key")
                .and_then(|key| {
                    (this.create)(&key, &call::session_request(&this.settings, &notes, &sdp))
                });
            match result {
                Ok(created) => {
                    let mut state = this.lock();
                    if let Some(active) = state.active.as_mut() {
                        active.call.session_id = Some(created.session_id);
                        Self::send_page(&state, json!({"type": "answer", "sdp": created.sdp}));
                    }
                }
                Err(error) => this.finish(&format!("Could not start the call: {error:#}"), true),
            }
        });
    }

    fn on_event(self: &Arc<Self>, event: &Value) {
        let actions = {
            let mut state = self.lock();
            let Some(active) = state.active.as_mut() else {
                return;
            };
            let actions = active.call.on_event(event);
            if active.call.heard {
                active.call.heard = false;
                active.last_heard = Instant::now();
            }
            actions
        };
        for action in actions {
            match action {
                Action::Started => {
                    let mut state = self.lock();
                    if let Some(active) = state.active.as_mut() {
                        active.stage = Stage::Live;
                        active.live_since.get_or_insert_with(Instant::now);
                        active.last_heard = Instant::now();
                    }
                    state.notice = None;
                }
                Action::Closed => self.finish_quietly("The call ended.", false),
                Action::Failed(message) => {
                    self.lock().notice = Some(message);
                }
                Action::Run { calls, stale } => {
                    let (tools, outgoing) = {
                        let state = self.lock();
                        let Some(active) = state.active.as_ref() else {
                            return;
                        };
                        (
                            active.tools.clone(),
                            state.page.as_ref().map(|page| page.outgoing.clone()),
                        )
                    };
                    let Some(outgoing) = outgoing else { return };
                    let this = self.clone();
                    thread::spawn(move || {
                        let send = |event: Value| {
                            let _ =
                                outgoing.send(json!({"type": "send", "event": event}).to_string());
                        };
                        for call_ in &calls {
                            let args: Value =
                                serde_json::from_str(&call_.arguments).unwrap_or(Value::Null);
                            let started = Instant::now();
                            let result = if stale && Tools::is_read_only(&call_.name) {
                                // Reading on for a request the developer has moved past.
                                json!({"error": "the developer has asked something new: stop looking things up and answer in one short sentence"})
                            } else {
                                tools.run(&call_.name, &args)
                            };
                            log(
                                "tool",
                                json!({
                                    "name": call_.name,
                                    "args": args,
                                    "ms": started.elapsed().as_millis() as u64,
                                    "result": clip(&result.to_string(), 2000),
                                }),
                            );
                            send(call::function_output(&call_.call_id, &result));
                        }
                        send(call::continue_response());
                        if calls.iter().any(|call| call.name == "end_call") {
                            // Long enough for the goodbye to be said.
                            thread::sleep(Duration::from_secs(6));
                            this.finish_quietly("Ended at your request.", true);
                        }
                    });
                }
            }
        }
    }
}

/// What the server calls into.
struct Sink(Arc<Inner>);

impl PageSink for Sink {
    fn connected(&self, outgoing: Sender<String>) -> u64 {
        let mut state = self.0.lock();
        state.pages += 1;
        let id = state.pages;
        // It only counts once it says hello and what it is: a tab left open
        // from an earlier version must not take over a call.
        state.candidates.push(Page {
            id,
            outgoing,
            ready: false,
            hidden: false,
        });
        id
    }

    fn message(&self, page: u64, message: Value) {
        let this = &self.0;
        match message.get("type").and_then(Value::as_str) {
            Some("event") => {
                let event = &message["event"];
                let kind = event
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let inner = event
                    .pointer("/event/type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let noisy = kind.ends_with("transcript.delta") || inner.ends_with(".delta");
                if kind.ends_with("transcript.delta") {
                    log("said", json!({"who": kind, "text": event["delta"]}));
                } else if !noisy {
                    log(
                        "event",
                        json!({"type": kind, "inner": inner, "error": event.get("error")}),
                    );
                }
            }
            other => log(
                "page",
                json!({"type": other, "message": clip(&message.to_string(), 200)}),
            ),
        }
        let kind = message
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        // Only the page that holds the call is heard after hello.
        if kind != "hello" && this.lock().page.as_ref().map(|p| p.id) != Some(page) {
            return;
        }
        match kind {
            "hello" => {
                let mut state = this.lock();
                let hidden = message
                    .get("hidden")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let ready = message
                    .get("ready")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if let Some(index) = state.candidates.iter().position(|p| p.id == page) {
                    let mut candidate = state.candidates.remove(index);
                    candidate.hidden = hidden;
                    candidate.ready = ready;
                    if !hidden && Mode::parse(&this.settings.browser) == Mode::Hidden {
                        // agentview runs its own browser now: a tab left open is told to go.
                        let _ = candidate
                            .outgoing
                            .send(json!({"type": "retire"}).to_string());
                        log(
                            "page",
                            json!({"type": "retired a visible page", "id": page}),
                        );
                        return;
                    }
                    // A newer page replaces the older: dropping its sender closes it.
                    state.page = Some(candidate);
                } else if state.page.as_ref().map(|p| p.id) != Some(page) {
                    return;
                } else if let Some(p) = state.page.as_mut() {
                    p.ready = ready;
                }
                if ready && state.notice.as_deref() == Some(MIC_NOTICE) {
                    state.notice = None;
                } else if !ready && state.active.is_some() {
                    state.notice = Some(MIC_NOTICE.into());
                }
                Inner::begin_negotiation(&mut state);
            }
            "offer" => {
                if let Some(sdp) = message.get("sdp").and_then(Value::as_str) {
                    this.on_offer(sdp.to_owned());
                }
            }
            "event" => {
                if let Some(event) = message.get("event") {
                    this.on_event(event);
                }
            }
            "stage" => {
                if let Some(active) = this.lock().active.as_mut() {
                    let stage = message
                        .get("stage")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    active.page_stage = Some((stage, Instant::now()));
                }
            }
            "mic" => {
                let flat = message
                    .get("flat_seconds")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let mut state = this.lock();
                let silent = state
                    .active
                    .as_ref()
                    .is_some_and(|active| active.stage == Stage::Live && !active.call.user_spoke);
                if silent && flat >= DEAD_MIC_SECONDS {
                    state.notice = Some(DEAD_MIC_NOTICE.into());
                } else if flat < 1.0 && state.notice.as_deref() == Some(DEAD_MIC_NOTICE) {
                    state.notice = None;
                }
            }
            "audio" => {
                let mut state = this.lock();
                let blocked = message.get("on").and_then(Value::as_bool) == Some(false);
                if blocked && state.active.is_some() {
                    state.notice = Some(CLICK_NOTICE.into());
                } else if state.notice.as_deref() == Some(CLICK_NOTICE) {
                    state.notice = None;
                }
            }
            "ended" => {
                let reason = message
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("The call ended.");
                this.finish(&sentence(reason), false);
            }
            _ => {}
        }
    }

    fn closed(&self, page: u64) {
        let mut state = self.0.lock();
        state.candidates.retain(|p| p.id != page);
        if state.page.as_ref().map(|p| p.id) == Some(page) {
            state.page = None;
        }
    }
}

/// How long the page may wait for the microphone before the person is told why.
const MIC_PROMPT_WAIT: Duration = Duration::from_secs(6);
const MIC_WAIT_NOTICE: &str = "Waiting for the microphone: macOS asks once. Allow it for your terminal (or the browser) in the dialog or in System Settings, Privacy & Security, Microphone.";
const MIC_NOTICE: &str = "Allow the microphone in the Agent View voice tab.";
/// A microphone that is exactly silent this long, with nothing heard yet, is not on.
const DEAD_MIC_SECONDS: f64 = 20.0;
const DEAD_MIC_NOTICE: &str = "No sound from the microphone. On a Mac, allow your terminal or the browser under System Settings, Privacy & Security, Microphone.";
const CLICK_NOTICE: &str = "Click the Agent View voice tab once so the browser lets it play sound.";

fn sentence(reason: &str) -> String {
    let mut chars = reason.chars();
    let first = chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default();
    format!("{first}{}.", chars.as_str().trim_end_matches('.'))
}

/// How a browser gets onto the page, from the settings.
fn launcher(settings: &Settings) -> Launcher {
    let mode = Mode::parse(&settings.browser);
    let configured = settings.chrome_path.clone();
    Box::new(move |url| {
        if std::env::var_os("AGENTVIEW_VOICE_NO_BROWSER").is_some() {
            return None;
        }
        match mode {
            Mode::None => None,
            Mode::Tab => {
                open_in_browser(url);
                None
            }
            Mode::Hidden => {
                let started = browser::find_chrome(configured.as_deref()).and_then(|chrome| {
                    let profile = config::browser_profile_dir()?;
                    browser::stop_orphans(&profile);
                    let extra = std::env::var("AGENTVIEW_VOICE_CHROME_ARGS").unwrap_or_default();
                    browser::spawn_hidden(&chrome, &format!("{url}?hidden=1"), &profile, &extra)
                        .ok()
                });
                if started.is_none() {
                    // No Chromium-based browser to hide: use a tab instead.
                    open_in_browser(url);
                }
                started
            }
        }
    })
}

/// Opens `url` in the default browser.
pub fn open_in_browser(url: &str) {
    // Tests and headless runs bring their own page.
    if std::env::var_os("AGENTVIEW_VOICE_NO_BROWSER").is_some() {
        return;
    }
    let result = if cfg!(target_os = "macos") {
        Command::new("open")
            .arg(url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        Command::new("xdg-open")
            .arg(url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    };
    let _ = result;
}

/// `POST /v1/live/sessions` through curl. The key goes over stdin, so it never
/// shows in `ps`; the body, which holds no secret, goes through a private file.
pub fn create_session(key: &str, body: &Value) -> Result<Created> {
    use std::io::Write;
    let directory = std::env::temp_dir();
    let path = directory.join(format!(
        "agentview-voice-{}-{}.json",
        std::process::id(),
        super::tools::now_ms()
    ));
    {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&path)?
            .write_all(body.to_string().as_bytes())?;
    }
    let result = (|| {
        let mut child = Command::new("curl")
            .args([
                "-sS",
                "--max-time",
                "30",
                "-X",
                "POST",
                "-H",
                "@-",
                "-H",
                "Content-Type: application/json",
                "--data-binary",
            ])
            .arg(format!("@{}", path.display()))
            .arg("-w")
            .arg("\n%{http_code}")
            .arg("https://api.openai.com/v1/live/sessions")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("curl is needed for Voice Mode")?;
        writeln!(
            child.stdin.take().context("no stdin")?,
            "Authorization: Bearer {key}"
        )?;
        let output = child.wait_with_output()?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let (body, status) = text.rsplit_once('\n').unwrap_or((&text, ""));
        if status.trim() != "201" && status.trim() != "200" {
            let detail = serde_json::from_str::<Value>(body)
                .ok()
                .and_then(|value| {
                    value
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| String::from_utf8_lossy(&output.stderr).trim().to_owned());
            bail!("OpenAI answered {} {detail}", status.trim());
        }
        parse_created(body)
    })();
    let _ = std::fs::remove_file(&path);
    result
}

fn parse_created(body: &str) -> Result<Created> {
    let value: Value = serde_json::from_str(body).context("OpenAI's answer was not JSON")?;
    Ok(Created {
        session_id: value
            .pointer("/session/id")
            .and_then(Value::as_str)
            .context("no session id")?
            .to_owned(),
        sdp: value
            .pointer("/transport/sdp")
            .and_then(Value::as_str)
            .context("no SDP answer")?
            .to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SessionState;
    use crate::voice::tools::fake::{info, FakeHost};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver};

    struct Rig {
        service: VoiceService,
        host: Arc<FakeHost>,
        opened: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<Value>>>,
    }

    fn rig(settings: Settings, key: Option<&'static str>) -> Rig {
        let host = FakeHost::with(vec![
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
        ]);
        let opened = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (counter, seen) = (opened.clone(), requests.clone());
        let service = VoiceService::with_parts(
            host.clone(),
            settings,
            0,
            Box::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                None
            }),
            Box::new(move || key.map(str::to_owned)),
            Box::new(move |key, body| {
                assert_eq!(key, "sk-test");
                seen.lock().unwrap().push(body.clone());
                Ok(Created {
                    session_id: "live_1".into(),
                    sdp: "v=0 answer".into(),
                })
            }),
        );
        Rig {
            service,
            host,
            opened,
            requests,
        }
    }

    fn page(rig: &Rig) -> (u64, Receiver<String>) {
        let (outgoing, inbox) = mpsc::channel();
        let sink = Sink(rig.service.inner.clone());
        (sink.connected(outgoing), inbox)
    }

    fn say(rig: &Rig, page: u64, message: Value) {
        Sink(rig.service.inner.clone()).message(page, message);
    }

    fn next(inbox: &Receiver<String>) -> Value {
        serde_json::from_str(
            &inbox
                .recv_timeout(Duration::from_secs(3))
                .expect("a message for the page"),
        )
        .unwrap()
    }

    fn until(mut probe: impl FnMut() -> bool) {
        for _ in 0..200 {
            if probe() {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out");
    }

    #[test]
    fn without_a_key_the_first_press_asks_for_one() {
        let rig = rig(Settings::default(), None);
        assert_eq!(rig.service.toggle(), Toggle::NeedKey);
        assert_eq!(rig.service.status().phase, Phase::Idle);
    }

    #[test]
    fn a_call_runs_from_page_to_tools_to_the_end() {
        let rig = rig(Settings::default(), Some("sk-test"));
        let (id, inbox) = page(&rig);
        say(
            &rig,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        assert_eq!(rig.service.toggle(), Toggle::Started);
        assert_eq!(rig.service.status().phase, Phase::Connecting);
        // The page was already there, so it is asked to start at once.
        assert_eq!(next(&inbox)["type"], "start");

        say(&rig, id, json!({"type": "offer", "sdp": "v=0 offer"}));
        let answer = next(&inbox);
        assert_eq!(answer, json!({"type": "answer", "sdp": "v=0 answer"}));
        let request = rig.requests.lock().unwrap()[0].clone();
        assert_eq!(request["transport"]["sdp"], "v=0 offer");
        assert!(request["session"]["instructions"]
            .as_str()
            .unwrap()
            .contains("2 recent sessions"));

        say(
            &rig,
            id,
            json!({"type": "event", "event": {"type": "session.started", "session": {"id": "live_1"}}}),
        );
        assert_eq!(rig.service.status().phase, Phase::Live);

        // The backend asks for the list; the answer and a continue go back.
        let delegation = "item_1";
        say(
            &rig,
            id,
            json!({"type": "event", "event": {"type": "response.event", "delegation_id": delegation,
            "event": {"type": "response.output_item.done", "item": {"type": "function_call", "call_id": "c1", "name": "list_sessions", "arguments": "{}"}}}}),
        );
        say(
            &rig,
            id,
            json!({"type": "event", "event": {"type": "response.event", "delegation_id": delegation, "event": {"type": "response.completed"}}}),
        );
        let output = next(&inbox);
        assert_eq!(output["type"], "send");
        assert_eq!(output["event"]["type"], "response.item.create");
        assert_eq!(output["event"]["item"]["call_id"], "c1");
        let listed: Value =
            serde_json::from_str(output["event"]["item"]["output"].as_str().unwrap()).unwrap();
        assert_eq!(listed["sessions"][0]["title"], "semantic search");
        assert_eq!(next(&inbox)["event"]["type"], "response.create");

        say(
            &rig,
            id,
            json!({"type": "event", "event": {"type": "session.input_transcript.delta", "delta": "Which need me"}}),
        );
        assert_eq!(rig.service.status().lines.len(), 1);

        assert_eq!(rig.service.toggle(), Toggle::Ended);
        assert_eq!(next(&inbox)["event"]["type"], "session.close");
        assert_eq!(next(&inbox)["type"], "end");
        assert_eq!(rig.service.status().phase, Phase::Idle);
    }

    #[test]
    fn a_page_that_connects_late_is_started_and_one_is_opened_when_none_comes() {
        let rig = rig(Settings::default(), Some("sk-test"));
        assert_eq!(rig.service.toggle(), Toggle::Started);
        until(|| rig.opened.load(Ordering::SeqCst) == 1);
        let (id, inbox) = page(&rig);
        say(
            &rig,
            id,
            json!({"type": "hello", "ready": false, "hidden": true}),
        );
        assert_eq!(next(&inbox)["type"], "start");
        assert!(rig.service.status().notice.unwrap().contains("microphone"));
        say(
            &rig,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        assert_eq!(rig.service.status().notice, None);
    }

    #[test]
    fn a_failed_session_creation_ends_the_call_with_the_reason() {
        let host = FakeHost::with(Vec::new());
        let service = VoiceService::with_parts(
            host,
            Settings::default(),
            0,
            Box::new(|_| None),
            Box::new(|| Some("sk-test".into())),
            Box::new(|_, _| bail!("OpenAI answered 401 bad key")),
        );
        let (outgoing, inbox) = mpsc::channel();
        let sink = Sink(service.inner.clone());
        let id = sink.connected(outgoing);
        sink.message(id, json!({"type": "hello", "ready": true, "hidden": true}));
        assert_eq!(service.toggle(), Toggle::Started);
        assert_eq!(next(&inbox)["type"], "start");
        sink.message(id, json!({"type": "offer", "sdp": "x"}));
        until(|| service.status().phase == Phase::Idle);
        assert!(service.status().notice.unwrap().contains("401 bad key"));
    }

    #[test]
    fn a_quiet_call_ends_itself_unless_something_is_watched() {
        let settings = Settings {
            idle_minutes: 0,
            ..Settings::default()
        };
        let rig = rig(settings, Some("sk-test"));
        let (id, inbox) = page(&rig);
        say(
            &rig,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        rig.service.toggle();
        say(&rig, id, json!({"type": "offer", "sdp": "x"}));
        until(|| next_is(&inbox, "answer"));
        say(
            &rig,
            id,
            json!({"type": "event", "event": {"type": "session.started", "session": {"id": "l"}}}),
        );
        until(|| rig.service.status().phase == Phase::Idle);
        assert!(rig.service.status().notice.unwrap().contains("of silence"));
    }

    fn next_is(inbox: &Receiver<String>, kind: &str) -> bool {
        inbox.try_iter().any(|message| {
            serde_json::from_str::<Value>(&message).is_ok_and(|value| value["type"] == kind)
        })
    }

    #[test]
    fn finishing_a_watched_session_is_said_aloud() {
        let rig = rig(Settings::default(), Some("sk-test"));
        let (id, inbox) = page(&rig);
        say(
            &rig,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        rig.service.toggle();
        say(&rig, id, json!({"type": "offer", "sdp": "x"}));
        say(
            &rig,
            id,
            json!({"type": "event", "event": {"type": "session.started", "session": {"id": "l"}}}),
        );
        // List, then watch the working session, through the backend's own calls.
        for (call_id, name, args) in [
            ("c1", "list_sessions", "{}"),
            ("c2", "watch_session", r#"{"id":"s2"}"#),
        ] {
            say(
                &rig,
                id,
                json!({"type": "event", "event": {"type": "response.event", "delegation_id": call_id,
                "event": {"type": "response.output_item.done", "item": {"type": "function_call", "call_id": call_id, "name": name, "arguments": args}}}}),
            );
            say(
                &rig,
                id,
                json!({"type": "event", "event": {"type": "response.event", "delegation_id": call_id, "event": {"type": "response.completed"}}}),
            );
            thread::sleep(Duration::from_millis(200));
        }
        {
            let mut infos = rig.host.infos.lock().unwrap();
            infos[1].state = SessionState::Completed;
            infos[1].tail = "Merged.".into();
        }
        let spoken = (0..40).find_map(|_| {
            thread::sleep(Duration::from_millis(100));
            inbox.try_iter().find_map(|message| {
                let value: Value = serde_json::from_str(&message).ok()?;
                (value["event"]["type"] == "session.commentary.append").then_some(value)
            })
        });
        let spoken = spoken.expect("the finished session is announced");
        assert!(spoken["event"]["content"]
            .as_str()
            .unwrap()
            .contains("Merged."));
    }

    #[cfg(unix)]
    #[test]
    fn a_hidden_browser_starts_with_the_call_and_stops_with_it() {
        let started = Arc::new(AtomicUsize::new(0));
        let counter = started.clone();
        let host = FakeHost::with(Vec::new());
        let service = VoiceService::with_parts(
            host,
            Settings::default(),
            0,
            Box::new(move |url| {
                assert!(url.starts_with("http://127.0.0.1:"));
                counter.fetch_add(1, Ordering::SeqCst);
                std::process::Command::new("sleep").arg("30").spawn().ok()
            }),
            Box::new(|| Some("sk-test".into())),
            Box::new(|_, _| bail!("unused")),
        );
        assert_eq!(service.toggle(), Toggle::Started);
        assert_eq!(started.load(Ordering::SeqCst), 1);
        let pid = service
            .inner
            .lock()
            .active
            .as_ref()
            .unwrap()
            .browser
            .as_ref()
            .unwrap()
            .id();
        assert_eq!(service.toggle(), Toggle::Ended);
        until(|| unsafe { libc::kill(pid as libc::pid_t, 0) } != 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_hidden_browser_that_dies_ends_the_call() {
        let host = FakeHost::with(Vec::new());
        let service = VoiceService::with_parts(
            host,
            Settings::default(),
            0,
            Box::new(|_| std::process::Command::new("true").spawn().ok()),
            Box::new(|| Some("sk-test".into())),
            Box::new(|_, _| bail!("unused")),
        );
        assert_eq!(service.toggle(), Toggle::Started);
        until(|| service.status().phase == Phase::Idle);
        assert!(service
            .status()
            .notice
            .unwrap()
            .contains("voice browser closed"));
    }

    #[test]
    fn a_microphone_that_stays_dead_is_reported_until_it_makes_a_sound() {
        let rig = rig(Settings::default(), Some("sk-test"));
        let (id, _inbox) = page(&rig);
        say(
            &rig,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        rig.service.toggle();
        say(&rig, id, json!({"type": "offer", "sdp": "x"}));
        say(
            &rig,
            id,
            json!({"type": "event", "event": {"type": "session.started", "session": {"id": "l"}}}),
        );
        until(|| rig.service.status().phase == Phase::Live);
        say(
            &rig,
            id,
            json!({"type": "mic", "rms": 0.0, "flat_seconds": 10.0}),
        );
        assert_eq!(rig.service.status().notice, None);
        say(
            &rig,
            id,
            json!({"type": "mic", "rms": 0.0, "flat_seconds": 20.0}),
        );
        assert!(rig
            .service
            .status()
            .notice
            .unwrap()
            .contains("No sound from the microphone"));
        say(
            &rig,
            id,
            json!({"type": "mic", "rms": 0.02, "flat_seconds": 0.0}),
        );
        assert_eq!(rig.service.status().notice, None);
    }

    #[test]
    fn a_tab_left_open_is_retired_and_cannot_take_over_a_hidden_call() {
        let rig = rig(Settings::default(), Some("sk-test"));
        let (tab, tab_inbox) = page(&rig);
        say(&rig, tab, json!({"type": "hello", "ready": true}));
        assert_eq!(next(&tab_inbox)["type"], "retire");
        assert_eq!(rig.service.toggle(), Toggle::Started);
        // Nothing is sent to the tab, and what it says is not heard.
        say(&rig, tab, json!({"type": "offer", "sdp": "stale"}));
        assert!(rig.requests.lock().unwrap().is_empty());
        let (hidden, inbox) = page(&rig);
        say(
            &rig,
            hidden,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        assert_eq!(next(&inbox)["type"], "start");
    }

    #[test]
    fn in_tab_mode_a_visible_page_is_used() {
        let settings = Settings {
            browser: "tab".into(),
            ..Settings::default()
        };
        let rig = rig(settings, Some("sk-test"));
        let (tab, inbox) = page(&rig);
        say(&rig, tab, json!({"type": "hello", "ready": true}));
        assert_eq!(rig.service.toggle(), Toggle::Started);
        assert_eq!(next(&inbox)["type"], "start");
    }

    #[test]
    fn a_page_stuck_waiting_for_the_microphone_is_explained_and_then_given_up_on() {
        let rig = rig(Settings::default(), Some("sk-test"));
        let (id, _inbox) = page(&rig);
        say(
            &rig,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        rig.service.toggle();
        say(&rig, id, json!({"type": "stage", "stage": "microphone"}));
        rig.service.inner.lock().active.as_mut().unwrap().page_stage =
            Some(("microphone".into(), Instant::now() - Duration::from_secs(8)));
        until(|| {
            rig.service
                .status()
                .notice
                .as_deref()
                .is_some_and(|n| n.contains("Waiting for the microphone"))
        });
        {
            let mut state = rig.service.inner.lock();
            let active = state.active.as_mut().unwrap();
            active.since = Instant::now() - START_TIMEOUT;
        }
        until(|| rig.service.status().phase == Phase::Idle);
        assert!(rig
            .service
            .status()
            .notice
            .unwrap()
            .contains("microphone was never allowed"));
    }

    #[test]
    fn a_browser_that_blocks_sound_is_reported_until_the_tab_is_clicked() {
        let rig = rig(Settings::default(), Some("sk-test"));
        let (id, _inbox) = page(&rig);
        say(
            &rig,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        rig.service.toggle();
        say(&rig, id, json!({"type": "audio", "on": false}));
        assert!(rig
            .service
            .status()
            .notice
            .unwrap()
            .contains("Click the Agent View voice tab"));
        say(&rig, id, json!({"type": "audio", "on": true}));
        assert_eq!(rig.service.status().notice, None);
    }

    #[test]
    fn the_page_reporting_the_call_over_ends_it() {
        let rig = rig(Settings::default(), Some("sk-test"));
        let (id, _inbox) = page(&rig);
        say(
            &rig,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        rig.service.toggle();
        say(
            &rig,
            id,
            json!({"type": "ended", "reason": "connection failed"}),
        );
        assert_eq!(rig.service.status().phase, Phase::Idle);
        assert!(rig
            .service
            .status()
            .notice
            .unwrap()
            .starts_with("Connection failed."));
    }
}

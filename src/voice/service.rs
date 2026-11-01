//! Voice Mode's owner: starts and ends calls, relays events between the voice
//! page and GPT-Live, and runs the tools the backend model asks for.

use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::call::{self, Action, Call, Line};
use super::config::{self, Settings};
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

/// What the dashboard shows about the call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelStatus {
    pub phase: Phase,
    pub seconds: u64,
    pub lines: Vec<Line>,
    /// Why the last call ended, or something the person should do.
    pub notice: Option<String>,
}

impl PanelStatus {
    pub fn idle() -> Self {
        Self {
            phase: Phase::Idle,
            seconds: 0,
            lines: Vec::new(),
            notice: None,
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
    last_heard: Instant,
    last_poll: Instant,
    warned: bool,
    call: Call,
    tools: Arc<Tools>,
}

#[derive(Default)]
struct State {
    server: Option<std::result::Result<u16, String>>,
    page: Option<Page>,
    pages: u64,
    active: Option<ActiveCall>,
    notice: Option<String>,
}

type Opener = Box<dyn Fn(&str) + Send + Sync>;
type KeySource = Box<dyn Fn() -> Option<String> + Send + Sync>;
type Creator = Box<dyn Fn(&str, &Value) -> Result<Created> + Send + Sync>;

struct Inner {
    host: Arc<dyn VoiceHost>,
    settings: Settings,
    port: u16,
    state: Mutex<State>,
    open_browser: Opener,
    key: KeySource,
    create: Creator,
}

#[derive(Clone)]
pub struct VoiceService {
    inner: Arc<Inner>,
}

impl VoiceService {
    pub fn new(host: Arc<dyn VoiceHost>) -> Self {
        Self::with_parts(
            host,
            Settings::load(),
            config::DEFAULT_PORT,
            Box::new(open_in_browser),
            Box::new(config::api_key),
            Box::new(create_session),
        )
    }

    pub(crate) fn with_parts(
        host: Arc<dyn VoiceHost>,
        settings: Settings,
        port: u16,
        open_browser: Opener,
        key: KeySource,
        create: Creator,
    ) -> Self {
        let service = Self {
            inner: Arc::new(Inner {
                host,
                settings,
                port,
                state: Mutex::default(),
                open_browser,
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
            self.inner.finish("You ended the call.", true);
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
            },
        }
    }

    /// Ends any call when the dashboard closes.
    pub fn shutdown(&self) {
        if self.inner.lock().active.is_some() {
            self.inner.finish("agentview closed.", true);
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
        tools.prime();
        let now = Instant::now();
        state.notice = None;
        state.active = Some(ActiveCall {
            stage: Stage::Waiting,
            since: now,
            live_since: None,
            opened_browser: false,
            last_heard: now,
            last_poll: now,
            warned: false,
            call: Call::default(),
            tools,
        });
        config::mark_hint_seen();
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
    fn finish(&self, reason: &str, notify_page: bool) {
        let mut state = self.lock();
        let Some(active) = state.active.take() else {
            return;
        };
        state.notice = Some(reason.to_owned());
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
        let idle_limit = Duration::from_secs(self.settings.idle_minutes * 60);
        let max = Duration::from_secs(self.settings.max_minutes * 60);
        match active.stage {
            Stage::Waiting => {
                if active.since.elapsed() >= PAGE_GRACE && !active.opened_browser {
                    active.opened_browser = true;
                    (self.open_browser)(&format!("http://127.0.0.1:{}/", self.port));
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
                if active.since.elapsed() >= START_TIMEOUT {
                    drop(state);
                    self.finish("The voice call did not connect.", true);
                }
            }
            Stage::Live => {
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
                    drop(state);
                    // The host and the tools can take a moment: not under the lock.
                    let polled = tools.poll();
                    let state = self.lock();
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
                Action::Closed => self.finish("The call ended.", false),
                Action::Failed(message) => {
                    self.lock().notice = Some(message);
                }
                Action::Run(calls) => {
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
                    thread::spawn(move || {
                        let send = |event: Value| {
                            let _ =
                                outgoing.send(json!({"type": "send", "event": event}).to_string());
                        };
                        for call_ in &calls {
                            let args: Value =
                                serde_json::from_str(&call_.arguments).unwrap_or(Value::Null);
                            let result = tools.run(&call_.name, &args);
                            send(call::function_output(&call_.call_id, &result));
                        }
                        send(call::continue_response());
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
        // A newer page replaces the older: dropping its sender closes it.
        state.page = Some(Page {
            id,
            outgoing,
            ready: false,
        });
        id
    }

    fn message(&self, page: u64, message: Value) {
        let this = &self.0;
        match message
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "hello" => {
                let mut state = this.lock();
                if state.page.as_ref().map(|p| p.id) != Some(page) {
                    return;
                }
                let ready = message
                    .get("ready")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if let Some(p) = state.page.as_mut() {
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
        if state.page.as_ref().map(|p| p.id) == Some(page) {
            state.page = None;
        }
    }
}

const MIC_NOTICE: &str = "Allow the microphone in the Agent View voice tab.";
const CLICK_NOTICE: &str = "Click the Agent View voice tab once so the browser lets it play sound.";

fn sentence(reason: &str) -> String {
    let mut chars = reason.chars();
    let first = chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default();
    format!("{first}{}.", chars.as_str().trim_end_matches('.'))
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
        say(&rig, id, json!({"type": "hello", "ready": true}));
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
        say(&rig, id, json!({"type": "hello", "ready": false}));
        assert_eq!(next(&inbox)["type"], "start");
        assert!(rig.service.status().notice.unwrap().contains("microphone"));
        say(&rig, id, json!({"type": "hello", "ready": true}));
        assert_eq!(rig.service.status().notice, None);
    }

    #[test]
    fn a_failed_session_creation_ends_the_call_with_the_reason() {
        let host = FakeHost::with(Vec::new());
        let service = VoiceService::with_parts(
            host,
            Settings::default(),
            0,
            Box::new(|_| {}),
            Box::new(|| Some("sk-test".into())),
            Box::new(|_, _| bail!("OpenAI answered 401 bad key")),
        );
        let (outgoing, inbox) = mpsc::channel();
        let sink = Sink(service.inner.clone());
        let id = sink.connected(outgoing);
        sink.message(id, json!({"type": "hello", "ready": true}));
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
        say(&rig, id, json!({"type": "hello", "ready": true}));
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
        say(&rig, id, json!({"type": "hello", "ready": true}));
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

    #[test]
    fn a_browser_that_blocks_sound_is_reported_until_the_tab_is_clicked() {
        let rig = rig(Settings::default(), Some("sk-test"));
        let (id, _inbox) = page(&rig);
        say(&rig, id, json!({"type": "hello", "ready": true}));
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
        say(&rig, id, json!({"type": "hello", "ready": true}));
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

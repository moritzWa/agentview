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
use super::config::{self, Provider, Settings};
use super::log::{clip, log};
use super::server::{self, PageSink};
use super::tools::{Tools, VoiceHost};
use super::xai;

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
#[derive(Clone, Debug, PartialEq)]
pub struct PanelStatus {
    pub phase: Phase,
    pub seconds: u64,
    pub lines: Vec<Line>,
    /// Why the last call ended, or something the person should do.
    pub notice: Option<String>,
    /// The notice only says an ordinary end ("You ended the call"): nothing to act on.
    pub quiet: bool,
    /// Whose voice a call uses (or would use).
    pub provider: Provider,
    /// How fast the voice talks, when the provider lets it change.
    pub speed: Option<f64>,
    /// Seconds since a call was paused, while it can still be picked up.
    pub paused_for: Option<u64>,
}

impl PanelStatus {
    pub fn idle() -> Self {
        Self {
            phase: Phase::Idle,
            seconds: 0,
            lines: Vec::new(),
            notice: None,
            quiet: false,
            provider: Provider::OpenAi,
            speed: None,
            paused_for: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Toggle {
    Started,
    Ended,
    NeedKey(Provider),
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
    provider: Provider,
    adapter: xai::Adapter,
    /// Grok's token, being fetched while the browser starts.
    token: Option<TokenSlot>,
    /// When the key was pressed, to time how long until the voice is live.
    pressed: Instant,
    /// Grok's conversation id, once it has said it.
    conversation: Option<String>,
    /// This call picks up a parked one.
    resume: Option<Resume>,
}

/// What a resumed call carries over.
struct Resume {
    conversation: Option<String>,
    recap: String,
    minutes_ago: u64,
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
    /// When the last call was ended, to time how fast the voice went quiet.
    ended_at: Option<Instant>,
}

/// Gets a browser onto the voice page; a hidden one comes back to be stopped later.
type Launcher = Box<dyn Fn(&str) -> Option<std::process::Child> + Send + Sync>;
type KeySource = Box<dyn Fn(Provider) -> Option<String> + Send + Sync>;
/// Trades an xAI key for a token the page may use.
/// Where a token being fetched ends up: nothing yet, then the token or why not.
type TokenSlot = Arc<Mutex<Option<std::result::Result<String, String>>>>;
type Minter = Box<dyn Fn(&str) -> Result<String> + Send + Sync>;
type Creator = Box<dyn Fn(&str, &Value) -> Result<Created> + Send + Sync>;

/// Remembers a setting between runs. Tests leave the real file alone.
fn persist(name: &str, value: Value) -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    config::save_setting(name, value)
}

/// The last things said in a call, for a provider that cannot replay them.
fn recap(lines: &[Line]) -> String {
    let mut text = String::new();
    for line in lines {
        let who = if line.who == call::Who::You {
            "Developer"
        } else {
            "You"
        };
        text.push_str(&format!("{who}: {}\n", line.text.trim()));
    }
    clip(text.trim(), 1200)
}

/// What a person can change while agentview runs.
struct Live {
    /// Chosen with `/voice grok`; empty means whichever provider has a key.
    provider: Option<Provider>,
    speed: f64,
}

/// What was typed in a session's input box when the call started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub text: String,
}

/// A call that was ended on purpose, kept for a while so the next press of the
/// key picks it up where it was.
struct Parked {
    tools: Arc<Tools>,
    provider: Provider,
    /// Grok's id for the conversation, which xAI replays for 30 minutes.
    conversation: Option<String>,
    /// The last things said, for a provider that cannot replay.
    recap: String,
    at: Instant,
}

/// The hidden browser of a call that just ended, kept for the next one.
#[derive(Default)]
struct Warm {
    child: Option<std::process::Child>,
    /// Which parking this is, so an old timer does not stop a newer browser.
    generation: u64,
}

struct Inner {
    host: Arc<dyn VoiceHost>,
    warm: Arc<Mutex<Warm>>,
    parked: Mutex<Option<Parked>>,
    settings: Settings,
    live: Mutex<Live>,
    port: u16,
    state: Mutex<State>,
    launch: Launcher,
    key: KeySource,
    create: Creator,
    mint: Minter,
}

#[derive(Clone)]
pub struct VoiceService {
    inner: Arc<Inner>,
}

impl VoiceService {
    pub fn new(host: Arc<dyn VoiceHost>) -> Self {
        let settings = Settings::load();
        // A browser an earlier agentview left running (it was killed while one was kept) is stopped.
        if let Some(profile) = config::browser_profile_dir() {
            thread::spawn(move || browser::stop_orphans(&profile));
        }
        let launch = launcher(&settings);
        Self::with_parts(
            host,
            settings,
            config::port(),
            launch,
            Box::new(config::key_for),
            Box::new(create_session),
            Box::new(xai::mint_token),
        )
    }

    pub(crate) fn with_parts(
        host: Arc<dyn VoiceHost>,
        settings: Settings,
        port: u16,
        launch: Launcher,
        key: KeySource,
        create: Creator,
        mint: Minter,
    ) -> Self {
        let live = Live {
            provider: Provider::parse(&settings.provider),
            speed: config::clamp_speed(settings.speed),
        };
        let service = Self {
            inner: Arc::new(Inner {
                host,
                warm: Arc::default(),
                parked: Mutex::new(None),
                settings,
                live: Mutex::new(live),
                port,
                state: Mutex::default(),
                launch,
                key,
                create,
                mint,
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

    /// Pauses the call: the voice stops at once and the call is kept, so the
    /// next press of the key continues it. Returns whether a call was on.
    pub fn pause(&self) -> bool {
        if self.inner.lock().active.is_none() {
            return false;
        }
        self.inner.finish_quietly("Paused.", true);
        true
    }

    /// Lets go of a paused call: the next one starts fresh.
    pub fn forget(&self) {
        *self.inner.parked.lock().unwrap() = None;
    }

    /// Whether a call is on right now.
    pub fn is_live(&self) -> bool {
        self.inner.lock().active.is_some()
    }

    /// Whose voice the next call uses.
    pub fn provider(&self) -> Provider {
        self.inner.provider()
    }

    /// Chooses whose voice the next call uses, and remembers it.
    pub fn set_provider(&self, provider: Provider) -> Result<()> {
        self.inner.live.lock().unwrap().provider = Some(provider);
        persist("provider", json!(provider.id()))
    }

    pub fn speed(&self) -> f64 {
        self.inner.live.lock().unwrap().speed
    }

    /// Changes how fast Grok talks, now and in later calls. Returns the speed
    /// that was set, which is kept inside what Grok accepts.
    pub fn set_speed(&self, speed: f64) -> f64 {
        let speed = config::clamp_speed(speed);
        self.inner.live.lock().unwrap().speed = speed;
        let _ = persist("speed", json!(speed));
        let state = self.inner.lock();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.provider == Provider::Xai)
        {
            // Already Grok's own event: it is passed through as it is.
            Inner::send_page(
                &state,
                json!({"type": "send", "event": xai::speed_update(speed)}),
            );
        }
        speed
    }

    /// Whether the provider that would answer has a key.
    pub fn has_key(&self) -> bool {
        (self.inner.key)(self.inner.provider()).is_some()
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.inner.port)
    }

    /// Starts a call, or ends the one in progress.
    pub fn toggle(&self) -> Toggle {
        self.toggle_with(None)
    }

    /// The same, with what the person had typed into a session's input box:
    /// the call starts knowing about it.
    pub fn toggle_with(&self, draft: Option<Draft>) -> Toggle {
        if self.inner.lock().active.is_some() {
            self.inner.finish_quietly("You ended the call.", true);
            return Toggle::Ended;
        }
        let provider = self.inner.provider();
        if (self.inner.key)(provider).is_none() {
            return Toggle::NeedKey(provider);
        }
        match self.inner.start(draft) {
            Ok(()) => Toggle::Started,
            Err(error) => Toggle::Failed(format!("{error:#}")),
        }
    }

    pub fn status(&self) -> PanelStatus {
        let provider = self.inner.provider();
        let speed = provider.has_speed().then(|| self.speed());
        let paused_for = self.inner.paused_for();
        let state = self.inner.lock();
        match &state.active {
            None => PanelStatus {
                notice: state.notice.clone(),
                quiet: state.quiet,
                provider,
                speed,
                paused_for,
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
                provider: active.provider,
                speed: active.provider.has_speed().then_some(speed.unwrap_or(1.0)),
                paused_for: None,
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
        if let Some(child) = self.inner.warm.lock().unwrap().child.take() {
            browser::stop(child);
        }
    }
}

impl Inner {
    /// How long the paused call has been paused, while it can still be picked up.
    fn paused_for(&self) -> Option<u64> {
        let parked = self.parked.lock().unwrap();
        let at = parked.as_ref()?.at.elapsed();
        (at < self.resume_window()).then_some(at.as_secs())
    }

    fn resume_window(&self) -> Duration {
        Duration::from_secs(self.settings.resume_minutes * 60)
    }

    fn provider(&self) -> Provider {
        self.live
            .lock()
            .unwrap()
            .provider
            .unwrap_or_else(|| self.settings.chosen_provider())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn send_page(state: &State, message: Value) {
        let mut message = message;
        // Grok has its own events: say what the call means in them.
        if message["type"] == "send"
            && state
                .active
                .as_ref()
                .is_some_and(|active| active.provider == Provider::Xai)
        {
            match xai::outgoing(&message["event"]) {
                Some(event) => message["event"] = event,
                None => return,
            }
        }
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

    fn start(self: &Arc<Self>, draft: Option<Draft>) -> Result<()> {
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
        let provider = self.provider();
        // A call paused a little while ago continues: same provider, not too long ago.
        let parked = self.parked.lock().unwrap().take().filter(|parked| {
            parked.provider == provider && parked.at.elapsed() < self.resume_window()
        });
        let resume = parked.as_ref().map(|parked| Resume {
            conversation: parked.conversation.clone(),
            recap: parked.recap.clone(),
            minutes_ago: parked.at.elapsed().as_secs() / 60,
        });
        let resumed = resume.is_some();
        let tools = match parked {
            Some(parked) => parked.tools,
            None => {
                let tools = Arc::new(Tools::new(self.host.clone()));
                tools.prime();
                tools
            }
        };
        tools.set_viewing(viewing());
        let has_draft = draft.is_some();
        tools.set_draft(draft.map(|draft| draft.text));
        if has_draft {
            log("draft", json!({"notes": tools.notes()}));
        }
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
            provider,
            adapter: xai::Adapter::default(),
            token: None,
            pressed: now,
            conversation: None,
            resume,
        });
        config::mark_hint_seen();
        super::log::start_call();
        log(
            "start",
            json!({"browser": self.settings.browser, "provider": provider.id(), "resumed": resumed}),
        );
        if provider == Provider::Xai {
            // The token is fetched while the browser starts, not after it.
            let slot = Arc::new(Mutex::new(None));
            if let Some(active) = state.active.as_mut() {
                active.token = Some(slot.clone());
            }
            let this = self.clone();
            thread::spawn(move || {
                let token = (this.key)(Provider::Xai)
                    .context("there is no xAI key")
                    .and_then(|key| (this.mint)(&key))
                    .map_err(|error| format!("{error:#}"));
                *slot.lock().unwrap() = Some(token);
            });
        }
        // The browser of the last call may still be up: use it. Otherwise a
        // hidden browser starts at once. A tab in your own browser is only
        // opened after a pinned one has had a moment to connect.
        let warm = match self.take_warm() {
            Some(child) if state.page.is_some() => Some(child),
            Some(child) => {
                browser::stop(child);
                None
            }
            None => None,
        };
        if let Some(child) = warm {
            if let Some(active) = state.active.as_mut() {
                active.opened_browser = true;
                active.browser = Some(child);
            }
        } else if state.page.is_none() && Mode::parse(&self.settings.browser) == Mode::Hidden {
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
        let provider = state
            .active
            .as_ref()
            .map_or(Provider::OpenAi, |active| active.provider);
        Self::send_page(state, json!({"type": "start", "provider": provider.id()}));
    }

    /// Ends the call: tells OpenAI to close, then the page to hang up.
    /// Keeps the hidden browser a little while: the next call finds it up, with
    /// the microphone already allowed, and starts at once.
    fn park(&self, child: std::process::Child) {
        let keep = Duration::from_secs(self.settings.keep_warm_seconds);
        let generation = {
            let mut warm = self.warm.lock().unwrap();
            if let Some(old) = warm.child.take() {
                browser::stop(old);
            }
            warm.generation += 1;
            warm.child = Some(child);
            warm.generation
        };
        let warm = self.warm.clone();
        thread::spawn(move || {
            thread::sleep(keep);
            let mut warm = warm.lock().unwrap();
            if warm.generation == generation {
                if let Some(child) = warm.child.take() {
                    browser::stop(child);
                }
            }
        });
    }

    /// The parked browser, when it is still running.
    fn take_warm(&self) -> Option<std::process::Child> {
        let mut warm = self.warm.lock().unwrap();
        warm.generation += 1;
        let mut child = warm.child.take()?;
        match child.try_wait() {
            Ok(None) => Some(child),
            _ => None,
        }
    }

    /// An ordinary end the person asked for: it is noted, not announced.
    fn finish_quietly(&self, reason: &str, notify_page: bool) {
        self.finish_with(reason, notify_page, true);
        self.lock().quiet = true;
    }

    fn finish(&self, reason: &str, notify_page: bool) {
        self.finish_with(reason, notify_page, false);
    }

    /// `park`: the call was ended on purpose, and it is kept for a while.
    fn finish_with(&self, reason: &str, notify_page: bool, park: bool) {
        let mut state = self.lock();
        let Some(mut active) = state.active.take() else {
            return;
        };
        if park && active.stage == Stage::Live {
            *self.parked.lock().unwrap() = Some(Parked {
                tools: active.tools.clone(),
                provider: active.provider,
                conversation: active.conversation.clone(),
                recap: recap(&active.call.lines()),
                at: Instant::now(),
            });
        }
        // First of all, the voice stops: nothing below waits before it.
        if let Some(page) = &state.page {
            let _ = page.outgoing.send(json!({"type": "silence"}).to_string());
        }
        state.quiet = false;
        state.ended_at = Some(Instant::now());
        state.notice = Some(reason.to_owned());
        log("end", json!({"reason": reason}));
        if let Some(child) = active.browser.take() {
            if notify_page && self.settings.keep_warm_seconds > 0 && state.page.is_some() {
                self.park(child);
            } else {
                // Give OpenAI's close a moment to go out before the browser stops.
                let wait = if notify_page { 1500 } else { 0 };
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(wait));
                    browser::stop(child);
                });
            }
        }
        let seconds = active.call.usage_seconds;
        if notify_page {
            if active.provider == Provider::OpenAi {
                Self::send_page(&state, json!({"type": "send", "event": call::close()}));
            }
            if let Some(page) = &state.page {
                let outgoing = page.outgoing.clone();
                // Grok has no close event: closing its websocket ends the call.
                // OpenAI is given a moment to receive its close first.
                let wait = if active.provider == Provider::OpenAi {
                    1200
                } else {
                    0
                };
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(wait));
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
        let (tools, provider, token, resume) = {
            let state = self.lock();
            let Some(active) = state
                .active
                .as_ref()
                .filter(|active| active.stage == Stage::Negotiating)
            else {
                return;
            };
            let resume = active
                .resume
                .as_ref()
                .map(|r| (r.conversation.clone(), r.recap.clone(), r.minutes_ago));
            (
                active.tools.clone(),
                active.provider,
                active.token.clone(),
                resume,
            )
        };
        let this = self.clone();
        thread::spawn(move || {
            tools.set_viewing(viewing());
            let mut notes = tools.notes();
            log("context", json!({"notes": notes}));
            let conversation = resume.as_ref().and_then(|(id, _, _)| id.clone());
            if let Some((_, recap, minutes)) = &resume {
                // Grok replays the call itself; the other provider is told the end of it.
                if provider == Provider::OpenAi || conversation.is_none() {
                    notes.push_str(&format!(
                        "\n\nThis call continues one the developer paused {minutes} min ago. The last things said:\n{recap}"
                    ));
                } else {
                    notes.push_str(&format!(
                        "\n\nThis call continues one the developer paused {minutes} min ago."
                    ));
                }
            }
            let key = (this.key)(provider)
                .with_context(|| format!("there is no {} key", provider.label()));
            // Grok's token was asked for when the call started; give it a moment.
            let prefetched = token.and_then(|slot| {
                (0..250)
                    .find_map(|_| {
                        let ready = slot.lock().unwrap().take();
                        if ready.is_none() {
                            thread::sleep(Duration::from_millis(20));
                        }
                        ready
                    })
                    .map(|result| result.map_err(|error| anyhow::anyhow!(error)))
            });
            match provider {
                Provider::OpenAi => {
                    let result = key.and_then(|key| {
                        (this.create)(&key, &call::session_request(&this.settings, &notes, &sdp))
                    });
                    match result {
                        Ok(created) => {
                            let mut state = this.lock();
                            if let Some(active) = state.active.as_mut() {
                                active.call.session_id = Some(created.session_id);
                                Self::send_page(
                                    &state,
                                    json!({"type": "answer", "sdp": created.sdp}),
                                );
                            }
                        }
                        Err(error) => {
                            this.finish(&format!("Could not start the call: {error:#}"), true)
                        }
                    }
                }
                Provider::Xai => {
                    match prefetched.unwrap_or_else(|| key.and_then(|key| (this.mint)(&key))) {
                        Ok(token) => {
                            let mut settings = this.settings.clone();
                            settings.speed = this.live.lock().unwrap().speed;
                            let state = this.lock();
                            Self::send_page(
                                &state,
                                json!({
                                    "type": "answer",
                                    "provider": "xai",
                                    "url": xai::realtime_url(&settings, conversation.as_deref()),
                                    "token": token,
                                    "session": xai::session_update(&settings, &notes),
                                    "context": notes,
                                }),
                            );
                        }
                        Err(error) => {
                            this.finish(&format!("Could not start the call: {error:#}"), true)
                        }
                    }
                }
            }
        });
    }

    fn on_event(self: &Arc<Self>, event: &Value) {
        let actions = {
            let mut state = self.lock();
            let Some(active) = state.active.as_mut() else {
                return;
            };
            if event.get("type").and_then(Value::as_str) == Some("conversation.created") {
                if let Some(id) = event.pointer("/conversation/id").and_then(Value::as_str) {
                    active.conversation = Some(id.to_owned());
                }
            }
            let events = if active.provider == Provider::Xai {
                active.adapter.incoming(event)
            } else {
                vec![event.clone()]
            };
            let actions: Vec<Action> = events
                .iter()
                .flat_map(|event| active.call.on_event(event))
                .collect();
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
                    // Back from a pause: say what happened in the sessions meanwhile.
                    let tools = state
                        .active
                        .as_ref()
                        .filter(|active| active.resume.is_some())
                        .map(|active| active.tools.clone());
                    if let Some(tools) = tools {
                        let polled = tools.poll();
                        let news: Vec<String> =
                            polled.spoken.into_iter().chain(polled.quiet).collect();
                        if !news.is_empty() {
                            Self::send_page(
                                &state,
                                json!({"type": "send", "event": call::commentary(&format!(
                                    "Welcome the developer back in one short sentence, then say what changed while they were away: {}",
                                    news.join(" ")
                                ))}),
                            );
                        }
                    }
                    if let Some(active) = state.active.as_mut() {
                        active.stage = Stage::Live;
                        log(
                            "live",
                            json!({"ms": active.pressed.elapsed().as_millis() as u64}),
                        );
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
                    let (tools, outgoing, provider) = {
                        let state = self.lock();
                        let Some(active) = state.active.as_ref() else {
                            return;
                        };
                        (
                            active.tools.clone(),
                            state.page.as_ref().map(|page| page.outgoing.clone()),
                            active.provider,
                        )
                    };
                    let Some(outgoing) = outgoing else { return };
                    let this = self.clone();
                    thread::spawn(move || {
                        let send = |event: Value| {
                            let event = if provider == Provider::Xai {
                                match xai::outgoing(&event) {
                                    Some(event) => event,
                                    None => return,
                                }
                            } else {
                                event
                            };
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
                if kind == "conversation.item.input_audio_transcription.completed" {
                    log(
                        "said",
                        json!({"who": kind, "text": event["transcript"], "status": event["status"]}),
                    );
                }
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
                // Grok's page has no SDP: it connects itself with a token.
                let sdp = message.get("sdp").and_then(Value::as_str);
                let grok = message.get("provider").and_then(Value::as_str) == Some("xai");
                if let Some(sdp) = sdp.or(grok.then_some("")) {
                    this.on_offer(sdp.to_owned());
                }
            }
            "event" => {
                if let Some(event) = message.get("event") {
                    this.on_event(event);
                }
            }
            "silenced" => {
                let ms = this
                    .lock()
                    .ended_at
                    .map(|at| at.elapsed().as_millis() as u64);
                log("silenced", json!({"ms_after_the_key": ms}));
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
            Box::new(move |_| key.map(str::to_owned)),
            Box::new(move |key, body| {
                assert_eq!(key, "sk-test");
                seen.lock().unwrap().push(body.clone());
                Ok(Created {
                    session_id: "live_1".into(),
                    sdp: "v=0 answer".into(),
                })
            }),
            Box::new(|_| Ok("grok-token".into())),
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
        assert_eq!(rig.service.toggle(), Toggle::NeedKey(Provider::OpenAi));
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
        // The voice is cut first, before the close goes out and the call is torn down.
        assert_eq!(next(&inbox)["type"], "silence");
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
            Box::new(|_| Some("sk-test".into())),
            Box::new(|_, _| bail!("OpenAI answered 401 bad key")),
            Box::new(|_| Ok("grok-token".into())),
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

    /// The next message of a kind, skipping others.
    fn next_of(inbox: &Receiver<String>, kind: &str) -> Value {
        loop {
            let message = next(inbox);
            if message["type"] == kind {
                return message;
            }
        }
    }

    fn next_is(inbox: &Receiver<String>, kind: &str) -> bool {
        inbox.try_iter().any(|message| {
            serde_json::from_str::<Value>(&message).is_ok_and(|value| value["type"] == kind)
        })
    }

    fn grok() -> Settings {
        Settings {
            provider: "grok".into(),
            ..Settings::default()
        }
    }

    #[test]
    fn a_grok_call_starts_with_its_provider_and_is_answered_with_a_token_and_a_session() {
        let rig = rig(grok(), Some("xai-test"));
        let (id, inbox) = page(&rig);
        say(
            &rig,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        assert_eq!(rig.service.toggle(), Toggle::Started);
        let start = next(&inbox);
        assert_eq!(
            (start["type"].as_str(), start["provider"].as_str()),
            (Some("start"), Some("xai"))
        );
        // Grok's page has no SDP to offer; the answer is a token and the session settings.
        say(&rig, id, json!({"type": "offer", "provider": "xai"}));
        let answer = next(&inbox);
        assert_eq!(answer["type"], "answer");
        assert_eq!(answer["provider"], "xai");
        assert_eq!(answer["token"], "grok-token");
        assert!(answer["url"]
            .as_str()
            .unwrap()
            .contains("grok-voice-latest"));
        assert_eq!(answer["session"]["session"]["voice"], "rex");
        assert!(
            rig.requests.lock().unwrap().is_empty(),
            "OpenAI is not asked"
        );
        assert_eq!(rig.service.status().provider, Provider::Xai);
        assert_eq!(rig.service.status().speed, Some(1.0));
    }

    #[test]
    fn grok_tool_calls_run_and_come_back_in_groks_events() {
        let rig = rig(grok(), Some("xai-test"));
        let (id, inbox) = page(&rig);
        say(
            &rig,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        rig.service.toggle();
        say(&rig, id, json!({"type": "offer", "provider": "xai"}));
        for event in [
            json!({"type": "session.updated"}),
            json!({"type": "response.created"}),
            json!({"type": "response.function_call_arguments.done", "call_id": "c1", "name": "list_sessions", "arguments": "{}"}),
            json!({"type": "response.done"}),
        ] {
            say(&rig, id, json!({"type": "event", "event": event}));
        }
        let mut seen = Vec::new();
        until(|| {
            seen.extend(
                inbox
                    .try_iter()
                    .filter_map(|m| serde_json::from_str::<Value>(&m).ok()),
            );
            seen.iter().any(|m| m["event"]["type"] == "response.create")
        });
        let output = seen
            .iter()
            .find(|m| m["event"]["type"] == "conversation.item.create")
            .expect("the tool result goes back");
        assert_eq!(output["event"]["item"]["type"], "function_call_output");
        assert_eq!(output["event"]["item"]["call_id"], "c1");
        assert!(output["event"]["item"]["output"]
            .as_str()
            .unwrap()
            .contains("semantic search"));
    }

    #[test]
    fn changing_the_speed_in_a_grok_call_tells_grok_and_openai_has_no_speed() {
        let call = rig(grok(), Some("xai-test"));
        let (id, inbox) = page(&call);
        say(
            &call,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        call.service.toggle();
        let _ = next(&inbox);
        assert_eq!(call.service.set_speed(9.0), 1.5);
        let update = next(&inbox);
        assert_eq!(update["event"]["type"], "session.update");
        assert_eq!(update["event"]["session"]["audio"]["output"]["speed"], 1.5);
        assert_eq!(call.service.speed(), 1.5);
        let other = rig(Settings::default(), Some("sk-test"));
        assert_eq!(other.service.status().speed, None);
    }

    #[test]
    fn ending_a_grok_call_silences_the_voice_and_closes_at_once() {
        let call = rig(grok(), Some("xai-test"));
        let (id, inbox) = page(&call);
        say(
            &call,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        call.service.toggle();
        let _ = next(&inbox);
        let started = Instant::now();
        assert_eq!(call.service.toggle(), Toggle::Ended);
        assert_eq!(next(&inbox)["type"], "silence");
        // Grok has no close event and nothing to wait for: the page ends right away.
        assert_eq!(next(&inbox)["type"], "end");
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn groks_token_is_fetched_while_the_browser_starts() {
        let host = FakeHost::with(Vec::new());
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        let service = VoiceService::with_parts(
            host,
            grok(),
            0,
            Box::new(|_| None),
            Box::new(|_| Some("xai-test".into())),
            Box::new(|_, _| bail!("OpenAI is not used")),
            Box::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok("early-token".into())
            }),
        );
        assert_eq!(service.toggle(), Toggle::Started);
        // No page has connected, and the token is already on its way.
        until(|| asked.load(Ordering::SeqCst) == 1);
        let (outgoing, inbox) = mpsc::channel();
        let sink = Sink(service.inner.clone());
        let id = sink.connected(outgoing);
        sink.message(id, json!({"type": "hello", "ready": true, "hidden": true}));
        assert_eq!(next(&inbox)["type"], "start");
        sink.message(id, json!({"type": "offer", "provider": "xai"}));
        assert_eq!(next(&inbox)["token"], "early-token");
        assert_eq!(asked.load(Ordering::SeqCst), 1, "one token, not two");
    }

    #[cfg(unix)]
    #[test]
    fn the_hidden_browser_is_kept_for_the_next_call() {
        let host = FakeHost::with(Vec::new());
        let launches = Arc::new(AtomicUsize::new(0));
        let counter = launches.clone();
        let service = VoiceService::with_parts(
            host,
            Settings::default(),
            0,
            Box::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                std::process::Command::new("sleep").arg("30").spawn().ok()
            }),
            Box::new(|_| Some("sk-test".into())),
            Box::new(|_, _| bail!("unused")),
            Box::new(|_| Ok("unused".into())),
        );
        assert_eq!(service.toggle(), Toggle::Started);
        until(|| launches.load(Ordering::SeqCst) == 1);
        let (outgoing, inbox) = mpsc::channel();
        let sink = Sink(service.inner.clone());
        let id = sink.connected(outgoing);
        sink.message(id, json!({"type": "hello", "ready": true, "hidden": true}));
        assert_eq!(service.toggle(), Toggle::Ended);
        // Not stopped: parked, with the page still connected.
        assert!(service.inner.warm.lock().unwrap().child.is_some());
        let _ = inbox.try_iter().count();
        assert_eq!(service.toggle(), Toggle::Started);
        assert_eq!(launches.load(Ordering::SeqCst), 1, "no second browser");
        assert_eq!(
            next(&inbox)["type"],
            "start",
            "the open page starts at once"
        );
        service.shutdown();
        assert!(service.inner.warm.lock().unwrap().child.is_none());
    }

    /// A call that is live: the page said hello, offered, and Grok said it was ready.
    fn live_grok_call(call: &Rig) -> (u64, Receiver<String>) {
        let (id, inbox) = page(call);
        say(
            call,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        call.service.toggle();
        say(call, id, json!({"type": "offer", "provider": "xai"}));
        say(
            call,
            id,
            json!({"type": "event", "event": {"type": "conversation.created", "conversation": {"id": "conv-1"}}}),
        );
        say(
            call,
            id,
            json!({"type": "event", "event": {"type": "session.updated"}}),
        );
        until(|| call.service.status().phase == Phase::Live);
        let _ = inbox.try_iter().count();
        (id, inbox)
    }

    #[test]
    fn a_call_that_was_ended_is_kept_and_the_next_one_continues_it() {
        let call = rig(grok(), Some("xai-test"));
        let (id, inbox) = live_grok_call(&call);
        say(
            &call,
            id,
            json!({"type": "event", "event": {"type": "session.input_transcript.delta", "delta": "check the auth one"}}),
        );
        assert!(call.service.pause());
        assert_eq!(next(&inbox)["type"], "silence");
        let status = call.service.status();
        assert_eq!(status.phase, Phase::Idle);
        assert_eq!(status.paused_for, Some(0));
        // The next press continues it: same conversation id goes back to Grok.
        assert_eq!(call.service.toggle(), Toggle::Started);
        let _ = inbox.try_iter().count();
        say(&call, id, json!({"type": "offer", "provider": "xai"}));
        let answer = loop {
            let message = next(&inbox);
            if message["type"] == "answer" {
                break message;
            }
        };
        assert!(answer["url"]
            .as_str()
            .unwrap()
            .ends_with("&conversation_id=conv-1"));
        assert_eq!(answer["session"]["session"]["resumption"]["enabled"], true);
        assert_eq!(
            call.service.status().paused_for,
            None,
            "continuing uses it up"
        );
    }

    #[test]
    fn a_forgotten_or_old_call_is_not_continued() {
        let call = rig(grok(), Some("xai-test"));
        let (_, inbox) = live_grok_call(&call);
        assert!(call.service.pause());
        call.service.forget();
        assert_eq!(call.service.status().paused_for, None);
        assert_eq!(call.service.toggle(), Toggle::Started);
        assert_eq!(next_of(&inbox, "start")["provider"], "xai");
        assert!(call
            .service
            .inner
            .lock()
            .active
            .as_ref()
            .unwrap()
            .resume
            .is_none());
        // An expired pause starts fresh as well. A zero-length window avoids
        // Instant subtraction underflow on freshly booted Windows runners.
        let mut settings = grok();
        settings.resume_minutes = 0;
        let call = rig(settings, Some("xai-test"));
        let (_, _inbox) = live_grok_call(&call);
        call.service.pause();
        assert_eq!(call.service.status().paused_for, None);
        call.service.toggle();
        assert!(call
            .service
            .inner
            .lock()
            .active
            .as_ref()
            .unwrap()
            .resume
            .is_none());
    }

    #[test]
    fn an_openai_call_continues_with_a_recap_of_the_end_of_the_last_one() {
        let call = rig(Settings::default(), Some("sk-test"));
        let (id, inbox) = page(&call);
        say(
            &call,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        call.service.toggle();
        say(&call, id, json!({"type": "offer", "sdp": "x"}));
        say(
            &call,
            id,
            json!({"type": "event", "event": {"type": "session.started", "session": {"id": "l"}}}),
        );
        say(
            &call,
            id,
            json!({"type": "event", "event": {"type": "session.input_transcript.delta", "delta": "the migration is on the billing branch"}}),
        );
        until(|| call.service.status().phase == Phase::Live);
        assert!(call.service.pause());
        let _ = inbox.try_iter().count();
        call.service.toggle();
        say(&call, id, json!({"type": "offer", "sdp": "y"}));
        until(|| call.requests.lock().unwrap().len() == 2);
        let body = call.requests.lock().unwrap()[1].to_string();
        assert!(
            body.contains("paused 0 min ago") && body.contains("billing branch"),
            "{body}"
        );
    }

    #[test]
    fn a_typed_draft_is_in_the_notes_and_send_draft_sends_the_final_text() {
        let call = rig(Settings::default(), Some("sk-test"));
        let (id, _inbox) = page(&call);
        say(
            &call,
            id,
            json!({"type": "hello", "ready": true, "hidden": true}),
        );
        crate::voice::set_viewing(Some("opencode:host:a".into()));
        assert_eq!(
            call.service.toggle_with(Some(Draft {
                text: "rename the settings".into()
            })),
            Toggle::Started
        );
        let tools = call
            .service
            .inner
            .lock()
            .active
            .as_ref()
            .unwrap()
            .tools
            .clone();
        let notes = tools.notes();
        assert!(
            notes.contains("rename the settings") && notes.contains("send_draft"),
            "{notes}"
        );
        let sent = tools.run(
            "send_draft",
            &json!({"text": "rename the settings\n\nKeep old names."}),
        );
        assert_eq!(sent["ok"], true, "{sent}");
        assert!(call.host.log.lock().unwrap().contains(
            &"send_draft opencode:host:a rename the settings\n\nKeep old names.".to_owned()
        ));
        // Sent once: there is no draft left to send.
        assert!(tools.run("send_draft", &json!({"text": "again"}))["error"].is_string());
        crate::voice::set_viewing(None);
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
            Box::new(|_| Some("sk-test".into())),
            Box::new(|_, _| bail!("unused")),
            Box::new(|_| Ok("grok-token".into())),
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
            Box::new(|_| Some("sk-test".into())),
            Box::new(|_, _| bail!("unused")),
            Box::new(|_| Ok("grok-token".into())),
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

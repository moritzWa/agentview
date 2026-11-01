//! One GPT-Live call: the events it sends, what to do about them, and the
//! JSON for what we send back.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

use super::config::Settings;

const LINES_KEPT: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    You,
    Agent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub who: Who,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

/// What an event asks of the service.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// The backend asked for these tools; return their results and continue.
    /// `stale`: the developer has said something new since this was asked.
    Run {
        calls: Vec<FunctionCall>,
        stale: bool,
    },
    /// OpenAI says the session is open.
    Started,
    /// OpenAI closed the session.
    Closed,
    /// An error worth showing.
    Failed(String),
}

#[derive(Default)]
pub struct Call {
    lines: VecDeque<Line>,
    pending: HashMap<String, Vec<FunctionCall>>,
    /// The newest request the voice handed to the backend.
    latest_delegation: String,
    pub session_id: Option<String>,
    pub usage_seconds: u64,
    /// Whether anyone has spoken since the call opened or since this was cleared.
    pub heard: bool,
    /// What they just said sounds like goodbye.
    pub farewell: bool,
    /// Whether anything they said has been transcribed yet.
    pub user_spoke: bool,
}

/// "That's all, bye": a short line that ends the call.
pub fn is_farewell(line: &str) -> bool {
    let text = line.to_lowercase().replace('\u{2019}', "'");
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|word| !word.is_empty())
        .collect();
    if words.is_empty() || words.len() > 12 {
        return false;
    }
    let joined = words.join(" ");
    [
        "goodbye",
        "good bye",
        "hang up",
        "that's all",
        "that is all",
        "end the call",
        "end call",
        "we're done",
        "i'm done",
        "that will be all",
        "talk to you later",
    ]
    .iter()
    .any(|phrase| joined.contains(phrase))
        || words.contains(&"bye")
}

impl Call {
    pub fn lines(&self) -> Vec<Line> {
        self.lines.iter().cloned().collect()
    }

    fn append(&mut self, who: Who, delta: &str) {
        if self.lines.back().map(|line| line.who) != Some(who) {
            self.lines.push_back(Line {
                who,
                text: String::new(),
            });
            while self.lines.len() > LINES_KEPT {
                self.lines.pop_front();
            }
        }
        if let Some(line) = self.lines.back_mut() {
            line.text.push_str(delta);
            if who == Who::You {
                self.farewell = is_farewell(&line.text);
                self.user_spoke = true;
            }
        }
    }

    pub fn on_event(&mut self, event: &Value) -> Vec<Action> {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "session.started" => {
                self.session_id = event
                    .pointer("/session/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                vec![Action::Started]
            }
            "session.input_transcript.delta" | "session.output_transcript.delta" => {
                let who = if kind.starts_with("session.input") {
                    Who::You
                } else {
                    Who::Agent
                };
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    self.append(who, delta);
                    self.heard = true;
                }
                Vec::new()
            }
            "session.usage.updated" | "session.closed" => {
                if let Some(seconds) = event.pointer("/usage/seconds").and_then(Value::as_u64) {
                    self.usage_seconds = self.usage_seconds.max(seconds);
                }
                if kind == "session.closed" {
                    vec![Action::Closed]
                } else {
                    Vec::new()
                }
            }
            "error" => {
                let message = event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("the voice session reported an error");
                vec![Action::Failed(message.to_owned())]
            }
            "session.delegation.created" => {
                if let Some(id) = event.pointer("/delegation/id").and_then(Value::as_str) {
                    self.latest_delegation = id.to_owned();
                }
                Vec::new()
            }
            "response.event" => self.on_response_event(event),
            _ => Vec::new(),
        }
    }

    fn on_response_event(&mut self, event: &Value) -> Vec<Action> {
        let delegation = event
            .get("delegation_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let inner = event.get("event").cloned().unwrap_or(Value::Null);
        match inner
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "response.output_item.done" => {
                let item = &inner["item"];
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    let field = |name: &str| {
                        item.get(name)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    };
                    self.pending
                        .entry(delegation)
                        .or_default()
                        .push(FunctionCall {
                            call_id: field("call_id"),
                            name: field("name"),
                            arguments: field("arguments"),
                        });
                }
                Vec::new()
            }
            "response.completed" | "response.done" => match self.pending.remove(&delegation) {
                Some(calls) if !calls.is_empty() => {
                    let stale =
                        !self.latest_delegation.is_empty() && self.latest_delegation != delegation;
                    vec![Action::Run { calls, stale }]
                }
                _ => Vec::new(),
            },
            "response.failed" | "response.incomplete" => {
                self.pending.remove(&delegation);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

static EVENT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn event_id(prefix: &str) -> String {
    format!(
        "{prefix}_{}",
        EVENT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn function_output(call_id: &str, output: &Value) -> Value {
    json!({
        "type": "response.item.create",
        "event_id": event_id("out"),
        "item": {"type": "function_call_output", "call_id": call_id, "output": output.to_string()},
    })
}

pub fn continue_response() -> Value {
    json!({"type": "response.create", "event_id": event_id("cont")})
}

/// Something the voice should say, in its own words.
pub fn commentary(text: &str) -> Value {
    json!({"type": "session.commentary.append", "event_id": event_id("say"), "delegation_id": null, "content": clip(text)})
}

/// Something the voice should know and not say.
pub fn thinking(text: &str) -> Value {
    json!({"type": "session.thinking.append", "event_id": event_id("note"), "delegation_id": null, "content": clip(text)})
}

pub fn instruction(text: &str) -> Value {
    json!({"type": "session.instructions.append", "event_id": event_id("inst"), "delegation_id": null, "content": clip(text)})
}

pub fn close() -> Value {
    json!({"type": "session.close", "event_id": event_id("close")})
}

/// An append holds 500 tokens at most and one over that is refused whole.
fn clip(text: &str) -> String {
    const LIMIT: usize = 1500;
    if text.chars().count() <= LIMIT {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(LIMIT - 1).collect();
    clipped.push('…');
    clipped
}

/// The body of `POST /v1/live/sessions`.
pub fn session_request(settings: &Settings, notes: &str, sdp: &str) -> Value {
    let tools: Value =
        serde_json::from_str(include_str!("tools.json")).expect("tools.json is valid");
    let mut responses = json!({
        "model": settings.backend_model,
        "instructions": include_str!("backend.md"),
        "reasoning": {"effort": settings.reasoning_effort},
        "tools": tools,
        "tool_choice": "auto",
    });
    if let Some(tier) = &settings.service_tier {
        responses["service_tier"] = json!(tier);
    }
    json!({
        "session": {
            "model": "gpt-live-1",
            "instructions": format!("{}\n\nStatus notes:\n{notes}", include_str!("talker.md")),
            "audio": {"output": {"voice": settings.voice}},
            "delegation": {"type": "responses", "responses": responses},
            "store": false,
        },
        "transport": {"type": "webrtc", "sdp": sdp},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_event(delegation: &str, inner: Value) -> Value {
        json!({"type": "response.event", "delegation_id": delegation, "event": inner})
    }

    fn done_call(call_id: &str, name: &str, arguments: &str) -> Value {
        json!({"type": "response.output_item.done", "item": {"type": "function_call", "call_id": call_id, "name": name, "arguments": arguments}})
    }

    #[test]
    fn calls_are_held_until_the_response_completes_then_run_together() {
        let mut call = Call::default();
        assert!(call
            .on_event(&response_event(
                "d1",
                done_call("c1", "list_sessions", "{}")
            ))
            .is_empty());
        assert!(call
            .on_event(&response_event(
                "d1",
                done_call("c2", "read_session", r#"{"id":"s1"}"#)
            ))
            .is_empty());
        let actions = call.on_event(&response_event("d1", json!({"type": "response.completed"})));
        let [Action::Run { calls, .. }] = actions.as_slice() else {
            panic!("expected one run: {actions:?}")
        };
        assert_eq!(
            calls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["list_sessions", "read_session"]
        );
        // Completing again, with nothing pending, runs nothing.
        assert!(call
            .on_event(&response_event("d1", json!({"type": "response.completed"})))
            .is_empty());
    }

    #[test]
    fn calls_of_different_delegations_do_not_mix() {
        let mut call = Call::default();
        call.on_event(&response_event(
            "d1",
            done_call("c1", "list_sessions", "{}"),
        ));
        call.on_event(&response_event(
            "d2",
            done_call("c2", "list_sessions", "{}"),
        ));
        let actions = call.on_event(&response_event("d2", json!({"type": "response.completed"})));
        assert!(
            matches!(&actions[0], Action::Run { calls, .. } if calls.len() == 1 && calls[0].call_id == "c2")
        );
    }

    #[test]
    fn calls_of_an_older_request_are_stale_once_a_newer_one_exists() {
        let mut call = Call::default();
        call.on_event(&json!({"type": "session.delegation.created", "delegation": {"id": "d1"}}));
        call.on_event(&response_event("d1", done_call("c1", "read_session", "{}")));
        call.on_event(&json!({"type": "session.delegation.created", "delegation": {"id": "d2"}}));
        let actions = call.on_event(&response_event("d1", json!({"type": "response.completed"})));
        assert!(matches!(&actions[0], Action::Run { stale: true, .. }));
        call.on_event(&response_event("d2", done_call("c2", "read_session", "{}")));
        let actions = call.on_event(&response_event("d2", json!({"type": "response.completed"})));
        assert!(matches!(&actions[0], Action::Run { stale: false, .. }));
    }

    #[test]
    fn a_failed_response_drops_its_calls() {
        let mut call = Call::default();
        call.on_event(&response_event(
            "d1",
            done_call("c1", "list_sessions", "{}"),
        ));
        call.on_event(&response_event("d1", json!({"type": "response.failed"})));
        assert!(call
            .on_event(&response_event("d1", json!({"type": "response.completed"})))
            .is_empty());
    }

    #[test]
    fn transcript_deltas_build_alternating_lines() {
        let mut call = Call::default();
        for (kind, delta) in [
            ("session.input_transcript.delta", "Which "),
            ("session.input_transcript.delta", "ones need me"),
            ("session.output_transcript.delta", "Checking."),
            ("session.output_transcript.delta", " Two do."),
            ("session.input_transcript.delta", "Thanks"),
        ] {
            call.on_event(&json!({"type": kind, "delta": delta}));
        }
        let lines = call.lines();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[0],
            Line {
                who: Who::You,
                text: "Which ones need me".into()
            }
        );
        assert_eq!(lines[1].text, "Checking. Two do.");
        assert!(call.heard);
    }

    #[test]
    fn a_short_goodbye_is_recognised_and_a_longer_request_is_not() {
        assert!(is_farewell("Okay, that is all for now. Thanks. Bye."));
        assert!(is_farewell("hang up"));
        assert!(is_farewell("That’s all, thank you"));
        assert!(!is_farewell("Tell the pineapple session that is all I needed from it and stop the old essay run as well"));
        assert!(!is_farewell("What does the bypass session do"));
        assert!(!is_farewell(""));
        let mut call = Call::default();
        call.on_event(&json!({"type": "session.input_transcript.delta", "delta": "okay bye"}));
        assert!(call.farewell);
        call.on_event(&json!({"type": "session.output_transcript.delta", "delta": "Bye."}));
        call.on_event(
            &json!({"type": "session.input_transcript.delta", "delta": "wait, one more thing"}),
        );
        assert!(!call.farewell);
    }

    #[test]
    fn only_the_latest_lines_are_kept() {
        let mut call = Call::default();
        for n in 0..20 {
            let kind = if n % 2 == 0 {
                "session.input_transcript.delta"
            } else {
                "session.output_transcript.delta"
            };
            call.on_event(&json!({"type": kind, "delta": n.to_string()}));
        }
        assert_eq!(call.lines().len(), LINES_KEPT);
        assert_eq!(call.lines().last().unwrap().text, "19");
    }

    #[test]
    fn usage_never_goes_backwards_and_closing_is_reported() {
        let mut call = Call::default();
        call.on_event(&json!({"type": "session.usage.updated", "usage": {"seconds": 40}}));
        call.on_event(&json!({"type": "session.usage.updated", "usage": {"seconds": 30}}));
        assert_eq!(call.usage_seconds, 40);
        assert_eq!(
            call.on_event(&json!({"type": "session.closed", "usage": {"seconds": 55}})),
            vec![Action::Closed]
        );
        assert_eq!(call.usage_seconds, 55);
    }

    #[test]
    fn the_session_opens_with_its_id_and_errors_are_surfaced() {
        let mut call = Call::default();
        assert_eq!(
            call.on_event(&json!({"type": "session.started", "session": {"id": "live_1"}})),
            vec![Action::Started]
        );
        assert_eq!(call.session_id.as_deref(), Some("live_1"));
        let actions = call.on_event(&json!({"type": "error", "error": {"message": "no credits"}}));
        assert_eq!(actions, vec![Action::Failed("no credits".into())]);
    }

    #[test]
    fn the_session_request_carries_the_tools_the_voice_and_no_storage() {
        let body = session_request(&Settings::default(), "2 recent sessions", "v=0 offer");
        assert_eq!(body["session"]["model"], "gpt-live-1");
        assert_eq!(body["session"]["audio"]["output"]["voice"], "meridian");
        assert_eq!(body["session"]["store"], false);
        assert_eq!(body["session"]["delegation"]["type"], "responses");
        assert_eq!(
            body["session"]["delegation"]["responses"]["model"],
            "gpt-6-luna"
        );
        let names: Vec<&str> = body["session"]["delegation"]["responses"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "list_sessions",
                "read_session",
                "send_message",
                "interrupt_session",
                "edit_last_message",
                "start_session",
                "watch_session",
                "end_call"
            ]
        );
        assert!(body["session"]["instructions"]
            .as_str()
            .unwrap()
            .ends_with("2 recent sessions"));
        assert_eq!(body["transport"]["sdp"], "v=0 offer");
    }

    #[test]
    fn appended_context_is_kept_under_the_size_limit() {
        let long = "word ".repeat(1000);
        assert!(
            commentary(&long)["content"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                <= 1500
        );
        assert_eq!(thinking("hi")["delegation_id"], Value::Null);
    }
}

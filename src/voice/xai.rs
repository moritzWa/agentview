//! Grok's side of a call. GPT-Live and Grok speak differently, and the rest of
//! Voice Mode (the call, the tools, the logs) speaks GPT-Live. This module sets
//! a Grok session up and translates what comes back and what goes out, so
//! nothing else has to know which one is on the line.

use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::config::{clamp_speed, Settings};

/// The websocket the page opens, with the token in the subprotocol.
pub fn realtime_url(settings: &Settings) -> String {
    format!("wss://api.x.ai/v1/realtime?model={}", settings.xai_model)
}

/// The first `session.update`: voice, prompt with the status notes, tools,
/// no reasoning (it keeps the voice fast), server-side turn detection.
pub fn session_update(settings: &Settings, notes: &str) -> Value {
    let tools: Value =
        serde_json::from_str(include_str!("tools.json")).expect("tools.json is valid");
    json!({
        "type": "session.update",
        "session": {
            "voice": settings.xai_voice,
            "instructions": format!("{}\n\nStatus notes:\n{notes}", include_str!("xai.md")),
            "turn_detection": {"type": "server_vad"},
            "reasoning": {"effort": settings.xai_reasoning},
            "tools": tools,
            "audio": {
                "input": {
                    "format": {"type": "audio/pcm", "rate": 24000},
                    "transcription": {"model": "grok-transcribe"},
                },
                "output": {
                    "format": {"type": "audio/pcm", "rate": 24000},
                    "speed": clamp_speed(settings.speed),
                },
            },
        },
    })
}

/// Changes how fast the voice talks, in a call that is on.
pub fn speed_update(speed: f64) -> Value {
    json!({
        "type": "session.update",
        "session": {"audio": {"output": {"speed": clamp_speed(speed)}}},
    })
}

/// A short-lived token the browser may use instead of the key.
pub fn mint_token(key: &str) -> Result<String> {
    use std::io::Write;
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "20",
            "-X",
            "POST",
            "-H",
            "@-",
            "-H",
            "Content-Type: application/json",
            "--data",
            r#"{"expires_after":{"seconds":300}}"#,
            "-w",
            "\n%{http_code}",
            "https://api.x.ai/v1/realtime/client_secrets",
        ])
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
    if status.trim() != "200" {
        let detail = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/error/message")
                    .or_else(|| value.get("error"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| String::from_utf8_lossy(&output.stderr).trim().to_owned());
        bail!("xAI answered {} {detail}", status.trim());
    }
    serde_json::from_str::<Value>(body)?
        .get("value")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("xAI did not return a token")
}

/// Turns Grok's server events into the GPT-Live events the call understands.
#[derive(Default)]
pub struct Adapter {
    started: bool,
    /// The last spoken turn that was passed on: Grok repeats its final transcript.
    last_item: String,
    /// Bumped whenever the developer starts speaking.
    spoken: u64,
    /// `spoken` when the response now running began: its tool calls are stale
    /// if the developer has spoken since.
    response: u64,
}

impl Adapter {
    fn delegation(number: u64) -> String {
        format!("x{number}")
    }

    pub fn incoming(&mut self, event: &Value) -> Vec<Value> {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "session.updated" if !self.started => {
                self.started = true;
                vec![json!({"type": "session.started", "session": {"id": "grok"}})]
            }
            "input_audio_buffer.speech_started" => {
                self.spoken += 1;
                vec![
                    json!({"type": "session.speech_started"}),
                    json!({"type": "session.delegation.created", "delegation": {"id": Self::delegation(self.spoken)}}),
                ]
            }
            "conversation.item.input_audio_transcription.completed" => {
                // It says "in_progress" while it is still listening to the end of the turn.
                if event
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s != "completed")
                {
                    return Vec::new();
                }
                let item = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let text = event
                    .get("transcript")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim();
                if text.is_empty() || (!item.is_empty() && item == self.last_item) {
                    return Vec::new();
                }
                self.last_item = item.to_owned();
                vec![json!({"type": "session.input_transcript.delta", "delta": format!("{text} ")})]
            }
            "response.output_audio_transcript.delta" | "response.audio_transcript.delta" => {
                match event.get("delta").and_then(Value::as_str) {
                    Some(delta) => {
                        vec![json!({"type": "session.output_transcript.delta", "delta": delta})]
                    }
                    None => Vec::new(),
                }
            }
            "response.created" => {
                self.response = self.spoken;
                Vec::new()
            }
            "response.function_call_arguments.done" => {
                let field = |name: &str| event.get(name).cloned().unwrap_or(Value::Null);
                vec![json!({
                    "type": "response.event",
                    "delegation_id": Self::delegation(self.response),
                    "event": {"type": "response.output_item.done", "item": {
                        "type": "function_call",
                        "call_id": field("call_id"),
                        "name": field("name"),
                        "arguments": field("arguments"),
                    }},
                })]
            }
            "response.done" => vec![json!({
                "type": "response.event",
                "delegation_id": Self::delegation(self.response),
                "event": {"type": "response.completed"},
            })],
            "error" => vec![event.clone()],
            _ => Vec::new(),
        }
    }
}

/// Turns what the call sends (GPT-Live's events) into Grok's.
pub fn outgoing(event: &Value) -> Option<Value> {
    let kind = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content = || {
        event
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    match kind {
        "response.item.create" => {
            let item = event.get("item")?.clone();
            Some(json!({"type": "conversation.item.create", "item": item}))
        }
        "response.create" => Some(json!({"type": "response.create"})),
        // Something the voice should know and not say.
        "session.thinking.append" => Some(json!({
            "type": "conversation.item.create",
            "item": {"type": "message", "role": "system", "content": [{"type": "input_text", "text": content()}]},
        })),
        // Something the voice should say, in its own words.
        "session.commentary.append" | "session.instructions.append" => Some(json!({
            "type": "response.create",
            "response": {"instructions": format!("Tell the developer, briefly and in your own words: {}", content())},
        })),
        // Already Grok's own (a speed change).
        "session.update" => Some(event.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_update_picks_a_male_voice_no_reasoning_and_the_tools() {
        let update = session_update(&Settings::default(), "Sessions: none.");
        let session = &update["session"];
        assert_eq!(session["voice"], "rex");
        assert_eq!(session["reasoning"]["effort"], "none");
        assert_eq!(session["audio"]["output"]["speed"], 1.0);
        assert_eq!(session["turn_detection"]["type"], "server_vad");
        let names: Vec<&str> = session["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"send_message") && names.contains(&"search_history"));
        assert!(session["instructions"]
            .as_str()
            .unwrap()
            .ends_with("Sessions: none."));
    }

    #[test]
    fn speed_is_kept_inside_what_grok_accepts() {
        assert_eq!(
            speed_update(3.0)["session"]["audio"]["output"]["speed"],
            1.5
        );
    }

    #[test]
    fn grok_events_become_the_events_the_call_understands() {
        let mut adapter = Adapter::default();
        assert!(adapter
            .incoming(&json!({"type": "session.created"}))
            .is_empty());
        let started = adapter.incoming(&json!({"type": "session.updated"}));
        assert_eq!(started[0]["type"], "session.started");
        // Only the first update starts the call.
        assert!(adapter
            .incoming(&json!({"type": "session.updated"}))
            .is_empty());
        // Grok sends the transcript several times; only the finished one counts, once.
        let partial = json!({"type": "conversation.item.input_audio_transcription.completed",
            "item_id": "i1", "status": "in_progress", "transcript": "go"});
        assert!(adapter.incoming(&partial).is_empty());
        let finished = json!({"type": "conversation.item.input_audio_transcription.completed",
            "item_id": "i1", "status": "completed", "transcript": " go "});
        let said = adapter.incoming(&finished);
        assert_eq!(said[0]["type"], "session.input_transcript.delta");
        assert_eq!(said[0]["delta"], "go ");
        assert!(adapter.incoming(&finished).is_empty());
        let reply = adapter
            .incoming(&json!({"type": "response.output_audio_transcript.delta", "delta": "Sent."}));
        assert_eq!(reply[0]["type"], "session.output_transcript.delta");
    }

    #[test]
    fn a_tool_call_asked_before_the_developer_spoke_again_is_stale() {
        let mut adapter = Adapter::default();
        adapter.incoming(&json!({"type": "input_audio_buffer.speech_started"}));
        adapter.incoming(&json!({"type": "response.created"}));
        let call = adapter.incoming(&json!({
            "type": "response.function_call_arguments.done",
            "call_id": "c1", "name": "list_sessions", "arguments": "{}"}));
        assert_eq!(call[0]["delegation_id"], "x1");
        assert_eq!(call[0]["event"]["item"]["name"], "list_sessions");
        // They speak again before the response finishes.
        let created = adapter.incoming(&json!({"type": "input_audio_buffer.speech_started"}));
        assert_eq!(created[1]["delegation"]["id"], "x2");
        let done = adapter.incoming(&json!({"type": "response.done"}));
        assert_eq!(done[0]["delegation_id"], "x1");
    }

    #[test]
    fn the_calls_events_are_translated_for_grok() {
        let output = outgoing(&json!({"type": "response.item.create", "item": {
            "type": "function_call_output", "call_id": "c1", "output": "{}"}}))
        .unwrap();
        assert_eq!(output["type"], "conversation.item.create");
        assert_eq!(output["item"]["call_id"], "c1");
        assert_eq!(
            outgoing(&json!({"type": "response.create"})).unwrap()["type"],
            "response.create"
        );
        let note = outgoing(&json!({"type": "session.thinking.append", "content": "It finished."}))
            .unwrap();
        assert_eq!(note["item"]["role"], "system");
        let say =
            outgoing(&json!({"type": "session.commentary.append", "content": "It finished."}))
                .unwrap();
        assert_eq!(say["type"], "response.create");
        assert!(say["response"]["instructions"]
            .as_str()
            .unwrap()
            .contains("It finished."));
        assert!(outgoing(&json!({"type": "session.close"})).is_none());
    }
}

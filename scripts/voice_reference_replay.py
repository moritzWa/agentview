"""Requires websocket-client. Uses paid xAI realtime calls, with simulated tools and no audio playback."""

import websocket, json, os, time, subprocess, concurrent.futures
from pathlib import Path
import argparse

R = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(
    description="Replay the Nov 12 session mix-up against the real Grok voice model. Audio is discarded, never played; all tools are simulated."
)
parser.add_argument(
    "--baseline", default="HEAD", help="Git revision for the baseline prompt"
)
parser.add_argument("--runs", type=int, default=4)
parser.add_argument("--output", type=Path, required=True)
parser.add_argument(
    "--cases", default="ambiguous,named,recent", help="Comma-separated replay cases"
)
parser.add_argument(
    "--send-failed",
    action="store_true",
    help="Retain the original call's failed data-leads send instead of the successful ownership-fixed send",
)
options = parser.parse_args()
if not 1 <= options.runs <= 20:
    parser.error("--runs must be between 1 and 20")
key = os.environ.get("XAI_API_KEY")
if not key:
    key = json.loads(
        Path.home().joinpath(".local/state/agentview/voice/key.json").read_text()
    )["xai_api_key"]
fixture = json.loads(
    (R / "tests/fixtures/voice/session-reference-replay.json").read_text()
)
base = subprocess.check_output(
    ["git", "show", options.baseline + ":src/voice/xai.md"], cwd=R, text=True
)
new = (R / "src/voice/xai.md").read_text()
wld = {"list": fixture["list"]}
history = fixture["history"]
if options.send_failed:
    for message in history:
        if message.get("tool_call_id") == "a2":
            message["content"] = json.dumps(
                {"error": "reply authority was not granted for this session"}
            )

tools = json.loads((R / "src/voice/tools.json").read_text())


def run(job):
    variant, case = job
    w = websocket.create_connection(
        "wss://api.x.ai/v1/realtime?model=grok-voice-latest",
        header=["Authorization: Bearer " + key],
        timeout=45,
    )

    def send(v):
        w.send(json.dumps(v))

    trace = []
    try:
        send(
            {
                "type": "session.update",
                "session": {
                    "instructions": (base if variant == "baseline" else new)
                    + "\n\nStatus notes:\nCurrent screen: eu-biotech-data-leads (this).",
                    "tools": tools,
                    "turn_detection": None,
                    "reasoning": {"effort": "none"},
                },
            }
        )
        while True:
            ack = json.loads(w.recv())
            if ack.get("type") == "error":
                return {"variant": variant, "case": case, "error": ack}
            if ack.get("type") == "session.updated":
                assert ack["session"]["instructions"].startswith(
                    (base if variant == "baseline" else new).splitlines()[0]
                )
                break
        for m in history:
            role = m["role"]
            if m.get("tool_calls"):
                for c in m["tool_calls"]:
                    send(
                        {
                            "type": "conversation.item.create",
                            "item": {
                                "type": "function_call",
                                "call_id": c["id"],
                                "name": c["function"]["name"],
                                "arguments": c["function"]["arguments"],
                            },
                        }
                    )
            elif role == "tool":
                send(
                    {
                        "type": "conversation.item.create",
                        "item": {
                            "type": "function_call_output",
                            "call_id": m["tool_call_id"],
                            "output": m["content"],
                        },
                    }
                )
            elif m.get("content"):
                send(
                    {
                        "type": "conversation.item.create",
                        "item": {
                            "type": "message",
                            "role": role,
                            "content": [
                                {
                                    "type": "input_text" if role == "user" else "text",
                                    "text": m["content"],
                                }
                            ],
                        },
                    }
                )
        utter = {
            "ambiguous": "Did the session respond finally?",
            "named": "Did the data leads session respond finally?",
            "recent": "What did the website session you just started say?",
            "relay": "Tell the data leads session to continue the register check.",
            "stop": "Stop the website footer session.",
            "end": "That is all, hang up.",
        }[case]
        send(
            {
                "type": "conversation.item.create",
                "item": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": utter}],
                },
            }
        )
        send({"type": "response.create"})
        for step in range(3):
            calls = []
            said = ""
            end = time.monotonic() + 75
            while time.monotonic() < end:
                e = json.loads(w.recv())
                t = e.get("type", "")
                if t == "error":
                    return {"variant": variant, "case": case, "error": e}
                if t == "response.output_audio_transcript.delta":
                    said += e.get("delta", "")
                if t == "response.function_call_arguments.done":
                    calls.append(e)
                if t == "response.done":
                    break
            if not calls:
                return {"variant": variant, "case": case, "said": said, "trace": trace}
            for c in calls:
                name = c["name"]
                args = json.loads(c["arguments"])
                i = args.get("id", "")
                if name == "read_session":
                    trace.append(i)
                    h = (
                        "s1"
                        if i in ("this", "eu-biotech-data-leads")
                        else "s2"
                        if i == "website footer line"
                        else i
                    )
                    rows = {row["id"]: row for row in wld["list"]["sessions"]}
                    row = rows.get(h, {})
                    result = {
                        "title": row.get("title", i),
                        "state": "done",
                        "answer_ready": True,
                        "turns": [
                            {
                                "you": "Run the register check."
                                if h == "s1"
                                else "Add the footer line.",
                                "it": "The register check came back: UK confirmed, two EU entries missing."
                                if h == "s1"
                                else "Added the line to the footer and pushed.",
                            }
                        ],
                        "context": {
                            "on_screen": "eu-biotech-data-leads",
                            "this_result_is_about": row.get("title", i),
                            "watching": [
                                "eu-biotech-data-leads",
                                "website footer line",
                            ],
                        },
                    }
                    if h == "s2":
                        result["context"]["warning"] = (
                            "The developer is looking at eu-biotech-data-leads, but this is about website footer line. If they said the session, it or that one, check they meant this one, or ask which."
                        )
                    send(
                        {
                            "type": "conversation.item.create",
                            "item": {
                                "type": "function_call_output",
                                "call_id": c["call_id"],
                                "output": json.dumps(result),
                            },
                        }
                    )
                    continue
                if name != "list_sessions":
                    return {
                        "variant": variant,
                        "case": case,
                        "action": name,
                        "args": args,
                    }
                send(
                    {
                        "type": "conversation.item.create",
                        "item": {
                            "type": "function_call_output",
                            "call_id": c["call_id"],
                            "output": json.dumps(wld["list"]),
                        },
                    }
                )
            send({"type": "response.create"})
        return {"variant": variant, "case": case, "error": "no final action"}
    finally:
        w.close()


jobs = [
    (v, c)
    for v in ["baseline", "candidate"]
    for c in options.cases.split(",")
    for _ in range(options.runs)
]


def safe_run(job):
    try:
        return run(job)
    except Exception as error:
        return {"variant": job[0], "case": job[1], "error": type(error).__name__}


with concurrent.futures.ThreadPoolExecutor(3) as ex:
    results = list(ex.map(safe_run, jobs))
options.output.write_text(json.dumps(results, indent=2))
print(
    f"Saved {len(results)} realtime replay results to {options.output}; "
    f"{sum('error' in result for result in results)} transport errors."
)

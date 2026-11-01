#!/usr/bin/env python3
"""Prints a voice-call log (AGENTVIEW_VOICE_LOG) as a conversation with the tool calls between."""
import json, sys

lines = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
speaker, text, started = None, "", 0.0

def flush():
    global speaker, text
    if speaker and text.strip():
        print(f"{started:6.1f}s {speaker}: {text.strip()}")
    speaker, text = None, ""

for entry in lines:
    kind, data, t = entry["kind"], entry["d"], entry["t"]
    if kind == "said":
        if data.get("status") not in (None, "completed"):
            continue  # Grok repeats a transcript while it is still listening
        who = "YOU  " if "input" in data["who"] else "VOICE"
        if who != speaker:
            flush()
            speaker, started = who, t
        text += data["text"]
        continue
    if kind not in ("tool", "end", "send"):
        if kind == "event" and data.get("type") == "error":
            flush()
            print(f"{t:6.1f}s   ERROR {data}")
        continue
    flush()
    if kind == "tool":
        args = json.dumps(data["args"], ensure_ascii=False)
        print(f"{t:6.1f}s   TOOL {data['name']} {args} ({data['ms']} ms) -> {data['result'][:240]}")
    elif kind == "send":
        print(f"{t:6.1f}s   TOLD {data['type']}: {data['content']}")
    elif kind == "end":
        print(f"{t:6.1f}s   END {data['reason']}")
    elif kind == "event" and data.get("type") == "error":
        print(f"{t:6.1f}s   ERROR {data}")
flush()

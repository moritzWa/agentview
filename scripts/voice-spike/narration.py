#!/usr/bin/env python3
"""Live Grok prompt check. Tool calls are stubbed; no real sessions are changed.

Needs websockets and the locally saved xAI key (or XAI_API_KEY). Audio is discarded.
"""
import asyncio
import json
import os
from pathlib import Path

import websockets

root = Path(__file__).resolve().parents[2]
key = os.environ.get("XAI_API_KEY") or json.loads(
    (Path.home() / ".local/state/agentview/voice/key.json").read_text()
)["xai_api_key"]


async def check(request, fail=False, screen=False):
    async with websockets.connect(
        "wss://api.x.ai/v1/realtime?model=grok-voice-latest",
        additional_headers={"Authorization": "Bearer " + key},
        max_size=None,
    ) as ws:
        await ws.recv()
        instructions = (root / "src/voice/xai.md").read_text()
        await ws.send(json.dumps({"type": "session.update", "session": {
            "instructions": instructions + ('\nCurrent screen: s1, titled Polish sentence draft. This overrides earlier screen notes. The user is looking at this chat. Its title may be stale.' if screen else '\nStatus notes: on_screen is s1, titled Voice Mode. s2 is titled Search.'),
            "voice": "rex", "reasoning": {"effort": "none"},
            "turn_detection": None,
            "tools": json.loads((root / "src/voice/tools.json").read_text()),
        }}))
        while True:
            event = json.loads(await asyncio.wait_for(ws.recv(), 20))
            if event["type"] == "error":
                raise AssertionError(event)
            if event["type"] == "session.updated":
                break
        await ws.send(json.dumps({"type": "conversation.item.create", "item": {
            "type": "message", "role": "user",
            "content": [{"type": "input_text", "text": request}],
        }}))
        await ws.send(json.dumps({"type": "response.create"}))
        text, calls, before_read = "", [], ""
        while True:
            event = json.loads(await asyncio.wait_for(ws.recv(), 30))
            if event["type"] == "error":
                raise AssertionError(event)
            if event["type"] == "response.output_audio_transcript.delta":
                text += event["delta"]
            if event["type"] != "response.done":
                continue
            pending = [item for item in event.get("response", {}).get("output", [])
                       if item.get("type") == "function_call"]
            if not pending:
                break
            for item in pending:
                if not calls:
                    before_read = text
                calls.append(item["name"])
                result = {"ok": True}
                if screen and item["name"] == "read_session":
                    result = {"title": "Polish sentence draft", "turns": [{
                        "you": "Explain Interlat's compressed communication between a talker and reasoner.",
                        "it": "The Interlat paper compressed 128 vectors to 8 with about a two-point evaluation loss. This is inter-model latent communication, not document retrieval.",
                    }]}
                await ws.send(json.dumps({"type": "conversation.item.create", "item": {
                    "type": "function_call_output", "call_id": item["call_id"],
                    "output": json.dumps({"error": "session unavailable"} if fail else result),
                }}))
            await ws.send(json.dumps({"type": "response.create"}))
        print(json.dumps({"request": request, "said": text, "tools": calls, "failed_tool": fail}))
        assert calls, "must actually call the tool before claiming success"
        if screen:
            assert calls[0] == "read_session", "read the actual chat before guessing"
            assert len(before_read.split()) <= 1, "do not guess or narrate before reading: " + before_read
            assert any(word in text.lower() for word in ("latent", "interlat", "talker", "reasoner")), text
            assert len(text.split()) <= 50, text
        elif fail:
            assert "unavailable" in text.lower(), "must explain failure rather than say done"
        else:
            assert not before_read.strip(), "routine sends need no preamble: " + before_read
            assert len(text.split()) <= 12, "one short acknowledgment, not a narrated recap"
            words = text.lower().replace(".", "").split()
            assert words.count("done") + words.count("sent") <= 1, "no redundant sent/done after the acknowledgment"


async def main():
    await check('Send "continue" to s1 and "check the search ranking" to s2.')
    await check('Send "continue" to s1.', fail=True)
    await check("It's kinda crazy that they compressed 128 to 8 vectors and only lost two points of the eval.", screen=True)


asyncio.run(asyncio.wait_for(main(), 120))

// Data-level page test: no browser, microphone or speaker is started.
import assert from "node:assert/strict";
import fs from "node:fs";
import vm from "node:vm";

const page = fs.readFileSync(new URL("../../src/voice/page.html", import.meta.url), "utf8");
const source = page.slice(page.indexOf("function openXai(msg)"), page.indexOf("// Grok says a tool's result"));
const sent = [];
const context = vm.createContext({
  WebSocket: class { send(data) { sent.push(JSON.parse(data)); } },
  xws: null, grokReady: false, heardEarly: ["buffered-microphone"],
  setState() {}, post() {}, caption() {}, end() {}, playPcm() {}, stopPlayback() {},
});
vm.runInContext(source, context);
context.openXai({ url: "ws://test", token: "unused", session: { type: "session.update" },
  context: "Current screen: full duplex deep dive. Recent exchanges: Interlat, 128 to 8 vectors." });
context.xws.onopen();
context.xws.onmessage({ data: JSON.stringify({ type: "session.updated" }) });
assert.deepEqual(sent.map(event => event.type), [
  "session.update", "conversation.item.create", "input_audio_buffer.append",
]);
assert.equal(sent[1].item.role, "system");
assert.match(sent[1].item.content[0].text, /Current screen: full duplex/);
assert.equal(sent[2].audio, "buffered-microphone");
context.xws.onmessage({ data: JSON.stringify({ type: "session.updated" }) });
assert.equal(sent.length, 3, "a later speed change must not replay old screen context");
console.log("PASS current-screen context is sent before buffered speech, exactly once per connection");

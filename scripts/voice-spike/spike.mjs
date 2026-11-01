// Protocol spike for Voice Mode: the real page, a stand-in for agentview's voice server, real GPT-Live.
// Needs `ws` and `playwright`: VOICE_E2E_MODULES_FROM is a package.json that has them.
// Headless Chromium with a synthesized microphone file; no sound is played. Run under heavy-job.py.
//   OPENAI_API_KEY=... node scripts/voice-spike/spike.mjs "What sessions need my attention?"
import { createRequire } from "node:module";
import { createServer } from "node:http";
import { readFileSync, writeFileSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";

const require = createRequire(process.env.VOICE_E2E_MODULES_FROM || `${process.env.HOME}/Code/understand/package.json`);
const { WebSocketServer } = require("ws");
const { chromium } = require("playwright");
const root = new URL("../../src/voice/", import.meta.url);
const read = (name) => readFileSync(new URL(name, root), "utf8");
const key = process.env.OPENAI_API_KEY;
if (!key) throw new Error("OPENAI_API_KEY is not set");
const line = process.argv[2] || "Which of my coding sessions need my attention right now?";

// A fake dashboard: the rows the real tool would return.
const sessions = [
  { id: "ses_a", title: "semantic search benchmark improvement", folder: "arca", state: "needs_you", you_last_wrote: "6 h ago", last_said: "I ran the 25 new prompts. Should I rerun the full benchmark with the new retriever?" },
  { id: "ses_b", title: "voice mode agent", folder: "agentview", state: "working", you_last_wrote: "now", last_said: "Writing the voice page." },
  { id: "ses_c", title: "distraction coach feature", folder: "cronus", state: "done", you_last_wrote: "4 h ago", last_said: "The category menu now has three options and the tests pass." },
];
const t0 = Date.now();
const events = [];
const stamp = () => ((Date.now() - t0) / 1000).toFixed(2).padStart(6);
const note = (text) => console.log(`${stamp()}s ${text}`);

function tool(name, args) {
  if (name === "list_sessions") return { sessions, more_parked: 16, parked_needing_you: 0 };
  if (name === "read_session") return { id: args.id, messages: [{ role: "user", text: "Run the benchmark on the 25 new prompts." }, { role: "assistant", text: sessions[0].last_said }] };
  return { error: `${name} is not part of the spike` };
}

const pending = new Map(); // delegation id -> function calls
let sessionId = "";
let sock;
const send = (event) => sock.send(JSON.stringify({ type: "send", event }));

async function createSession(sdp) {
  const body = {
    session: {
      model: "gpt-live-1",
      instructions: read("talker.md") + "\n\nStatus notes:\n3 recent sessions: one needs the developer, one is working, one is done. 16 older ones are parked.",
      audio: { output: { voice: "meridian" } },
      delegation: { type: "responses", responses: { model: "gpt-6-luna", instructions: read("backend.md"), reasoning: { effort: "low" }, tools: JSON.parse(read("tools.json")), tool_choice: "auto" } },
      store: false,
    },
    transport: { type: "webrtc", sdp },
  };
  const started = Date.now();
  const response = await fetch("https://api.openai.com/v1/live/sessions", { method: "POST", headers: { Authorization: `Bearer ${key}`, "Content-Type": "application/json" }, body: JSON.stringify(body) });
  const text = await response.text();
  note(`POST /v1/live/sessions -> ${response.status} in ${Date.now() - started} ms`);
  if (!response.ok) throw new Error(text);
  return JSON.parse(text);
}

function onEvent(event) {
  events.push({ at: Date.now() - t0, event });
  const type = event.type;
  if (type === "session.input_transcript.delta" || type === "session.output_transcript.delta") return;
  if (type === "response.event") {
    const inner = event.event ?? {};
    note(`response.event/${inner.type}`);
    if (inner.type === "response.output_item.done" && inner.item?.type === "function_call") {
      const list = pending.get(event.delegation_id) ?? [];
      list.push(inner.item);
      pending.set(event.delegation_id, list);
      note(`  function_call ${inner.item.name} ${inner.item.arguments}`);
    }
    if (inner.type === "response.completed" || inner.type === "response.done") {
      const calls = pending.get(event.delegation_id);
      if (calls?.length) {
        pending.delete(event.delegation_id);
        for (const call of calls) {
          const result = tool(call.name, JSON.parse(call.arguments || "{}"));
          send({ type: "response.item.create", event_id: `out_${call.call_id}`, item: { type: "function_call_output", call_id: call.call_id, output: JSON.stringify(result) } });
        }
        send({ type: "response.create", event_id: `cont_${Date.now()}` });
        note(`  returned ${calls.length} result(s) and continued`);
      }
    }
    return;
  }
  note(type + (type === "error" ? " " + JSON.stringify(event.error) : ""));
}

// ---- microphone: macOS `say`, converted to WAV, silent lead-in, silence after
const dir = mkdtempSync(join(tmpdir(), "voice-spike-"));
execFileSync("say", ["-v", "Samantha", "-o", join(dir, "speech.aiff"), line]);
execFileSync("ffmpeg", ["-y", "-loglevel", "error", "-i", join(dir, "speech.aiff"), "-af", "adelay=3000|3000,apad=pad_dur=40", "-ar", "48000", "-ac", "1", "-sample_fmt", "s16", join(dir, "mic.wav")]);

// ---- stand-in voice server
const http = createServer((req, res) => {
  if (req.url === "/") { res.writeHead(200, { "content-type": "text/html" }); res.end(read("page.html")); } else { res.writeHead(404); res.end(); }
});
const wss = new WebSocketServer({ server: http, path: "/ws" });
wss.on("connection", (ws) => {
  sock = ws;
  ws.on("message", async (raw) => {
    const msg = JSON.parse(String(raw));
    if (msg.type === "hello") { note("page hello, starting call"); ws.send(JSON.stringify({ type: "start" })); }
    else if (msg.type === "offer") {
      try {
        const made = await createSession(msg.sdp);
        sessionId = made.session?.id;
        ws.send(JSON.stringify({ type: "answer", sdp: made.transport.sdp }));
      } catch (error) { note("session creation failed: " + error.message); finish(1); }
    } else if (msg.type === "event") onEvent(msg.event);
    else if (msg.type === "state") note("peer " + msg.state);
    else if (msg.type === "ended") note("page ended: " + msg.reason);
  });
});
await new Promise((resolve) => http.listen(0, "127.0.0.1", resolve));
const port = http.address().port;

const browser = await chromium.launch({ headless: true, args: [
  "--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream", `--use-file-for-fake-audio-capture=${join(dir, "mic.wav")}%noloop`,
  "--autoplay-policy=no-user-gesture-required", "--mute-audio",
] });
const page = await browser.newPage();
page.on("pageerror", (e) => note("PAGE ERROR " + e.message));

async function memory() {
  const cdp = await page.context().newCDPSession(page);
  const metrics = Object.fromEntries((await cdp.send("Performance.getMetrics")).metrics.map((m) => [m.name, m.value]));
  const rss = execFileSync("ps", ["-A", "-o", "rss=,command="]).toString().split("\n")
    .filter((l) => /chrom/i.test(l) && /ms-playwright/.test(l)).map((l) => parseInt(l.trim(), 10)).filter(Boolean);
  return { jsHeapMB: Math.round(metrics.JSHeapUsedSize / 1e6), chromeProcesses: rss.length, chromeRssMB: Math.round(rss.reduce((a, b) => a + b, 0) / 1024) };
}

let done = false;
async function finish(code) {
  if (done) return; done = true;
  const usage = [...events].reverse().find((e) => e.event.type === "session.usage.updated")?.event.usage;
  const said = events.filter((e) => e.event.type === "session.output_transcript.delta").map((e) => e.event.delta).join("");
  const heard = events.filter((e) => e.event.type === "session.input_transcript.delta").map((e) => e.event.delta).join("");
  console.log(JSON.stringify({ session: sessionId, heard, said, usage, memory: await memory().catch(() => null) }, null, 1));
  writeFileSync(join(dir, "events.json"), JSON.stringify(events));
  note("events saved to " + join(dir, "events.json"));
  try { send({ type: "session.close" }); await new Promise((r) => setTimeout(r, 1500)); } catch {}
  await browser.close(); http.close(); process.exit(code);
}
await page.goto(`http://127.0.0.1:${port}/`);
// finished when the voice has answered after the function call, or after 75 s
const deadline = Date.now() + 75_000;
let lastOutput = 0, seenCall = false;
while (Date.now() < deadline) {
  await new Promise((r) => setTimeout(r, 500));
  seenCall ||= events.some((e) => e.event.type === "response.event" && e.event.event?.item?.type === "function_call");
  const outputs = events.filter((e) => e.event.type === "session.output_transcript.delta");
  if (outputs.length) lastOutput = outputs.at(-1).at;
  if (seenCall && lastOutput && Date.now() - t0 - lastOutput > 6000) break;
}
await finish(seenCall ? 0 : 2);

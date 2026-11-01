// Plays the part of the pinned browser tab: headless Chromium with a synthesized microphone,
// opening the voice page and printing its captions. Run under heavy-job.py.
//   node page-driver.mjs <port> <mic.wav> <seconds>
import { createRequire } from "node:module";
const require = createRequire(process.env.VOICE_E2E_MODULES_FROM || `${process.env.HOME}/Code/understand/package.json`);
const { chromium } = require("playwright");
const [port, wav, seconds] = [process.argv[2], process.argv[3], Number(process.argv[4] || 90)];
const browser = await chromium.launch({ headless: true, args: [
  "--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream", `--use-file-for-fake-audio-capture=${wav}%noloop`,
  ...(process.env.STRICT_AUTOPLAY ? [] : ["--autoplay-policy=no-user-gesture-required"]), "--mute-audio",
] });
const page = await browser.newPage();
page.on("pageerror", (e) => console.log("PAGE ERROR", e.message));
await page.goto(`http://127.0.0.1:${port}/`);
console.log("PAGE READY");
let last = "";
const end = Date.now() + seconds * 1000;
while (Date.now() < end) {
  await new Promise((r) => setTimeout(r, 1000));
  const text = await page.locator("#log").innerText().catch(() => "");
  const state = await page.locator("#state").innerText().catch(() => "");
  if (process.env.STRICT_AUTOPLAY && state.includes("click this page") && !globalThis.clicked) { globalThis.clicked = true; console.log("blocked, clicking"); await page.mouse.click(10, 10); }
  const shown = `[${state}] ${text.replace(/\n+/g, " | ")}`;
  if (shown !== last) { console.log(shown); last = shown; }
}
await browser.close();

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

// Load the Worker as an ES module without imposing a package.json on this repo.
const source = await readFile(new URL("../site-worker.js", import.meta.url), "utf8");
const { default: worker } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);

for (const host of ["agent-view.app", "www.agent-view.app", "www.betteragentview.com"]) {
  test(`${host} redirects permanently, retaining paths and query strings`, async () => {
    const response = await worker.fetch(new Request(`https://${host}/film.mp4?v=2`), {});
    assert.equal(response.status, 308);
    assert.equal(response.headers.get("Location"), "https://betteragentview.com/film.mp4?v=2");
  });
}

test("the canonical domain serves assets rather than redirecting to itself", async () => {
  const request = new Request("https://betteragentview.com/favicon.svg");
  const response = await worker.fetch(request, { ASSETS: { fetch: async (received) => {
    assert.equal(received, request);
    return new Response("asset");
  } } });
  assert.equal(response.status, 200);
  assert.equal(await response.text(), "asset");
});

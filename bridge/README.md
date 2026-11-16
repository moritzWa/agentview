# AgentVoice mobile bridge

ChatGPT can search OpenCode history and queue messages through this private MCP
server. It includes the Cloudflare Worker, OAuth, Neon schemas and sync protocol.
The production connector and login daemon are Rust code in `src/bridge`.

- [Pair the Mac and connect ChatGPT](../docs/mobile-bridge.md)
- [Deploy and preserve the existing service](SETUP.md)
- [Worker authentication and tool contracts](private/README.md)
- [Text-only sync protocol](sync/README.md)
- [Recorded deployment verification](test/e2e/VERIFIED.md)

This is separate from Agent View's built-in OpenAI/Grok voice mode. ChatGPT
mobile Live voice uses the connected AgentVoice MCP app, not Agent View's audio
engine. The legacy VoiceCode app and public synthetic MCP probe remain in
[voizecode](https://github.com/moritzWa/voizecode).

## Local checks

Use Node 24 and run from this directory:

```console
npm ci
npm test
npm run build
```

Tests use synthetic data and local PGlite. The build bundles the Worker without
deploying it. CI runs both without cloud credentials. No workflow deploys on push.
The operator-only scripts in `test/e2e` need prepared cloud/VM resources. They
are not ordinary unit tests and must not run against personal history casually.

## Source relocation

Imported from `moritzWa/voizecode` commit
`0edee06` (Fix MCP paging, matching search excerpts and freshness checks).
Private runtime modules, SQL and `wrangler.private.jsonc` were copied unchanged.
The public probe, old design proposals and unrelated voice app were not imported.
Historical verification records describe earlier runs, not a fresh mobile test.

Moving source does not change the deployed Worker, credentials, OAuth grants,
database, Durable Object or local pairing. See [SETUP.md](SETUP.md) before deploying.

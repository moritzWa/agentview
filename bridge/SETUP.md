# Mobile bridge deployment

## Existing deployment

The repository move needs **no deployment or provisioning**. Keep these identities:

| Resource | Existing value |
| --- | --- |
| Worker name | `agentvoice-private` |
| Issuer | `https://agentvoice-private.moritzw.workers.dev` |
| ChatGPT MCP URL | `https://agentvoice-private.moritzw.workers.dev/mcp` |
| Google callback | `https://agentvoice-private.moritzw.workers.dev/auth/google/callback` |
| Durable Object | `GATE`, class `WakeGate`, migration `v1` |
| Neon organization | AgentVoice Free, `org-purple-unit-21830200` |
| Neon project | `frosty-pond-48075355` |
| Owner | `wallawitsch@gmail.com` |

The Google web client remains the one saved as **AgentVoice Google OAuth** in
1Password's Main vault. The runtime role credential remains **AgentVoice Neon
database**. Do not register a replacement client or reset the database for this
move. Existing OAuth grants and connector credentials remain valid.

Worker secrets are `DATABASE_URL`, `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET`.
They stay in Cloudflare. Neither GitHub Actions nor a source checkout needs them
for local checks. Do not commit credentials, local Wrangler state or journals.

## Later Worker updates

Use Node 24, Cloudflare access to the **existing account**, and this directory:

```console
npm ci
npm test
npm run build
npx wrangler whoami
npx wrangler deployments list --config wrangler.private.jsonc
```

Confirm the account owns the existing Worker before an intentional deployment:

```console
npm run deploy
```

This updates the same Worker, not a new service. Keep `name`, `ISSUER`, the
Durable Object binding/class and migration history unchanged. Do not delete or
recreate the Worker or replace its secrets. No push or PR automatically deploys.

## Provisioning is not migration

`private/provision.mjs` is an operator tool for initial setup or explicit schema
maintenance. It applies both SQL schemas, creates or reuses `agentvoice_runtime`,
grants runtime access and writes the three Worker secrets. It requires `neonctl`,
`op`, Cloudflare authentication and the dedicated Google OAuth item. Its macOS
credential path uses the `op-service-account` Keychain item and Homebrew `op`.
Read the script before running it. Do not run it to relocate this repository.

For a separate installation, create a dedicated Neon project and Google web
client, configure the intended owner in `private/auth.mjs`, and change the Worker
name and issuer to your own URL. Register that URL's Google callback. The current
provisioner uses the named 1Password items above, so adapt those names rather than
overwriting an existing installation's secrets. Run from `bridge`:

```console
NEON_PROJECT_ID=<dedicated-project> node private/provision.mjs
npm run deploy
```

The service is single-owner, not a public signup product. Do not point a new
installation at the existing owner's database.

## Operator verification

The `test/e2e` scripts retain their original prepared-VM paths and deployment
targets. `cloud-vm.mjs` also loads Playwright from the operator's Understand
checkout. Inspect and adapt these paths before reuse. `tool-contracts.mjs` writes
disposable synthetic fixtures to the deployed Neon database and removes them on
exit. It is not read-only. None of these scripts runs in CI.

Existing verification and limitations are in [VERIFIED.md](test/e2e/VERIFIED.md).
Maintenance remains tracked in
[voizecode issue #18 (credential renewal, retention and oversized future messages)](https://github.com/moritzWa/voizecode/issues/18).
Its earlier iPhone rollout-gate wording predates the confirmed rollout.

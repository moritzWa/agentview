# BetterAgentView rename

The public name is **BetterAgentView**, the repository is
`moritzWa/betteragentview`, and the website is `https://betteragentview.com`.
The old GitHub repository redirects to the renamed repository.

## Existing installations

- `betteragentview` is the new command. `av` and `agentview` still work.
- Configuration, state directories, sockets, daemon labels, `AGENTVIEW_*`
  variables and the Rust library name `agentview` stay unchanged. Renaming
  these would break installed integrations or split existing session state.
- The legacy `agentview --version` response stays machine-compatible.
  `betteragentview --version` reports the new command name.
- Release archive names retain their `agentview-` prefix so older installers
  can update. Archives include both commands and `av`.
- Local checkouts need not move. Git remotes can point to
  `https://github.com/moritzWa/betteragentview.git`.

## Website

Cloudflare Registrar holds `betteragentview.com` in the existing personal
account. It was registered on November 20, 2026 for $11.17 for one year, with
auto-renewal enabled at $11.17/year and WHOIS privacy enabled.

The existing Cloudflare Worker remains named `agentview` to preserve its
deployment identity. `site-worker.js` redirects `agent-view.app`,
`www.agent-view.app` and `www.betteragentview.com` to `betteragentview.com`
with status 308, preserving the path and query string. The canonical host
serves static assets directly. Keep the old domain registered for redirects.

Deploy with `wrangler deploy` from the repository root after merging the site
and film updates. Check all four hostnames, a blog permalink and `/film.mp4`.
Redirect tests run with `node --test scripts/test-site-rename.mjs`.

Existing blog slugs stay unchanged so published links keep working.

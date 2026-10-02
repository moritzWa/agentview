<div align="center">

<h1><img src="docs/assets/logo.svg" alt="agentview logo" width="40" height="40"> agentview</h1>

**All your coding agents. One terminal.**<br>
See which one needs you. Jump in, jump back out. Works with 18 coding
harnesses and plain shell jobs.

[![Tests](https://img.shields.io/badge/tests-verified-2ea44f.svg)](docs/testing.md)
[![Release](https://img.shields.io/badge/release-v0.1.0-7c5cff.svg)](https://github.com/moritzWa/agentview/releases/latest)
[![License](https://img.shields.io/badge/license-MIT-7c5cff.svg)](LICENSE)

<img src="docs/assets/demo.gif" alt="agentview demo: browsing sessions, inspecting one, and starting a new task" width="800">

</div>

Install agentview on macOS or Linux:

```console
curl -fsSL https://raw.githubusercontent.com/moritzWa/agentview/main/install.sh | bash
```

<details>
<summary>Windows PowerShell</summary>

```powershell
irm https://raw.githubusercontent.com/moritzWa/agentview/main/install.ps1 | iex
```

</details>

<details>
<summary>Build from source</summary>

```console
cargo install --locked --git https://github.com/moritzWa/agentview
```

</details>

Launch the dashboard:

```console
agentview
```

The installer also adds the shorter `av` command. Start typing a task, press
`Tab` to choose a harness, and press `Shift+Tab` to choose one of that account's
available models.

The dashboard follows the terminal's light or dark background, then the
operating system appearance, and switches live when the system appearance
changes. Force one with `--theme light` or `--theme dark`.

## Why agentview?

Five agents means five terminal tabs. The one waiting on you is always in the
tab you aren't looking at. agentview lists every session from every harness in
one place, with the ones that need you on top. The conversation still lives in
the harness that created it; selecting a row opens that harness's native
interface.

## Features

New in agentview, compared with
[Open Agent View](https://github.com/xhluca/open-agent-view), which it started from:

- **Start any recent session.** `Ctrl+G` searches older sessions the dashboard
  does not list (OpenCode today) and ones you hid, by name, harness, folder, or
  ID. Pick one and it is back on the list.
- **Launches stay on the dashboard.** Harnesses that can run in the background
  start there; the new row is selected and `Enter` opens it.
- **Pin and reorder.** `Ctrl+T` pins a session to the top; `Option+↑` / `↓`
  moves it within its group.
- **Start in any folder.** `/cd` or `Ctrl+O` picks the working directory for a
  new task. Tasks can span several lines, and a multi-line paste becomes one
  draft instead of one launch per line.
- **OpenCode that outlives the dashboard.** Sessions keep running after you
  quit, and new ones get OpenCode's generated titles.
- **Cursor everywhere.** Launch Cursor chats and see their live state on every
  platform, including chats started outside agentview.
- **Remembers where you were.** Opens on the last view, with the last harness
  you launched selected.
- **Light and dark themes** that follow the terminal and the OS, live.
- **Ctrl+X twice** deletes or hides a session, with no confirmation dialog.

From Open Agent View:

- **Know where to look.** Sessions are grouped as waiting for input, working,
  completed, or unknown, with the harness shown on every row.
- **Return without killing the task.** Open a native session, then move back to
  the dashboard while its work continues.
- **Migrate between harnesses.** `Ctrl+M` moves a conversation to another
  harness and keeps going.
- **Stay fast as the list grows.** Discovery runs concurrently and the TUI only
  renders the page that fits the terminal.
- **Use controls agentview can prove.** Stop, reply, archive, and delete are
  offered only when the selected provider and session support them safely.

## The everyday workflow

| Do this | In the dashboard |
| --- | --- |
| Move through sessions | `↑` / `↓` |
| Open the selected native session | `Enter` or `→` |
| Return to agentview | `Shift+←`, or `←` twice at an empty prompt |
| Rename a session in agentview | `Ctrl+R` |
| Migrate a session to another harness | `Ctrl+M` |
| Filter the session list | `Ctrl+F` |
| Add a line to a new task | `Shift+Enter` (or `Ctrl+J`) |
| Bring back a hidden or older session | `Ctrl+G` (or `/hidden`) |
| Stop, then delete or hide a managed session | `Ctrl+X`, then `Ctrl+X` again |
| See the complete contextual key map | `?` |

See the [CLI and keyboard guide](docs/cli.md) for model selection, login/setup,
completed-session visibility, paging, bulk actions, and non-interactive CLI
commands.

For deliberately unattended work, `av --yolo` maps to a verified native
permission-bypass mode on supported harnesses, stays visibly marked, and fails
closed everywhere else. It is off by default; see the
[security and provider mapping](docs/cli.md#explicit-yolo-mode) before using it.

`Ctrl+M` opens a destination picker, then a name editor prefilled with the
current name plus the destination harness. agentview delegates the conversion to
[session-migrate](https://session-migrate.github.io/), keeps the imported
session visible, and stores the chosen name only as a private agentview display name.

Install the companion CLI once with
`curl -LsSf https://session-migrate.github.io/install.sh | sh`.

## Harnesses

agentview brings 18 local coding harnesses plus Terminal into one dashboard,
listed in the in-app picker's order: [Claude Code](https://github.com/anthropics/claude-code), [OpenAI Codex](https://github.com/openai/codex), [Pi](https://pi.dev), [OpenCode](https://github.com/anomalyco/opencode), [Cursor](https://cursor.com/cli), [GitHub Copilot](https://github.com/github/copilot-cli), [Antigravity](https://developers.google.com/antigravity), [Mistral Vibe](https://github.com/mistralai/mistral-vibe), [Muse Code](https://dev.meta.ai/), [Qwen Code](https://github.com/QwenLM/qwen-code), [Kimi Code](https://github.com/MoonshotAI/kimi-cli), [Oh My Pi](https://github.com/can1357/oh-my-pi), [Grok](https://github.com/xai-org/grok-build), [Kilo Code](https://github.com/Kilo-Org/kilocode), [OpenHands](https://github.com/OpenHands/OpenHands-CLI), [Hermes Agent](https://github.com/NousResearch/hermes-agent), [MastraCode](https://github.com/mastra-ai/mastra/tree/main/mastracode), [Devin](https://github.com/CognitionAI/devin-cli), [Terminal](docs/cli.md).

<details>
<summary><strong>Compare feature support by harness</strong></summary>

| Harness | Launch | Model / shell picker | Open / resume | Inspect | Inline reply | Approval / input | Stop | Delete / archive |
| --- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| Claude Code | ✓ | ✓ | ✓ | ✓ | — | — | ✓ | — |
| OpenAI Codex | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Pi | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| OpenCode | ✓ | ✓ | ✓ | ✓ | ✓ | — | ✓ | — |
| Cursor | ✓ | ✓ | ✓ | ✓ | ✓ | — | ✓ | — |
| GitHub Copilot | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — |
| Antigravity | ✓ | ✓ | ✓ | — | — | — | ✓ | — |
| Mistral Vibe | ✓ | ✓ | ✓ | ✓ | — | — | ✓ | — |
| Muse Code | ✓ | ✓ | ✓ | ✓ | — | — | ✓ | — |
| Qwen Code | ✓ | ✓ | ✓ | ✓ | — | — | ✓ | — |
| Kimi Code | ✓ | ✓ | ✓ | ✓ | — | — | ✓ | — |
| Oh My Pi | ✓ | ✓ | ✓ | ✓ | — | — | ✓ | — |
| Grok | ✓ | ✓ | ✓ | ✓ | — | — | ✓ | — |
| Kilo Code | ✓ | ✓ | ✓ | ✓ | — | — | ✓ | — |
| OpenHands | ✓ | ✓¹ | ✓ | ✓ | — | — | ✓ | — |
| Hermes Agent | ✓² | ✓¹ | ✓² | ✓ | — | — | ✓² | — |
| MastraCode | ✓² | ✓¹ | ✓² | ✓ | — | — | ✓² | — |
| Devin | ✓ | ✓ | ✓ | ✓ | — | — | ✓ | — |
| Terminal | ✓ | ✓ | ✓ | ✓ | — | — | ✓ | ✓ |

`✓` means agentview exposes the feature for sessions it owns. A dash means the
session still appears in the dashboard, but that action stays in the harness's
native interface. “Delete / archive” is checked when at least one safe removal
operation is available.

¹ Hermes and MastraCode offer models seen in saved sessions and accept exact
model IDs; their native setup handles provider configuration. OpenHands reads
model choices from its saved configurations and `LLM_MODEL`, and also accepts
an exact model ID.

² Hermes and MastraCode foreground prompt automation requires a Unix terminal;
their saved sessions can also be inspected on Windows. See the
[integration notes](docs/exploration/shared-sqlite-harnesses.md) for native
version coverage and model-picker limits.

</details>

Exact CLI versions, model discovery, authentication behavior, platform limits,
and provider-specific caveats live in the [provider notes](docs/exploration/README.md).

## Documentation

- [Install, update, and uninstall](docs/install.md)
- [CLI and keyboard reference](docs/cli.md)
- [Troubleshooting and recovery](docs/troubleshooting.md)
- [Architecture](docs/architecture.md)
- [Testing and real-TTY evidence](docs/testing.md)
- [Documentation index](docs/README.md)

Contributions are welcome through [CONTRIBUTING.md](CONTRIBUTING.md). Report
security-sensitive findings through [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE). agentview is independent and is not affiliated with or
endorsed by the providers or CLI projects listed above.

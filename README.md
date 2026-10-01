<div align="center">

<h1><img src="docs/assets/logo.svg" alt="agentview logo" width="40" height="40"> agentview</h1>

**Every coding agent you have running, in one terminal view.**<br>
See what's working, what needs you, and jump straight into it, across 18 coding
harnesses plus Terminal.

[![Tests](https://img.shields.io/badge/tests-verified-2ea44f.svg)](docs/testing.md)
[![Release](https://img.shields.io/badge/release-v0.1.0-7c5cff.svg)](https://github.com/moritzWa/agentview/releases/latest)
[![License](https://img.shields.io/badge/license-MIT-7c5cff.svg)](LICENSE)

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

Running several coding agents at once turns into a pile of terminal tabs, and
the one that is stuck waiting for you is always in the tab you are not looking
at. agentview puts every session from every harness in one list, sorted by what
needs you.

agentview brings Claude Code, Codex, Pi, OpenCode, Cursor, GitHub Copilot,
Antigravity, Mistral Vibe, Muse Code, Qwen Code, Kimi Code, Oh My Pi, Grok,
Kilo Code, OpenHands, Hermes Agent, MastraCode, Devin, and ordinary terminal jobs into one dashboard.
The conversation still lives in the harness that created it; selecting a row
opens that harness's native interface.

- **Know where to look.** Sessions are grouped as waiting for input, working,
  completed, or unknown, with the harness shown on every row.
- **Return without killing the task.** Open a native session, then move back to
  the dashboard while its work continues.
- **Arrange the list your way.** Pin sessions to the top, reorder them, and
  hide the ones you are done with.
- **Stay fast as the list grows.** Discovery runs concurrently and the TUI only
  renders the page that fits the terminal.
- **Use controls agentview can prove.** Stop, reply, archive, and delete are offered
  only when the selected provider and session support them safely.

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

agentview brings 18 local coding harnesses plus Terminal into one
dashboard. This inventory follows the same order as the in-app harness picker.

<table>
  <tr>
    <td align="center" width="25%"><a href="https://github.com/anthropics/claude-code"><img src="docs/assets/providers/claude.svg" width="64" height="64" alt="Claude Code logo"><br><strong>Claude Code</strong></a></td>
    <td align="center" width="25%"><a href="https://github.com/openai/codex"><img src="docs/assets/providers/codex.png" width="64" height="64" alt="OpenAI Codex logo"><br><strong>OpenAI Codex</strong></a></td>
    <td align="center" width="25%"><a href="https://pi.dev"><img src="docs/assets/providers/pi.svg" width="64" height="64" alt="Pi logo"><br><strong>Pi</strong></a></td>
    <td align="center" width="25%"><a href="https://github.com/anomalyco/opencode"><img src="docs/assets/providers/opencode.svg" width="64" height="64" alt="OpenCode logo"><br><strong>OpenCode</strong></a></td>
  </tr>
  <tr>
    <td align="center"><a href="https://cursor.com/cli"><img src="docs/assets/providers/cursor.svg" width="64" height="64" alt="Cursor logo"><br><strong>Cursor</strong></a></td>
    <td align="center"><a href="https://github.com/github/copilot-cli"><img src="docs/assets/providers/copilot.svg" width="64" height="64" alt="GitHub Copilot logo"><br><strong>GitHub Copilot</strong></a></td>
    <td align="center"><a href="https://developers.google.com/antigravity"><img src="docs/assets/providers/antigravity.svg" width="64" height="64" alt="Antigravity logo"><br><strong>Antigravity</strong></a></td>
    <td align="center"><a href="https://github.com/mistralai/mistral-vibe"><img src="docs/assets/providers/mistral-vibe.svg" width="64" height="64" alt="Mistral Vibe logo"><br><strong>Mistral Vibe</strong></a></td>
  </tr>
  <tr>
    <td align="center"><a href="https://dev.meta.ai/"><img src="docs/assets/providers/muse.svg" width="64" height="64" alt="Muse Code logo"><br><strong>Muse Code</strong></a></td>
    <td align="center"><a href="https://github.com/QwenLM/qwen-code"><img src="docs/assets/providers/qwen.svg" width="64" height="64" alt="Qwen Code logo"><br><strong>Qwen Code</strong></a></td>
    <td align="center"><a href="https://github.com/MoonshotAI/kimi-cli"><img src="docs/assets/providers/kimi-code.svg" width="64" height="64" alt="Kimi Code logo"><br><strong>Kimi Code</strong></a></td>
    <td align="center"><a href="https://github.com/can1357/oh-my-pi"><img src="docs/assets/providers/oh-my-pi.svg" width="64" height="64" alt="Oh My Pi logo"><br><strong>Oh My Pi</strong></a></td>
  </tr>
  <tr>
    <td align="center"><a href="https://github.com/xai-org/grok-build"><img src="docs/assets/providers/grok.svg" width="64" height="64" alt="Grok logo"><br><strong>Grok</strong></a></td>
    <td align="center"><a href="https://github.com/Kilo-Org/kilocode"><img src="docs/assets/providers/kilo-code.svg" width="64" height="64" alt="Kilo Code logo"><br><strong>Kilo Code</strong></a></td>
    <td align="center"><a href="https://github.com/OpenHands/OpenHands-CLI"><img src="docs/assets/providers/openhands.svg" width="64" height="64" alt="OpenHands logo"><br><strong>OpenHands</strong></a></td>
    <td align="center"><a href="https://github.com/NousResearch/hermes-agent"><strong>Hermes Agent</strong></a></td>
  </tr>
  <tr>
    <td align="center"><a href="https://github.com/mastra-ai/mastra/tree/main/mastracode"><strong>MastraCode</strong></a></td>
    <td align="center"><a href="https://github.com/CognitionAI/devin-cli"><strong>Devin</strong></a></td>
    <td align="center"><a href="docs/cli.md"><img src="docs/assets/providers/terminal.svg" width="64" height="64" alt="Terminal icon"><br><strong>Terminal</strong></a></td>
  </tr>
</table>

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

² Hermes and MastraCode foreground prompt automation requires a Unix terminal;
their saved sessions can also be inspected on Windows. See the
[integration notes](docs/exploration/shared-sqlite-harnesses.md) for native
version coverage and model-picker limits.

¹ Hermes and MastraCode offer models seen in saved sessions and accept exact
model IDs. Their native setup handles provider configuration. OpenHands model choices are read from its saved configurations and
`LLM_MODEL`; an exact model ID can also be entered directly.

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

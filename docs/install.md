# Installation

agentview installs one canonical executable named `agentview`. It
also creates `av` as its short command. The normal installation downloads a verified
prebuilt binary: Rust and Cargo are not user prerequisites. An unrelated
existing command at the alias path is never overwritten.

## Supported platforms

Releases cover:

- Linux x86_64 with glibc 2.35 or newer (Debian 12, Ubuntu 22.04+, and similar)
- Linux ARM64 with glibc 2.35 or newer
- macOS Apple silicon
- macOS Intel
- Windows x64

The PowerShell installer selects the `x86_64-pc-windows-msvc` archive,
verifies SHA-256, installs `agentview.exe` and `av.exe` without administrator privileges, and adds
the user-local directory to `PATH`.

Apple-silicon installation is exercised natively. The Intel archive is built
for `x86_64-apple-darwin`, exercised through Rosetta, and independently built
and tested on the native Intel macOS CI runner. The installer selects the
archive from `uname` and never reuses a Linux binary on macOS.

The dashboard needs an interactive terminal. `--json` works without a TTY.
All provider CLIs and Docker are optional; install only the providers you
intend to supervise.

## Install a missing coding-agent harness

agentview can stage a provider's official user-local installer, show its
native download/install progress, and run it only after confirmation:

```console
agentview setup claude
agentview setup codex
agentview setup pi
agentview setup opencode
agentview setup cursor
agentview setup copilot
agentview setup antigravity
agentview setup mistral-vibe
agentview setup muse
agentview setup qwen
agentview setup kimi
agentview setup omp
agentview setup grok
agentview setup kilo
agentview setup openhands
```

For a non-interactive script, review the named source first and add `--yes`.
Official shell installers are downloaded to a private temporary file and then
executed; agentview does not pipe a network response directly into a shell. Codex and
Pi use their official npm packages. A failed download/install leaves the
existing agentview binary and provider state alone. Oh My Pi, Grok, and OpenHands use
their official shell installers; Kilo Code uses `@kilocode/cli` from npm.
Restart `agentview` after a
new harness is installed, select it with Tab, and use Shift+Tab for models.
When authentication is required, Enter in the model picker hands the terminal
to the provider's native login and reloads the account catalog afterward. From
the dashboard, `/setup HARNESS` runs the same installation/login sequence in a
private terminal; the native boundary-double-arrow or Shift+Arrow gesture
backgrounds it as a Terminal job and Enter resumes that exact screen. This
prevents setup from inheriting the last opened agent UI.

## One-line installation

Install the latest published release on macOS or Linux:

```console
curl -fsSL \
  https://raw.githubusercontent.com/moritzWa/agentview/main/install.sh | bash
```

On Windows, run this in PowerShell:

```powershell
irm https://raw.githubusercontent.com/moritzWa/agentview/main/install.ps1 | iex
```

Native Windows opens provider CLIs in the foreground and returns to the
dashboard when their native process exits. Durable Unix-socket supervision and
the Shift+Arrow background gesture remain available through WSL 2; Windows
ConPTY background/resume is not claimed yet. Session discovery, filtering,
renaming, model selection, provider login handoff, foreground launch, JSON
output, and the built-in PowerShell/Command Prompt terminal picker run natively.

The Unix installer writes `~/.local/bin/agentview`, creates an `av`
symlink. The Windows installer writes the equivalent two executable names to
`%LOCALAPPDATA%\Programs\agentview\bin` and adds that directory to the user
`PATH`. It never requires administrator privileges or edits a PowerShell
profile.

Install a specific version or location with script arguments:

```console
curl -fsSL \
  https://raw.githubusercontent.com/moritzWa/agentview/main/install.sh |
  bash -s -- --version MAJOR.MINOR.PATCH --install-dir /absolute/bin
```

The Unix installer requires `curl`, `tar`, `install`, `ln`, `readlink`, and either `sha256sum` or
`shasum`. Run `./install.sh --help` for all arguments and environment variables.
The Windows installer requires Windows PowerShell 5.1 or PowerShell 7 and uses
only built-in archive and checksum commands.

## Verify the installation

Start with checks that do not contact an agent provider:

```console
agentview --version
av --version
agentview --help
agentview --json --no-host-providers
```

The JSON command should report empty `sessions` and `warnings` arrays. It does
not start the TUI, a provider, Docker, or the durable Codex supervisor.

Then inspect the providers installed on this machine:

```console
agentview doctor
agentview
```

Missing optional providers are warnings. See [troubleshooting](troubleshooting.md)
for provider-specific checks and [TUI validation](tui-validation.md) for a full
interactive test.

## Upgrade

Use the installed shorthand:

```console
av update
# or: av upgrade
```

The updater downloads this repository's installer, which resolves the latest
published release, verifies its SHA-256 checksum, and stages the new binary
before replacement. Existing provider sessions and agentview state are
not removed. Its final line reports the verified old-to-new version transition,
or says that the installed version is already current. Re-running the
installation command above is equivalent.

Pin `--version` in automation; the default `latest` channel can change whenever
a new stable release is published.

## Uninstall

Remove the executable installed at `~/.local/bin/agentview`, its `av`
symlink, or their equivalents
under the custom path passed to `--install-dir`.

Uninstalling does not stop or delete provider sessions, containers,
bind-mounted workspaces, state homes, or authority records. See the
[control model](control-model.md) before removing state manually.

## Build from source

Source builds are for contributors and unsupported platforms, not the normal
installation path. They require Rust 1.75 or newer and an authorized checkout:

```console
cargo test --locked
cargo install --path . --locked --root "$HOME/.local"
```

See [`CONTRIBUTING.md`](../CONTRIBUTING.md) for the development workflow and the
[release guide](release.md) for packaging details.

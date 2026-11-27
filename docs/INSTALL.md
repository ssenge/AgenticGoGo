# Installing `rlt`

`rlt` has two parts that install separately:

1. **The `rlt` binary** — the loop engine, on your shell `PATH`.
2. **The `/rlt:*` skills** — a plugin (a plugin can't put a binary on your PATH, so this is a
   separate step). They work on **all three agents**.

## Requirements

`rlt` drives **one coding agent**, chosen by `agent:` in `rlt.yaml`. Whichever you pick must be on
your `PATH`:

| `agent:` | CLI | install + authenticate | check it works headlessly |
|---|---|---|---|
| `claude` *(default)* | [Claude Code](https://claude.com/claude-code) | `npm i -g @anthropic-ai/claude-code` · `claude auth login` | `claude -p "hello"` |
| `codex` | [OpenAI Codex](https://developers.openai.com/codex/cli) | `npm i -g @openai/codex` · `codex login` | `codex exec "hello"` |
| `copilot` | [GitHub Copilot CLI](https://github.com/github/copilot-cli) | `npm i -g @github/copilot` · `copilot login` | `copilot -p "hello"` |

`rlt` only ever drives the agent **headlessly**, so the check column is the one that matters — a
version number or a login status does not tell you the thing `rlt` depends on.

Your existing plugins, MCP servers and settings for that agent keep working inside the `RUN` stage.

`rlt` sets **no credentials of its own** — it just launches your agent headlessly (`claude -p`,
`codex exec`, `copilot -p`), inheriting whatever auth that CLI already uses. So a subscription or an
API key both work; the only requirement is that the agent runs in your environment.

Agents are **not interchangeable** — notably, only Claude reports a dollar cost. `rlt doctor` tells
you whether your chosen agent can do what your config asks. See
[Choosing an agent](../README.md#choosing-an-agent).

## 1 — The binary

```bash
# A) one-liner — detects OS/arch; installs to /usr/local/bin, or ~/.local/bin if that's read-only
curl -fsSL https://raw.githubusercontent.com/ssenge/relentless/main/scripts/install.sh | sh

# B) prebuilt binary — grab it from Releases (on Windows, take the .exe)
#    https://github.com/ssenge/relentless/releases

# C) from source — needs the Rust toolchain
cargo build --release && sudo cp target/release/rlt /usr/local/bin/
```

Options for the one-liner:

- `RLT_VERSION=v0.0.11` — pin a specific version (default: latest release).
- `RLT_INSTALL_DIR=~/bin` — choose the install directory. Without it, the installer upgrades the `rlt`
  first on your `PATH` in place when you can write there (otherwise `/usr/local/bin`, then `~/.local/bin`), so an
  older copy earlier on `PATH` doesn't keep winning. It then lists every other `rlt` on `PATH` with its version, prints
  a WARNING with the `cp` or `rm` that fixes it for each one older than the copy just installed (it never changes
  them itself), and says which copy `rlt` resolves to.
- `RLT_DOWNLOAD_URL=<url>` — download the binary from this URL (a `file://` URL works) instead of GitHub's.

Verify:

```bash
rlt --version
rlt doctor      # checks your agent is on PATH, that it can do what your config asks,
                # that the config parses, the conditions are valid, and the skills are installed
```

## 2 — The `/rlt:*` skills

Three skills: **`rlt-new`** (set up the loop for a project), **`rlt-status`** (check on a run) and
**`rlt-supervise`** (steer a run from a second session). Two ways to get them, both working on all
three agents.

### Route A — the plugin marketplace

All three agents accept the **same** marketplace, so there is one plugin, not three.

```
# Claude Code — inside a session
/plugin marketplace add ssenge/relentless
/plugin install rlt@relentless
```
```bash
# Claude Code — non-interactively
claude plugin marketplace add ssenge/relentless
claude plugin install rlt@relentless --scope user
```
```bash
# OpenAI Codex   (needs the full URL)
codex plugin marketplace add https://github.com/ssenge/relentless
codex plugin add rlt@relentless
```
```bash
# GitHub Copilot
copilot plugin marketplace add ssenge/relentless
copilot plugin install rlt@relentless
```

### Route B — install into a project, with the binary you just installed

```bash
rlt skills install                  # for the agent named in rlt.yaml (default: claude)
rlt skills install --agent codex    # or name one explicitly
rlt skills install --user           # account-wide, under $HOME
```

It writes to wherever that agent actually looks: `.claude/skills/` for Claude, `.agents/skills/` for
Codex and Copilot (the agent-neutral convention both honour).

### Invoking them

The prefix is **not** the same on each agent:

| agent | invoke with |
|---|---|
| Claude Code | `/rlt:new` — or `/rlt-new` if you used Route B (the `rlt:` namespace comes from the plugin) |
| GitHub Copilot | `/rlt-new` |
| OpenAI Codex | **`$rlt-new`** — Codex uses `$`, not `/`. (`/skills` opens a picker.) |

On any of them you can also just **ask** ("set up Relentless for this project"): every agent selects
a skill by matching your request against its description.

## Platform notes

`rlt` is **unix-first** (macOS + Linux). The Windows binary builds and the core outer loop runs, but
two safety features are not implemented there — see the platform note in
[`docs/CONFIG.md`](CONFIG.md).

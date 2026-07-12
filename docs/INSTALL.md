# Installing `agg`

`agg` has two parts that install separately:

1. **The `agg` binary** — the loop engine, on your shell `PATH`.
2. **The `/agg:*` skills** — a plugin (a plugin can't put a binary on your PATH, so this is a
   separate step). They work on **all three agents**.

## Requirements

`agg` drives **one coding agent**, chosen by `agent:` in `agg.yaml`. Whichever you pick must be on
your `PATH`:

| `agent:` | CLI | install + authenticate | check it works headlessly |
|---|---|---|---|
| `claude` *(default)* | [Claude Code](https://claude.com/claude-code) | `npm i -g @anthropic-ai/claude-code` · `claude auth login` | `claude -p "hello"` |
| `codex` | [OpenAI Codex](https://developers.openai.com/codex/cli) | `npm i -g @openai/codex` · `codex login` | `codex exec "hello"` |
| `copilot` | [GitHub Copilot CLI](https://github.com/github/copilot-cli) | `npm i -g @github/copilot` · `copilot login` | `copilot -p "hello"` |

`agg` only ever drives the agent **headlessly**, so the check column is the one that matters — a
version number or a login status does not tell you the thing `agg` depends on.

Your existing plugins, MCP servers and settings for that agent keep working inside the `RUN` stage.

`agg` sets **no credentials of its own** — it just launches your agent headlessly (`claude -p`,
`codex exec`, `copilot -p`), inheriting whatever auth that CLI already uses. So a subscription or an
API key both work; the only requirement is that the agent runs in your environment.

Agents are **not interchangeable** — notably, only Claude reports a dollar cost. `agg doctor` tells
you whether your chosen agent can do what your config asks. See
[Choosing an agent](../README.md#choosing-an-agent).

## 1 — The binary

```bash
# A) one-liner — detects OS/arch; installs to /usr/local/bin, or ~/.local/bin if that's read-only
curl -fsSL https://raw.githubusercontent.com/ssenge/AgenticGoGo/main/scripts/install.sh | sh

# B) prebuilt binary — grab it from Releases (on Windows, take the .exe)
#    https://github.com/ssenge/AgenticGoGo/releases

# C) from source — needs the Rust toolchain
cargo build --release && sudo cp target/release/agg /usr/local/bin/
```

Options for the one-liner:

- `AGG_VERSION=v0.0.11` — pin a specific version (default: latest release).
- `AGG_INSTALL_DIR=~/bin` — choose the install directory.

Verify:

```bash
agg --version
agg doctor      # checks your agent is on PATH, that it can do what your config asks,
                # that the config parses, the conditions are valid, and the skills are installed
```

## 2 — The `/agg:*` skills

Three skills: **`agg-new`** (set up the loop for a project), **`agg-status`** (check on a run) and
**`agg-supervise`** (steer a run from a second session). Two ways to get them, both working on all
three agents.

### Route A — the plugin marketplace

All three agents accept the **same** marketplace, so there is one plugin, not three.

```
# Claude Code — inside a session
/plugin marketplace add ssenge/AgenticGoGo
/plugin install agg@agenticgogo
```
```bash
# Claude Code — non-interactively
claude plugin marketplace add ssenge/AgenticGoGo
claude plugin install agg@agenticgogo --scope user
```
```bash
# OpenAI Codex   (needs the full URL)
codex plugin marketplace add https://github.com/ssenge/AgenticGoGo
codex plugin add agg@agenticgogo
```
```bash
# GitHub Copilot
copilot plugin marketplace add ssenge/AgenticGoGo
copilot plugin install agg@agenticgogo
```

### Route B — install into a project, with the binary you just installed

```bash
agg skills install                  # for the agent named in agg.yaml (default: claude)
agg skills install --agent codex    # or name one explicitly
agg skills install --user           # account-wide, under $HOME
```

It writes to wherever that agent actually looks: `.claude/skills/` for Claude, `.agents/skills/` for
Codex and Copilot (the agent-neutral convention both honour).

### Invoking them

The prefix is **not** the same on each agent:

| agent | invoke with |
|---|---|
| Claude Code | `/agg:new` — or `/agg-new` if you used Route B (the `agg:` namespace comes from the plugin) |
| GitHub Copilot | `/agg-new` |
| OpenAI Codex | **`$agg-new`** — Codex uses `$`, not `/`. (`/skills` opens a picker.) |

On any of them you can also just **ask** ("set up AgenticGoGo for this project"): every agent selects
a skill by matching your request against its description.

## Platform notes

`agg` is **unix-first** (macOS + Linux). The Windows binary builds and the core outer loop runs, but
two safety features are not implemented there — see the platform note in
[`docs/CONFIG.md`](CONFIG.md).

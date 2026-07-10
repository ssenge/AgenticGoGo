# Installing `agg`

`agg` has two parts that install separately:

1. **The `agg` binary** — the loop engine, on your shell `PATH`.
2. **The `/agg:*` Claude Code skills** — a plugin (a plugin can't put a binary on your PATH, so this is a separate step).

## Requirements

`agg` drives the [Claude Code](https://claude.com/claude-code) CLI, which must be on your `PATH`.
Your other Claude Code plugins and MCP servers keep working inside the `RUN` stage.

`agg` sets **no credentials of its own** — it just runs `claude -p`, inheriting whatever auth your
`claude` CLI already uses. So it works with a **Claude subscription (Pro/Max)** *or* an
`ANTHROPIC_API_KEY`. The only requirement is that `claude -p` runs in your environment.

## 1 — The binary

```bash
# A) one-liner — detects OS/arch; installs to /usr/local/bin, or ~/.local/bin if that's read-only
curl -fsSL https://raw.githubusercontent.com/ssenge/AgenticGoGo/main/install.sh | sh

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
agg doctor      # checks claude is on PATH, config parses, conditions are valid
```

## 2 — The `/agg:*` skills

Inside Claude Code:

```
/plugin marketplace add ssenge/AgenticGoGo
/plugin install agg@agenticgogo
```

…or non-interactively, from a terminal:

```bash
claude plugin marketplace add ssenge/AgenticGoGo
claude plugin install agg@agenticgogo --scope user
```

This gives you `/agg:new` (set up the loop for a project), `/agg:status` (check on a run), and
`/agg:supervise` (steer a run from a second session — see the README).

## Platform notes

`agg` is **unix-first** (macOS + Linux). The Windows binary builds and the core outer loop runs, but
two safety features are not implemented there — see the platform note in
[`docs/CONFIG.md`](CONFIG.md).

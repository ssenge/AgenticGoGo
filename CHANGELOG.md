# Changelog

Notable changes to agg. Newest first; each entry names the config keys and interfaces it touches.

## Unreleased

### Added

- **The container tier runs the agent.** Under `isolation: container` the agent CLI itself runs inside
  the step's `image:` — as the project owner (`--user uid:gid`, never root), with a fresh `HOME` every
  worker session, and its credentials passed **by name** (`-e NAME`: `ANTHROPIC_API_KEY`,
  `CLAUDE_CODE_OAUTH_TOKEN`, `CODEX_API_KEY`, …) or mounted **read-only** (`~/.codex/auth.json`) — never
  baked into an image, never written to a ledger, log or the report. New keys:
  `isolation_policy.pass_env` (more names) and `isolation_policy.credentials` (more read-only mounts),
  `judge.image` (the judges' image). Reference images and the image contract: `docker/claude`,
  `docker/codex`, `docs/CONFIG.md` → *The container tier runs the agent*. Startup probes each image
  (as the project owner): an image without the CLI a step needs, or a mounted path the engine cannot
  see (colima shares only `$HOME` — a bind of anything else is an empty directory), is refused at its
  key; `agg doctor --json` reports `container_images`. A container step's agent no longer needs to be
  installed on the host. Driver: `Agg::image`, `Step::image`, `Agg::judge_image`, `Agg::pass_env`,
  `Agg::credentials`. Manual real-agent check: `scripts/e2e_container_real.sh`.
- **Read-only context directories — `context:`.** A list of `{ path, as, mode: ro }`: directories from
  outside the project (a knowledge base, reference material) that every confined process reads and
  none may change. `sandbox` keeps them read-only in the kernel (macOS: a Seatbelt write deny plus an
  unlink deny on each ancestor, so a context cannot be renamed away and replaced; Linux: `bwrap
  --ro-bind`), `container` mounts them `:ro` at `/context/<as>`. The brief lists each one with a pointer
  to its `index.md`; every confined process, judges included, finds it as `AGG_CONTEXT_<AS>`. Refused at
  startup, each at its key: a missing path, a file, an overlap with the project, a malformed or
  duplicate `as`, a `mode` other than `ro`, and any `isolation: none` step. Driver: `Agg::context(path,
  name)`. See `docs/CONFIG.md` → *Context directories*.

### Changed

- **Judges run in the run's strongest tier, `container` included** — there is no opt-out. When any
  step is `isolation: container`, every script judge and LLM judge runs in `judge.image` with the project
  mounted read-only (scratch and toolchain cache under `agg/private/container/judge/`). They used to
  run on the host, unconfined, so a container worker could rewrite a judge script into a host escape.
  A container run's judges therefore need their toolchain in `judge.image`. Hooks stay host tooling.
- `isolation: container` no longer mounts the host's agent state (`~/.claude`, `~/.codex`) into the
  container; the agent gets its own `HOME` instead.

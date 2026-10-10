# Adding an agent backend

A **backend** is how agg drives one coding-agent CLI: Claude Code, OpenAI Codex and GitHub Copilot
today. This guide is for adding another one (Gemini CLI, OpenHands, Kiro, …). It covers the
`AgentBackend` contract, the capability flags, how the event stream and the one-shot call are parsed,
usage and rate-limit reporting, the recorded fixtures, the tests a backend must ship, and a checklist.

The code is the authority. Read these first, in this order:

| file | what it holds |
|---|---|
| `src/backend/mod.rs` | the `AgentBackend` trait, the capability matrix of the three agents (module doc), the shared rate-limit matcher `looks_rate_limited`, the registry `KNOWN` + `for_name` |
| `src/backend/contract.rs` | the data types: `Capabilities`, `SessionSpec`, `SessionReport`, `OneShot` |
| `src/backend/claude.rs` + `stream.rs` | the reference backend and its wire-format parser |
| `src/backend/codex/`, `src/backend/copilot/` | two backends whose wire formats differ from Claude's in every way that matters |
| `src/backend/worker.rs` | how agg spawns, supervises and reads a worker session |
| `src/capability.rs` | the startup refusals that the capability flags feed |
| `src/usage.rs` | `Tokens` and `Usage`, what a backend reports |
| `tests/fixtures/agent-streams/` | the recorded streams every parser test runs against |

## The rule: the wire, not the docs

Every field path in the three backends was read off a **recorded stream** from the real CLI, not off
its documentation. That is not pedantry. Copilot's SDK docs describe a `session.shutdown` terminal
event, an `assistant.usage` event and no top-level session id; the CLI emits none of those
(`src/backend/copilot/mod.rs`). Codex's `-p` means `--profile`, not "prompt". Only a recording shows
that. So: install the CLI, record its output, check the recording in, and write the parser against it.
A backend whose CLI you cannot run is a backend you cannot write yet ([Kiro](backends/kiro.md) is the
worked example of that verdict).

## What a backend is

One unit struct implementing `AgentBackend` (`src/backend/mod.rs`), selected by the `agent:` value in
`agg.yaml`. It is always a **subprocess**: agg builds a `Command`, the OS sandbox or container wraps
it, agg spawns it in its own process group, reads its stdout line by line, and supervises it with a
heartbeat and a watchdog (`src/backend/worker.rs`). The backend decides *what* to run and *how to
read* what comes back; agg owns process management, git, isolation and judging.

It is used in two roles, possibly by two different backends in one run:

- **the worker** (`defaults.agent`, `steps.<s>.agent`): one fresh agentic session per loop step,
  `session_command` + the per-line parsers;
- **the ruler** (`judge.agent`): non-agentic, read-only `one_shot` calls for `.md` (LLM) judges and the
  summarizer.

## The contract, method by method

All in `trait AgentBackend` (`src/backend/mod.rs`). "Called by" says where agg uses each.

| method | must do | called by |
|---|---|---|
| `name()` | the `agg.yaml` `agent:` value; must equal the `KNOWN` entry | everywhere (banner, report, per-agent usage) |
| `bin()` | the CLI binary, resolved via `PATH` so tests can shim a fake | `session_command`, doctor |
| `capabilities()` | the honest `Capabilities` (next section) | `capability::assess` at startup, `agg doctor --json` |
| `default_model()` / `default_summary_model()` | the worker / ruler model when `agg.yaml` names none. `""` = pass no model flag (Codex: a named model can be a hard 400) | config resolution at use time, `agg init` |
| `default_effort()` | the effort when `agg.yaml` names none; `""` = pass none | config resolution, `agg init` |
| `session_command(&SessionSpec)` | build the whole worker `Command`: auto-approval, the event-stream flag, model, effort, `extra_args` (operator `worker_args`, placed **before** the prompt), the prompt, `cwd`, **stdin `/dev/null`**, stdout+stderr piped. Returns a whole `Command` so a backend can reshape argv (Codex resume is a subcommand) | `worker::run_session` |
| `parse_event(line)` | one stream line → a display `stream::Event` (💬 think, 🔧 tool, ↳ tool result, ✅ result, ▶ init) or `None` | worker reader thread → log, TUI, summarizer |
| `parse_usage(line)` | the tokens on **this line**, as an increment | worker reader (summed), `tally_one_shot` |
| `parse_model(line)` | the model **this line** says ran; default `None` | worker reader (first wins), `tally_one_shot` |
| `parse_session_id(line)` | the resume handle on **this line** | worker reader (last wins) |
| `parse_result(line)` | `Some(SessionReport)` only for a **terminal** event: cost, `rate_limited`, `rate_limit_reset` | worker reader, `tally_one_shot` |
| `one_shot(prompt, model, timeout, cwd, isolation)` | a non-agentic, read-only prompt → text call; returns `OneShot { body, stderr, success, usage }` | LLM judges (`src/core/judge.rs`), summarizer (`src/summary.rs`) |
| `confine_one_shot` (provided) | wraps a one-shot in the OS sandbox under `isolation: sandbox`; call it on every one-shot `Command` | your `one_shot` |
| `tally_one_shot` (provided) | sums a one-shot's usage with your own parsers (a single JSON object or a JSON Lines stream) | your `one_shot` |
| `config_conflict(model, effort)` | explain a pair of individually legal values the CLI rejects together; default `None` | `capability::assess` |
| `spend_ceiling_hint()` | how the CLI caps its own spend when it cannot report dollars; default `None` | `capability::assess` |
| `self_sandboxes()` | `true` only if the CLI has its own kernel jail; reporting only, it never exempts the CLI from agg's wrapper | `agg doctor --json` |
| `writable_state_paths()` | the `~/…` dirs and files the CLI must write while confined (`~/.claude` + `~/.claude.json`, `~/.codex`, …); use `state_paths_that_exist` | the sandbox and container wrappers |
| `is_installed()` / `preflight()` | `--version` probe; `preflight` bails with install + login + a headless check command | `agg doctor`, `agg check`, run startup |

Three traps, each of which shipped once (`src/backend/mod.rs`, "Three traps"):

1. **`-p` is not universal.** Codex takes the prompt positionally; `-p` is `--profile` there.
2. **Resume is not always a flag.** Codex resumes with `codex exec resume <ID> <PROMPT>`.
3. **Terminal events are not uniform.** Codex ends with `turn.completed`, `turn.failed` *or* a bare
   `error`; Copilot's `result` has top-level fields while every other Copilot event nests under
   `data`. Process exit stays the ground truth: a missed terminal event degrades the report, it never
   hangs the loop.

And one more that is not a trap but a rule: **stdin is `/dev/null`**. `codex exec` blocks forever on
stdin EOF as a non-TTY child; Copilot ignores piped stdin when `-p` is given. The prompt goes on argv.
(agg keeps the argv short: the worker's prompt is a fixed pointer at `agg/private/INSTRUCTIONS.md`,
`loop_::INSTRUCTIONS_POINTER`, and the backend does not need to care how long the brief is.)

## The capability flags: refuse rather than degrade

`Capabilities` (`src/backend/contract.rs`) says what the CLI can **report** or **support**. A `false`
is not a defect to work around. It turns a config that needs the capability into a **startup
refusal** (`src/capability.rs`, run by `agg run`, `agg plan`, `agg check` and `agg doctor`), because
the alternative is a guard that silently never fires: an `over_cost` that reads a cost nobody reports
stays `0.0` forever, and an autonomous loop spends real money with no ceiling. The flags are also
serialized as-is into `agg doctor --json`, so their names are a stable interface.

| flag | true means | a config that needs it | when `false` | Claude / Codex / Copilot |
|---|---|---|---|---|
| `reports_output_tokens` | `parse_usage` yields output tokens | `sequence.limits.tokens`, `over_budget` in `done_if`/`abort_if` | refused at startup | ✅ / ✅ / ✅ |
| `reports_cost_usd` | `parse_result` yields `cost_usd` in **dollars** (a credit multiplier is not dollars) | `sequence.limits.cost`, `over_cost` | **warning** if `spend_ceiling_hint()` names a self-cap (the guard is inert, the run is still bounded), else refused | ✅ / ❌ / ❌ (hint: `--max-ai-credits`) |
| `supports_effort` | `session_command` passes `spec.effort` | a non-empty `effort:` | refused at startup | ✅ / ✅ (`-c model_reasoning_effort=`, `max` clamps to `high`) / ✅ |
| `supports_one_shot` | `one_shot` is a read-only call: tools that write are denied, MCP off, **worker-writable repo config ignored** | an LLM judge, or `summary.enabled`, on the **ruler** | refused at startup (your `one_shot` may then be `unreachable!()`) | ✅ / ✅ / ✅ |
| `detects_rate_limits` | `parse_result` can tell a rate/usage limit from an ordinary failure | nothing refuses on it | reported (`agg doctor --json`, the agent table in `docs/GUIDE.md`); a limit is then scored as an ordinary failed session | ✅ / ✅ / ❌ |
| `supports_resume` | `parse_session_id` yields a handle and `session_command` honours `spec.resume_id` | nothing today: every worker session is fresh (`worker::run_session` passes `resume_id: None`) | reported only | ✅ / ✅ / ✅ |

"Refuse rather than degrade" in each case:

- **Tokens and cost.** Say `false` when the CLI does not report the number. Never return `Some(0)`
  for "not reported": `None` is what lets agg tell the two apart (`SessionReport`, `usage::Tokens`).
- **Effort.** If the CLI has fewer levels than `low|medium|high|xhigh|max`, map them in
  `session_command` (Codex clamps `xhigh`/`max` to `high`: asking for more thinking than the model
  offers gives its most). If two settings conflict, say so in `config_conflict` (Copilot rejects
  `--model auto` with `--effort`); do not drop a flag silently.
- **One-shot.** "Read-only" is the property, enforced by the CLI, not requested from the model.
  Each backend gets it differently: Claude omits `--dangerously-skip-permissions` and adds
  `--strict-mcp-config --setting-sources user`; Codex uses `--sandbox read-only --ignore-user-config
  --ignore-rules --ephemeral`; Copilot withholds `--allow-all-tools` and adds `--no-custom-instructions
  --disable-builtin-mcps`. The part that is easy to miss is the **trust boundary**: the worker writes
  the repo, so a one-shot that loads repo instructions, hooks or agent profiles lets the worker steer
  its own judge. If the CLI has no way to ignore those for one call, `supports_one_shot` is `false`.
- **Rate limits.** Detect from **terminal events only** (next sections). If the CLI does not put the
  limit anywhere you can see it, say `false`, as Copilot does, rather than guess a field path.

Adding a **new** flag is a stable-interface change: the field, its consumer in `capability.rs`, the
`agg doctor --json` row in `docs/GUIDE.md` ("Embedding agg"), and the flag list in `tests/check.rs`.

## Parsing a worker session

`worker::run_session` builds the command, wraps it for the isolation tier, spawns it, and hands every
stdout line to the backend:

```text
for each line:
  parse_event       → printed to the log, pushed to the TUI tail, 💬 thoughts kept for the summarizer
  parse_usage       → ADDED to the session's tokens
  parse_model       → kept if no model was seen yet (first wins)
  parse_session_id  → kept (last wins)
  parse_result      → terminal: rate_limited / rate_limit_reset stored, cost_usd SET (not added)
```

- `parse_usage` returns an **increment**, never a total. Claude reports cumulative usage once, on its
  terminal `result`; Copilot reports `outputTokens` on every `assistant.message` and **nothing** on its
  terminal event. Summing serves both; a terminal-only reader would report zero for Copilot.
- `parse_session_id` is per line for the same reason: Claude and Copilot put the id on the terminal
  event, Codex on the **first** one (`thread.started`).
- `parse_result` returns `Some` only for a terminal event, and a `Some` with `None` fields is correct
  for an agent that reports nothing there.
- Parsers must not panic on garbage: every line is untrusted, tool output included. Return `None`.

stderr is captured but not parsed. A session that exits non-zero with **zero output tokens** three
times in a row is treated as a broken invocation and the run aborts (`context::worker_is_broken`);
that is how a CLI rejecting its own argv shows up, so make sure a real session reports its tokens.

## Parsing a one-shot

`one_shot` returns the model's **text** in `OneShot.body` with the envelope removed, plus `stderr`,
`success` (the exit status) and `usage`. agg's judge parser then looks for the verdict JSON in the
body. Two rules:

- **Fall back to the raw stdout** when the envelope is not the expected shape (`claude.rs`
  `unwrap_envelope`, `codex/mod.rs` `last_agent_message`). An empty body reads as "not met"; the raw
  text at least shows what happened.
- **Call `self.tally_one_shot(&out.stdout)`** for `usage`. It handles both shapes, a single JSON object
  (Claude's `--output-format json`) and a JSON Lines stream (Codex, Copilot), with your own parsers.

Build the command, pass it through `self.confine_one_shot(command, cwd, isolation)?`, and run it with
`crate::os::proc::run_with_timeout`. stdin `/dev/null` here too.

## Usage and model reporting

What a backend reports ends up in the run report's `session_log` and totals, `agg status --json` and
`agg history --json` (`src/usage.rs`, `docs/schemas/report.v2.json`):

- **Four token kinds that do not overlap** (`usage::Tokens`): `input` (uncached prompt), `output`,
  `cache_write`, `cache_read`. Map the CLI's fields onto them exactly. If the CLI's input count
  *includes* cached tokens, subtract (Codex: `input = input_tokens − cached_input_tokens`). If it
  reports reasoning tokens as a breakdown of output, do not add them again (Codex
  `reasoning_output_tokens`). Never fold one kind into another: Claude's cache **writes** were once
  added to output, and a 4-token answer reported 23 892.
- **`None` is "not reported"**, `Some(0)` is "reported zero". A sum stays `None` only if nothing in it
  reported that kind; `cost_source` is `reported`, `partial` or `unknown` from the same rule.
- **Cost** is dollars or nothing. It is cumulative on the terminal event, so it is SET.
- **The model is the one the CLI reports**, not the configured one: an alias resolves, an `auto` mode
  picks. Claude names it on `system`/`init` (and in `modelUsage` of a one-shot), Copilot on
  `data.model` of `assistant.*`; Codex's stream never names it, so its record says `null` rather than
  guessing.
- Worker and ruler are recorded apart; the backend does not need to know which it is serving.

## Rate-limit detection

A rate-limited session is not judged. agg saves its partial work under
`refs/agg/partial/<run>/<session>`, waits until the reset (or `ratelimit_backoff_secs`, default 1800,
when no reset is known), and lets `agg send wake` end the wait no sooner than `ratelimit_min_gap_secs`
(default 60) after the limited attempt (`src/features/backoff.rs`). The backend's part:

1. In `parse_result`, set `rate_limited` from the **terminal** event only. Pass its error text to the
   shared matcher `backend::looks_rate_limited` (`src/backend/mod.rs`). Never scan tool output: a build
   log that prints "429" must not park the loop for 30 minutes.
2. If the CLI's wording matches nothing in that list, **add the pattern** there, with the verbatim
   message it was captured from in a comment, and a case in
   `the_rate_limit_detector_matches_what_each_agent_really_sends` (`src/backend/tests.rs`). The list has
   been widened three times for wording no one had seen yet.
3. If the message says when the limit resets, return it as an **absolute** epoch in
   `rate_limit_reset` (Claude: `resets 12:50pm`, parsed by `stream::parse_reset_epoch`, bounded to
   25 h). Otherwise `None`, and agg uses the fixed backoff.
4. agg counts a limit only when the process also **exits non-zero** (`worker.rs`: "a clean exit 0 is
   never a rate-limit"), so a CLI that exits 0 after a limit is not detected.

## Recorded fixtures

`tests/fixtures/agent-streams/` holds raw CLI output captured from the real CLIs, and every parser
test reads it with `include_str!`. A fixture is the evidence a field path rests on.

**Format.** The CLI's stdout, byte for byte, one event per line (`.jsonl`) for a stream, or the single
JSON document (`.json`) for a one-shot that prints one object. Name it `<agent>-<cli version>.jsonl`,
with a suffix for a variant (`claude-2.1.296-oneshot.json`, `copilot-1.0.70-tools.jsonl`). You may
**drop** lines that carry nothing the parsers read and **remove operator-specific fields** (paths,
account ids, MCP server lists), as the Claude fixture's README entry says it did. Never edit a value a
parser reads, and never invent a line: a hand-written "fixture" proves nothing.

**How to record one.** Cheapest possible prompt, cheapest model, the exact flags `session_command` or
`one_shot` will use, in a scratch directory:

```bash
cd "$(mktemp -d)"
claude -p "Reply with exactly: OK" --output-format stream-json --verbose --model haiku \
  --strict-mcp-config --setting-sources user > claude-<version>.jsonl
copilot -p "Reply with exactly: OK" --allow-all-tools --output-format json > copilot-<version>.jsonl
codex exec --json "Reply with exactly: OK" < /dev/null > codex-<version>.jsonl
```

Record at least: a plain answer (usage, model, session id, the terminal event), and where they differ,
the one-shot shape and a run that calls a tool. A failure (bad model name, forced 400) shows the error
shape cheaply; a real rate limit cannot be forced, so capture it when it happens and add it then.
Recording costs real quota, so diff against the existing fixtures first.

**Document it.** Add a row to `tests/fixtures/agent-streams/README.md` (file, the command that
produced it) and a short "what it proves" section: where the tokens are, where the terminal event is,
what is absent.

## Required tests

Unit tests next to the backend (`src/backend/<agent>/tests.rs`, as `codex/` and `copilot/` do), each
against the recorded fixture where one applies:

- **usage**: the summed `parse_usage` over the fixture equals the reported figures, kind by kind,
  with unreported kinds `None`; the terminal event contributes what the wire says it does.
- **model**: `parse_model` finds the reported model (or none, if the stream never names it), and
  `tally_one_shot` over the one-shot fixture yields model, tokens and cost.
- **session id**: found on the event the wire puts it on.
- **terminal events**: every terminal shape is `Some`, a non-terminal line is `None`, and an
  unreported cost is `None`, not `Some(0.0)`.
- **rate limits**: a terminal limit is detected, the same words inside tool output are not.
- **argv**: the prompt is where the CLI reads it (and last, if positional); `worker_args` precede it;
  an empty model or effort omits the flag; resume has the right shape; the argv under `isolation:
  none` vs `sandbox` (the OS wrapper confines, the flags do not change, unless the CLI self-sandboxes).
- **one-shot isolation**: the flags that grant writes are absent from the one-shot argv and the flags
  that cut repo config are present (Copilot's `the_judge_call_never_gets_write_access` is the model).
- **capabilities**: an assertion per flag, with the reason in the message.

Shared tests in `src/backend/tests.rs` pick a new `KNOWN` entry up automatically:
`every_known_agent_actually_resolves`, `no_backend_defaults_to_an_effort_its_default_model_cannot_accept`,
and the unknown-agent error listing. `src/doctor/json.rs` asserts doctor reports exactly `KNOWN`, and
`src/driver/types.rs` asserts the driver's `Agent` enum names exactly `KNOWN`.

End to end: the suites shim a fake CLI on `PATH` (`tests/cli.rs` `project_with_fake_claude`,
`scripts/e2e.sh` §9k shims a fake `codex` for a per-step agent override). Add a fake for the new
binary where a test exercises agent selection. `scripts/e2e_real.sh` runs the real Claude CLI against
a real model; a real run of the new CLI through `agg run` is the final check before shipping, and it
is what caught Copilot's `auto` + `effort` conflict, which no unit test could.

## Checklist

- [ ] The CLI installed, logged in, and a headless run working by hand.
- [ ] Fixtures recorded and checked in, with a README entry and "what it proves".
- [ ] `src/backend/<agent>/mod.rs` (+ `tests.rs`): the struct and `impl AgentBackend`. Module doc: the
      CLI version verified against and the event table, as in `codex/mod.rs`.
- [ ] `capabilities()` honest, one comment per flag saying where the evidence is.
- [ ] `session_command`: auto-approval, stream flag, prompt placement, stdin `/dev/null`, stdout and
      stderr piped, `worker_args` before the prompt, empty model/effort omitted.
- [ ] Parsers: increments for usage, first model, per-line session id, terminal-only `parse_result`,
      `looks_rate_limited` on terminal error text only.
- [ ] `one_shot` read-only, repo config ignored, `confine_one_shot`, `tally_one_shot`, raw fallback.
- [ ] `writable_state_paths` measured under `isolation: sandbox`; `preflight` with install, login and a
      headless check command (keep it parallel with the other three).
- [ ] Registered: `pub mod` + `KNOWN` + `for_name` in `src/backend/mod.rs`; an `Agent` variant and its
      `name()` in `src/driver/types.rs`. (A driver can also bring its own backend without touching the
      crate: `Agent::Custom(Arc<dyn AgentBackend>)`.)
- [ ] Where agents are listed: `relative_root` and `host_agent` in `src/skills.rs` (which skills dir the
      CLI reads, and the env marker it sets in its shell, if any); `tests/schema.rs` (the scaffold for
      every agent); the "known agents" assertion in `scripts/e2e.sh` §13; the colour accents in
      `src/ui/dashboard/fmt.rs` and `src/web/src/lib/format.js` (optional; unknown agents get a default).
- [ ] Docs: the agent table in `docs/GUIDE.md` ("Choosing an agent"), the `agent:` values in
      `docs/CONFIG.md`, `AGENTS.md`, the README, and a CHANGELOG entry.
- [ ] Gate: `cargo build` with 0 warnings, `cargo test`, `cargo clippy --all-targets -- -D warnings`,
      `scripts/e2e.sh`, and one real `agg run` against the new CLI.

## Investigated, not implemented

- **Kiro CLI**: headless mode, auto-approval and a JSON Lines stream exist, but the stream's event
  schema, token reporting, rate-limit signalling and a one-shot trust boundary are undocumented, and
  the CLI was not available to record. See [backends/kiro.md](backends/kiro.md).

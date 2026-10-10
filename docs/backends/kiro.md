# Kiro CLI as an agg backend: investigation

**Verdict (2026-10-10): not implemented.** The Kiro CLI has a documented headless mode with
auto-approval, model and effort selection, and a JSON Lines output format. What it does not have is a
*documented* event schema: the docs do not say which events `--output-format stream-json` emits, where
the session ends, where (or whether) tokens are reported, or how a rate limit shows up. agg's rule is
to verify agent behaviour on the wire, never from docs alone ([BACKENDS.md](../BACKENDS.md#the-rule-the-wire-not-the-docs)),
and the CLI is not installed here, so no stream could be recorded. A backend written now would have to
guess its field paths, which is exactly the failure that header comment in `src/backend/mod.rs`
warns about (Copilot's SDK docs were wrong on three counts).

The sections below record what the official docs say, what is missing, and what it would take.

Kiro CLI version at the time of writing: 2.29.0 (2026-10-09). Headless mode shipped in 2.0 (2026-04-13). [10]

## What agg needs, and what Kiro documents

| agg needs (`AgentBackend`) | Kiro CLI, per its docs | Enough? |
|---|---|---|
| Headless prompt on argv, stdin `/dev/null` | `kiro-cli chat --no-interactive "<prompt>"`; the prompt is positional or piped stdin, not both. V2/V3 read stdin only without a positional argument. [1] | ✅ |
| Auto-approval for a worker | `--trust-all-tools` (also auto-approves Hook confirmations); `--trust-tools=<ids>` for exact tool IDs on V3. [1][2] | ✅ |
| Machine-readable event stream | `--output-format stream-json`: "run events as JSON Lines on stdout", V2/V3 engines only. The event types are **not documented**. An interrupted V3 run "writes a final interruption record". [1] | ❌ schema undocumented |
| Terminal event (`parse_result`) | Not documented for `stream-json`. The exit codes are: 0 success, 1 general failure, 3 MCP startup failure (with `--require-mcp-startup`), 4 requested agent not found. [3] | ⚠ exit code only |
| Output tokens (`reports_output_tokens`) | Not documented for headless mode. Kiro bills in **credits**, with per-model credit multipliers, not tokens. [5][6] The ACP V3 protocol names "turn metering (`promptTurnSummaries`)" on a `session_info_update` with `_meta.kiro.kind: turn_completion`, but not its fields. [4] | ❌ |
| Dollar cost (`reports_cost_usd`) | None. Credits, not currency (like Copilot's AI Credits). [5][6] | ❌ |
| Model that ran (`parse_model`) | `--model <id>` pins it; if Kiro cannot set it, a V3 headless run "writes a warning to stderr and continues", so the configured model is not proof of what ran. Where the stream names the model is not documented. [1] | ❌ |
| Effort (`supports_effort`) | `--effort low|medium|high|xhigh|max`, for models that support it. [1][2] | ✅ |
| Resume (`supports_resume`) | `--resume`, `--resume-id <ID>`, `--list-sessions`; session ids are UUIDs, saved per directory. Where the id appears in `stream-json` is not documented. [2] | ⚠ |
| Rate-limit detection (`detects_rate_limits`) | No exit code for it. [3] The ACP protocol has a `_kiro/error/rate_limit` notification (formerly `_kiro.dev/error/rate_limit`), payload undocumented, and whether it reaches `stream-json` is not stated. [4] | ❌ |
| Read-only one-shot (`supports_one_shot`) | Half there. `--trust-tools=read_file,grep_search` in non-interactive V3 approves only those tools, and there is no user to approve the rest. [1] But the repo can carry agent inputs: custom agents (`.kiro/agents/`), Steering files (`.kiro/steering/`), workspace MCP configuration, Skills, Workflows, and (V3 headless, 2.27.1+) Hooks. [1][7][8] Kiro withholds most of them from an **untrusted** workspace, but trust is a per-workspace decision, not a per-call flag, and the worker runs in the same workspace as its judge. No flag to ignore workspace inputs for one call (like Claude's `--setting-sources user` or Codex's `--ignore-rules`) is documented, so the worker could steer its own judge through files it writes. | ❌ trust boundary open |
| Headless authentication | `KIRO_API_KEY` only, and API keys exist only on Pro, Pro+, Pro Max and Power plans (an admin must enable them for managed subscriptions). [1] | ⚠ paid plans only |

Two things are in Kiro's favour and worth keeping for later: permission rules and the workspace
trust decision live **outside** the repository (`~/.kiro/settings/permissions.yaml`,
`~/.kiro/workspace-roots/<hash>/`), so "a cloned repo cannot inject permission rules" [8]; and the ACP server (`kiro-cli acp`) is a
documented JSON-RPC protocol. ACP does not fit agg's contract, though: it is a bidirectional session
over stdin/stdout, and agg's backends are one argv invocation with stdin nulled and stdout read as a
line stream (`AgentBackend::session_command`).

## Third-party evidence (not authoritative)

An open-source orchestrator, `mkny13/mahler`, drives `kiro-cli chat <prompt> --output-format
stream-json --no-interactive --trust-tools read,write,glob,grep` and documents the events it reads as
"ACP v2 events (verified 2026-10-01): runStarted, metadata, sessionUpdate (agent_message_chunk),
runError, runFinished", with `sessionId` in `data` and `status`, `finalText`, `stopReason` on
`runFinished`. [9] It reads credits from a separate `/usage` prompt, not from the stream, which
suggests the stream carries no usage. Its checked-in fixture `tests/fixtures/kiro.log` is not a raw
capture (its session id is a placeholder), so it cannot serve as an agg fixture. This is consistent
with the official docs, and it does not replace a recording.

## What a Kiro backend would need

1. **A recorded stream**, made with the CLI installed and an API key: `kiro-cli chat --v3
   --no-interactive --trust-all-tools --output-format stream-json "Reply with exactly: OK"`, plus one
   run that uses a tool, one that fails, and the one-shot shape below. Check them in under
   `tests/fixtures/agent-streams/kiro-<version>*.jsonl` (see [BACKENDS.md](../BACKENDS.md#recorded-fixtures)).
2. **From those recordings:** the terminal event (and its failure shape and the interruption record),
   where the session id and the model appear, and whether any token count appears at all. Without
   tokens, `reports_output_tokens` is `false` and `limits.tokens`/`over_budget` are refused on Kiro
   steps. `reports_cost_usd` is `false` in any case (credits). Whether Kiro has a self-cap for
   `spend_ceiling_hint` is undocumented.
3. **A captured rate limit or credit exhaustion**, to see whether it reaches the stream, stderr or
   only the exit code. Until then `detects_rate_limits: false`, as for Copilot.
4. **A judge isolation recipe** that keeps workspace Hooks, agents, Steering, Skills and MCP
   configuration out of a one-shot call even when the worker's workspace is trusted, verified on
   the wire: a probe in which the model tries to write and is denied. Until then `supports_one_shot: false`, so a Kiro *ruler* is refused for LLM judges and the
   summarizer (a Kiro *worker* with a Claude/Codex/Copilot ruler is fine).
5. The confined-worker carve-out (`writable_state_paths`, presumably `~/.kiro`), measured under
   `isolation: sandbox` as the other backends' were.

## Sources

1. Kiro docs, *Headless mode* (page updated 2026-10-10): <https://kiro.dev/docs/cli/headless/>
2. Kiro docs, *CLI commands*: <https://kiro.dev/docs/reference/cli-commands/>
3. Kiro docs, *Exit codes* (page updated 2026-10-05): <https://kiro.dev/docs/reference/exit-codes/>
4. Kiro docs, *Migrate an ACP client to CLI V3* (session updates; replacement of `_kiro.dev/metadata`
   and `_kiro.dev/error/rate_limit`): <https://kiro.dev/docs/cli/v3/acp-migration/>
5. Kiro docs, *Models* (credit multipliers): <https://kiro.dev/docs/models/>
6. Kiro docs, *Billing* (credits): <https://kiro.dev/docs/billing/>
7. Kiro blog, *Run Kiro CLI programmatically: introducing headless mode* (custom agents in the
   repository's `.kiro/agents/`): <https://kiro.dev/blog/introducing-headless-mode/>
8. Kiro docs, *Permissions*: <https://kiro.dev/docs/permissions/>
9. `mkny13/mahler`, `mahler/platforms.py` (`kiro_argv`, `_read_kiro_event`), third party:
   <https://github.com/mkny13/mahler/blob/main/mahler/platforms.py>
10. Kiro changelog (2.29.0, 2026-10-09; 2.0, 2026-04-13): <https://kiro.dev/changelog/cli/>,
    <https://kiro.dev/changelog/cli/2-0/>

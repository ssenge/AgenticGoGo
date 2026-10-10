# Captured agent event streams

Real `--output-format json` output, recorded from live CLIs. These exist so a future backend can be
written and tested against **observed** field paths instead of documentation — which, for Copilot,
turned out to be wrong on three counts (see `backend.rs`).

Recording one costs real quota. Diff against these first. How to record one, and what a backend
must test against it: `docs/BACKENDS.md` ("Recorded fixtures").

| file | produced by |
|---|---|
| `copilot-1.0.70.jsonl` | `copilot -p "Reply with exactly: OK" --allow-all-tools --output-format json` (free tier, gpt-5-mini, 0 premium requests) |
| `claude-2.1.296.jsonl` | `claude -p "Reply with exactly: OK" --output-format stream-json --verbose --model haiku --strict-mcp-config --setting-sources user` — reduced to the `system`/`init` (its operator-specific fields dropped), `assistant`, `rate_limit_event` and `result` lines |
| `claude-2.1.296-oneshot.json` | the same prompt with `--output-format json`, the shape a judge or summarizer call returns |

## What `claude-2.1.296*` proves

- the cumulative `usage` is on the terminal `result` and splits four ways: `input_tokens` (uncached),
  `output_tokens`, `cache_creation_input_tokens` (cache WRITE) and `cache_read_input_tokens`. A
  4-token answer wrote 23 888 tokens to the cache — which is why folding cache writes into output
  was wrong.
- the model that ran is `model` on `system`/`init` (`--model haiku` resolved to a full name), and the
  one-shot JSON, which has no init, names it as the key of `modelUsage`.

## What `copilot-1.0.70.jsonl` proves, and its SDK docs deny

- the terminal event is **`result`** (not `session.shutdown`), and it carries `sessionId` + `exitCode`
- **tokens are on `assistant.message.data.outputTokens`** — per message. The terminal event has NO
  token count at all; its `usage` object holds `premiumRequests` and durations.
- there is **no dollar cost anywhere**.

The first two are why `AgentBackend::parse_usage` accumulates per-line instead of reading the
terminal event.

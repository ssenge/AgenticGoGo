# Captured agent event streams

Real `--output-format json` output, recorded from live CLIs. These exist so a future backend can be
written and tested against **observed** field paths instead of documentation — which, for Copilot,
turned out to be wrong on three counts (see `backend.rs`).

Recording one costs real quota. Diff against these first.

| file | produced by |
|---|---|
| `copilot-1.0.70.jsonl` | `copilot -p "Reply with exactly: OK" --allow-all-tools --output-format json` (free tier, gpt-5-mini, 0 premium requests) |

## What `copilot-1.0.70.jsonl` proves, and its SDK docs deny

- the terminal event is **`result`** (not `session.shutdown`), and it carries `sessionId` + `exitCode`
- **tokens are on `assistant.message.data.outputTokens`** — per message. The terminal event has NO
  token count at all; its `usage` object holds `premiumRequests` and durations.
- there is **no dollar cost anywhere**.

The first two are why `AgentBackend::parse_usage` accumulates per-line instead of reading the
terminal event.

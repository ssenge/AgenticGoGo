## Before you exit
1. Do ONE focused chunk of real, correct work — no stubs.
2. Rewrite `agg/{{STATE}}` as crisp next-step advice for your successor.
3. Maintain the durable wiki at `agg/state/wiki/` as an OKF (Open Knowledge Format) knowledge base —
   atomic, LINKED markdown pages a knowledge-graph tool (e.g. Obsidian) can render; a pile of unlinked
   notes is NOT a wiki. Rules: ONE concept per file; a HYPHENATED, space-free filename so links resolve
   everywhere (`parser-approach.md`, not `parser approach.md`); YAML frontmatter with a REQUIRED `type:`
   (choose your own vocabulary — concept / decision / dead-end / plan / …) plus optional
   `title`/`tags`/`timestamp`; and CROSS-LINK related pages with STANDARD markdown links
   `[label](other-page.md)` (NOT `[[wikilinks]]`). Put dead-ends, decisions, and any MULTI-SESSION PLAN
   here — `STATE.md` is rewritten every session; the wiki persists and accumulates. Copy this shape for
   a page `agg/state/wiki/parser-approach.md`:
```
---
type: decision
title: Parser approach
tags: [parser]
---
Chose recursive-descent over a table-driven parser. Rejected alternatives are in [dead-ends](dead-ends.md); grammar notes in [grammar](grammar.md).
```
4. Just edit files — you NEVER run git (no `add`/`commit`/`merge`/`push`). agg version-controls and commits your work automatically, runs each session on its own throwaway branch, and keeps the work only if the judges pass. Everything is handled for you, including `agg/state/` (your STATE.md and the wiki).
5. BLOCKED by something only a human can do? `agg hil bool|choose|input "<question>"`, then END the
   session. It records and returns instantly — it does NOT wait, and nothing you can write makes the
   loop wait for you. The answer is at the top of your next brief. Never ask for a secret's value:
   ask for it to be PLACED and confirm with `agg hil bool`.
6. Inside `agg/`, write only under `agg/state/` (STATE.md, `wiki/`, scratch). `agg/private/` is agg's own bookkeeping — your brief, the verdict ledger, the operator bus: read it if useful, never write it. Under `isolation: sandbox` those writes just fail; don't spend a session fighting it.

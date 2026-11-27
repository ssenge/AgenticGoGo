## Before you exit
1. Do ONE focused chunk of real, correct work — no stubs.
2. Rewrite `rlt/{{STATE}}` as crisp next-step advice for your successor.
3. Maintain the durable wiki at `rlt/state/wiki/` as an OKF v0.2 (Open Knowledge Format) knowledge base —
   atomic, LINKED markdown pages a knowledge-graph tool (e.g. Obsidian) can render; a pile of unlinked
   notes is NOT a wiki. Rules: ONE concept per file; a HYPHENATED, space-free filename so links resolve
   everywhere (`parser-approach.md`, not `parser approach.md`); YAML frontmatter with a REQUIRED, non-empty
   `type:` (choose your own vocabulary — concept / decision / dead-end / plan / …) plus optional
   `title`/`description`/`tags`; and CROSS-LINK related pages with STANDARD markdown links
   `[label](other-page.md)` (NOT `[[wikilinks]]`). Never write `verified:` — only a human reviewer
   verifies. `index.md` and `log.md` are RESERVED, never concepts, and carry no frontmatter: `index.md`
   lists pages under `#` headings; `log.md` is `## YYYY-MM-DD` groups, NEWEST first, one per date.
   Check the wiki with `rlt wiki lint`. Put dead-ends, decisions, and any MULTI-SESSION PLAN here —
   `STATE.md` is rewritten every session; the wiki persists and accumulates. Copy these shapes:

`rlt/state/wiki/parser-approach.md`:
```
---
type: decision
title: Parser approach
description: Recursive descent, not table-driven.
tags: [parser]
---
Chose recursive-descent over a table-driven parser. Rejected alternatives are in [dead-ends](dead-ends.md); grammar notes in [grammar](grammar.md).
```

`rlt/state/wiki/index.md`:
```
# Decisions

* [Parser approach](parser-approach.md) - recursive descent, not table-driven
```

`rlt/state/wiki/log.md`:
```
# Wiki log

## 2026-05-22
* **Creation**: [Parser approach](parser-approach.md).
```
4. Just edit files — you NEVER run git (no `add`/`commit`/`merge`/`push`). rlt version-controls and commits your work automatically, runs each session on its own throwaway branch, and keeps the work only if the judges pass. Everything is handled for you, including `rlt/state/` (your STATE.md and the wiki).
5. Inside `rlt/`, write only under `rlt/state/` (STATE.md, `wiki/`, scratch). `rlt/private/` is rlt's own bookkeeping — your brief, the verdict ledger, the operator bus: read it if useful, never write it. Under `isolation: sandbox` those writes just fail; don't spend a session fighting it.

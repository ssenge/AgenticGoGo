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
Edit files freely — agg saves and version-controls your work automatically; you do NOT commit.

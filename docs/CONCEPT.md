# Agentic AI Orchestration Patterns: From Prompt Engineering to Agentic Workflows as Code

<!-- ⚠ OUTLINE, superseded in two places by the essay it produced (MEDIUM.md, published
     2026-08-12). The essay ships as "On Agentic AI Orchestration Patterns: Deterministic Loops for
     Stochastic Agents", and its top rung is "Agentic Workflows as Code" — NOT "Agents as Code",
     which is what this file said throughout. The agents stay agents; it is the WORKFLOW around
     them that becomes code, so the old name pointed at the one part of the system that is not a
     reviewable artifact. Renamed here so the outline and the published piece agree; the README and
     AGENTS.md were corrected in the same pass. -->

> **OUTLINE ONLY** — section names and bullets. Prose to follow.
>
> Working thesis: *each pattern exists because the previous one hit a wall. The unit of design keeps
> growing — token → context → iteration → topology → artifact — and every step is forced, not
> fashionable.*

---

## 1. The thesis in one paragraph

- Five years of "how do I get the model to do what I want" have produced a **ladder**, not a pile of
  competing techniques.
- Each rung closes a gap the rung below **cannot** close — not "does it better".
- Name the rungs and two things fall out: you can tell which one your problem is on, and you stop
  paying for a rung you don't need.
- ⚠ Framing to hold throughout: these are **not** alternatives. A graph still contains loops; a loop
  still needs a good prompt.

## 2. The ladder at a glance

- Table with a fixed shape — the whole article in one screen:

  | pattern | unit of design | the gap it closes | what breaks next |
  |---|---|---|---|
  | Prompt Engineering | one call | output is unsteerable | one shot; nothing carries |
  | Context Engineering | what the call *sees* | the model can't know what it wasn't shown | still one call; context is finite |
  | Loop Engineering | the iteration | unbounded work in a bounded window | one path only |
  | Graph Engineering | the topology | conditional work, cost control, recovery | the workflow is now a program |
  | Agentic Workflows as Code | the artifact | reproducibility, review, trust | — |

- Read the last column as the argument: **each row's failure is the next row's reason to exist.**

## 3. Level 0 — the harness (why this is not on the ladder)

- **Definition:** what a *single* agent session runs inside — system prompt, tools, file access,
  memory, sandbox, the verifier.
- **Why it is separate:** the harness is a *substrate*, orchestration is *composition over time*.
  Loops and graphs both run on a harness.
- ⚠ **The most common category error in this space:** treating "the loop" and "the harness" as the
  same thing. They vary independently — you can have a sophisticated harness with no loop, or a
  sophisticated graph over a naive harness.
- Why naming it first is worth a section: everything after this is *about topology*, and the reader
  needs somewhere to put harness concerns so they stop contaminating the argument.

## 4. Prompt Engineering — the unit is one call

- **Definition:** shaping the instruction so a single model call produces the wanted output.
- **Gap it closed:** raw generative output is unsteerable; instruction shape measurably changes
  quality (role framing, few-shot, chain-of-thought, output format contracts).
- **Canonical instances:** few-shot, CoT, structured-output schemas.
- **Where it runs out:**
  - one call, one shot — nothing persists to the next call;
  - it optimises *how you ask*, and cannot fix *what the model was never told*;
  - diminishing returns: past a point you are tuning wording, not capability.

## 5. Context Engineering — the unit is what the call sees

- **Definition:** deliberately assembling the information a call operates on — retrieval, file
  selection, summarisation, compaction, memory.
- **Gap it closed:** prompt quality ≠ information quality. A perfectly phrased question about code
  the model cannot see is still unanswerable.
- **Canonical instances:** RAG, repo maps, compaction/summarisation, memory files.
- **Where it runs out:**
  - still **one call**. The window is finite, and long tasks exceed any window;
  - **context rot** — a long session degrades: earlier instructions get diluted, contradictions
    accumulate, attention drifts;
  - no notion of *progress*. Nothing decides whether the work is done.

## 6. Loop Engineering — the unit is the iteration

- **Definition:** run a step, check a condition, repeat — with a **fresh** context each iteration and
  state carried *outside* the conversation (filesystem, git, notes).
- **Gap it closed — two, and the second is the important one:**
  1. **Unbounded work in a bounded window.** Context rot is dodged rather than fought: every
     iteration starts clean, so the run's length is decoupled from the window size.
  2. **Who decides "done".** A loop forces the question out of the model and into a condition the
     model does not evaluate. *This is the real content of the pattern.*
- **The non-obvious requirement:** a loop **without an external verifier is just repetition.** If the
  agent reports its own success, the loop terminates on a claim, not a fact.
  - name the failure: self-graded loops stop early, or never;
  - the verifier must be outside the agent's reach → foreshadows §8.
- **Canonical instance:** the *Ralph loop* (Huntley, 2025) — fresh session, prompt file on disk, state
  in the repo. Note it is one instance of the pattern, not the pattern itself.
- **Where it runs out:**
  - **one path.** Every iteration does the same thing;
  - cannot skip work that is unnecessary, cannot spend conditionally;
  - a check that costs 40 minutes runs on the iterations where it obviously cannot pass;
  - no recovery branch: "if it stalled, try differently" is not expressible.

## 7. Graph Engineering — the unit is the topology

- **Definition:** the loop **generalised**. Nodes are steps and checks, edges are permissible
  transitions; branches, joins and multiple paths become expressible.
- **A loop is the degenerate graph** — one node, one back-edge, one condition. This is the key
  sentence of the article; the progression is a *generalisation*, not a replacement.
- **Gaps it closes:**
  - **conditional work** — skip a step that a verdict makes pointless;
  - **cost control** — order checks cheap-to-expensive and short-circuit; the expensive one runs only
    where it can matter (the single largest cost lever in agentic systems);
  - **recovery paths** — stall → step back, try a different approach, escalate to a human;
  - **bounds that respond to what the run learned**, rather than constants chosen up front.
- **Knowledge as a graph, too** — the second sense of the word, and worth making explicit:
  - fresh sessions mean durable knowledge must be *navigable*, not append-only;
  - a linear log forces re-reading; a cross-linked graph lets the next session **enter at the right
    node**;
  - same shape at both levels: control flow and memory both stop being lines.
- **Where it runs out:**
  - the workflow is now a **program** — with branches, state and invariants;
  - expressing a program in a config format means inventing a language you did not design and cannot
    debug (conditionals in YAML, string-typed expressions, no tests);
  - and nothing yet says whether that program is *trustworthy*.

## 8. Agentic Workflows as Code — the unit is the artifact

- **Definition:** the whole orchestration — steps, topology, checks, definition of done — is
  **committed source**: versioned, diffable, reviewable, testable.
- **Gaps it closes:**
  - **reproducibility** — a prompt in someone's terminal history is not a system;
  - **review** — a workflow change becomes a pull request, with the same scrutiny as the code it
    drives;
  - **trust / tamper-evidence** — this is the one people miss. If the verifier is committed, an agent
    that edits the verifier to pass is *caught by version control*. The check is restorable because
    it is in git.
  - **testability** — a compiled workflow can be unit-tested. So can a config, if the engine
    validates it.
- **The direct analogy:** what
  [Infrastructure as Code](https://en.wikipedia.org/wiki/Infrastructure_as_code) did to
  click-configured servers, applied to agent workflows. Same argument, same objections, same
  resolution.
- ⚠ **Two axes, not one — resolve this explicitly:**
  - §4-7 are a **topology** axis (how much structure the work needs);
  - Agentic-Workflows-as-Code is an **artifact** axis (how the structure is expressed and persisted);
  - it *applies* at every rung — you can version a prompt — but only becomes **unavoidable** at the
    graph rung, because that is where the thing being versioned stops being text and becomes a
    program;
  - hence its position at the end of the ladder is honest, but the reader should understand it as a
    turn, not a step.

## 9. Choosing a rung (the practical section)

- **You do not need a graph for a loop-shaped problem.** Most work is a loop.
- Signals you have outgrown each rung:
  - prompt → context: the model is confidently wrong about things it was never shown;
  - context → loop: the task does not fit one window, or the session degrades before it finishes;
  - loop → graph: you are paying for checks that cannot pass, or you want "if X then different work";
  - anything → as-code: someone else needs to run, review, or trust it.
- **The cost of over-engineering the rung** — a graph you don't need is a language you now maintain.

## 10. What none of this fixes (the honest section)

- **The agent is still stochastic.** Only the *orchestration* is deterministic. Do not promise more.
- **Goodhart's law applies to verifiers.** A check is a target; agents optimise the check, not the
  intent. Concrete failure to cite: an agent shrinking an artifact to fit a *timing out* grader
  rather than improving it.
- **Side effects escape the model.** Anything outside the harness re-executes on replay and is not
  transactional.
- **A judge that errors is not a judge that failed** — conflating them silently burns budget.
- **Cost is real** — the honest number for a long autonomous run, stated plainly.

## 11. Prior art and lineage

- Ralph loop — Huntley (2025); a history of the pattern; note it has no formal literature, which is
  itself interesting.
- Prompt / context engineering — established, with encyclopedia coverage.
- Flow engineering, agentic graph compilation, agent-harness evolution — the emerging academic
  strand (arXiv), positioned as parallel discovery rather than lineage.
- Infrastructure as Code — the analogy that carries the last section.
- Evolutionary code search (generate → verify → keep) — the same loop, arrived at years earlier from
  a different direction.

## 12. Glossary

- One line each, precise, so the terms are quotable: *harness · orchestration · loop · graph ·
  verifier/judge · definition of done · fresh session · artifact*.

---

## Editorial notes (delete before publishing)

- **Keep it vendor-neutral until the end.** Earn every concept first; a tool named early makes the
  whole piece read as marketing. One short closing section — "what this looks like implemented" — is
  enough, and lands harder for having waited.
- **The fixed per-section shape is the article's spine:** *Definition → the gap it closes → where it
  runs out.* Do not vary it; the repetition is what makes the ladder feel inevitable rather than
  asserted.
- **Strongest single sentence to build §7 around:** *a loop is the degenerate graph.*
- **Most likely objection to pre-empt:** "this is just workflow engines with LLMs". Answer: the
  verifier is the difference — a workflow engine does not have to defend against the worker gaming
  the exit condition.
- **Open question to settle before drafting:** does Context Engineering deserve its own rung, or is
  it the second half of Prompt Engineering? Wikipedia currently treats it as a *subsection* of prompt
  engineering. Keeping it separate makes the ladder five rungs and the progression cleaner; merging
  makes it four and harder to argue with.

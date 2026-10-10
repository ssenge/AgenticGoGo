# Changelog

## Unreleased

### Added
- **`agg wiki lint [PATH] [--json]`**: checks a knowledge wiki (default: the worker's
  `agg/state/wiki/`) against OKF v0.2. Problems, which fail the lint with exit 1: a concept without
  parseable frontmatter or a non-empty `type`; an `index.md` that is not link entries under headings,
  or carries frontmatter beyond a root `okf_version`; a `log.md` whose headings are not `YYYY-MM-DD`
  dates, newest first. Warnings: the shape of the optional `status`, `generated`, `verified`,
  `sources` and timestamp fields, and a `verified` key in the worker's own wiki. Read-only. JSON
  schema: `docs/schemas/wiki_lint.v1.json`.
- **`docs/BACKENDS.md`**: how to add an agent backend: the `AgentBackend` contract, the capability
  flags and what each refuses, stream and one-shot parsing, usage and model reporting, rate-limit
  detection, recorded fixtures and the required tests.
- **`docs/backends/kiro.md`**: the Kiro CLI was investigated as a backend and is not implemented. Its
  headless mode exists, but the event schema of its JSON stream, token reporting, rate-limit
  signalling and a read-only one-shot that ignores repo configuration are undocumented, and no
  stream could be recorded.
- **Judges built on swe-metrics** (A-20). A judge can be DECLARED in `judges:` instead of written as a
  file: `metric: { id, tool?, stat?, labels?, scope?, combine? }` reads one number of a
  [swe-metrics](https://github.com/ssenge/swe-metrics) report, `score: { definition, version }` its
  0–100 quality score, each held to `max:`/`min:`.
  - All metric and score judges of a session share ONE `swe-metrics measure` of the session's tree; the
    report and the tools' SARIF are kept as evidence.
  - Fail closed: a skipped or failed analyzer, or an absent metric, makes the judge error with the
    analyzer's reason — never met.
  - Several tools for one metric: `tool:` or `combine: strictest | all | majority`; with `all`, tools
    moving in different directions are no improvement for the ratchet (the verdict's new optional
    `mixed` field).
  - `scope: changed` measures only the files changed since the base branch; a sub-path scope measures a
    directory of the project. With no changed file the analyzer applies to (nothing changed, or only
    docs), the judge is vacuously met (`met: true`, no `value`, the verdict's new optional `vacuous`
    field) — no improvement, no regression, and not a "was met" reading at the gate. Real failures in
    the changed scope still error.
  - A score is computed against the incumbent's report (`agg/private/metrics/incumbent/`); a score
    capped by a failed gate names the gates; a definition/version mismatch is refused; a `null` score is
    an error with the score's notes; a changed definition resets the ratchet reference on `--resume`.
  - A ratchet on a metric or score judge may omit `direction` (the judge states it; the report's
    direction is checked against it).
  - swe-metrics stays optional: a config declaring such a judge is refused at startup when
    `swe-metrics` is not on `PATH` or speaks another schema version.
  - Rust driver: `Judge::metric(name, MetricJudge::new(id)…)` and `Judge::score(name, ScoreJudge::new(def, version)…)`.

### Changed
- The worker's brief now shows the reserved `index.md` and `log.md` shapes next to the concept
  example, says never to write `verified`, no longer suggests the OKF v0.1 `timestamp` key, and points
  at `agg wiki lint`. A worker that copies the examples writes a conformant wiki (tested). agg still
  writes nothing into the wiki.
- The brief's "start with" hint for the wiki lists `index.md` first when there is one.
- `sequence.ratchet.<judge>.direction` is optional for metric and score judges (still required for
  every other judge — refused at startup without it, now located at `sequence.ratchet.<judge>.direction`).
- `judges.<name>` accepts `metric`, `score`, `max`, `min` beside `timeout`.

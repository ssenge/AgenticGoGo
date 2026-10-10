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

### Changed
- The worker's brief now shows the reserved `index.md` and `log.md` shapes next to the concept
  example, says never to write `verified`, no longer suggests the OKF v0.1 `timestamp` key, and points
  at `agg wiki lint`. A worker that copies the examples writes a conformant wiki (tested). agg still
  writes nothing into the wiki.
- The brief's "start with" hint for the wiki lists `index.md` first when there is one.

# Changelog

Notable changes to agg. Newest first; each entry names the config keys and interfaces it touches.

## Unreleased

### Added

- **Read-only context directories — `context:`.** A list of `{ path, as, mode: ro }`: directories from
  outside the project (a knowledge base, reference material) that every confined process reads and
  none may change. `sandbox` keeps them read-only in the kernel (macOS: a Seatbelt write deny plus an
  unlink deny on each ancestor, so a context cannot be renamed away and replaced; Linux: `bwrap
  --ro-bind`), `container` mounts them `:ro` at `/context/<as>`. The brief lists each one with a pointer
  to its `index.md`; every confined process, judges included, finds it as `AGG_CONTEXT_<AS>`. Refused at
  startup, each at its key: a missing path, a file, an overlap with the project, a malformed or
  duplicate `as`, a `mode` other than `ro`, and any `isolation: none` step. Driver: `Agg::context(path,
  name)`. See `docs/CONFIG.md` → *Context directories*.

//! agg's OWN features — each a plugin (`Handler`) living in its own module, registered on hooks by
//! `Lifecycle::default_pipeline`. They touch the core ONLY through the public plugin API in `loop_`
//! (the `LoopState` context, `Extensions`, `emit`/`charge`), exactly as a third-party plugin does
//! (see `tests/plugin_api.rs`). The core file knows nothing about what any of them do — proving
//! "agg is just the built-in plugin" (LOOPSTATE_REDESIGN §3.1 / HOOK_REDESIGN §1) in the layout itself.

pub mod summary;

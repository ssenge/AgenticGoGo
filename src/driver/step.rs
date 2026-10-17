//! [`Step`] — what a driver hands to `agg.step(&s)`, and the TEMPLATE mechanism that keeps a
//! family of steps from repeating its own protections.
//!
//! # The merge rules, in one place
//!
//! | field | rule |
//! |---|---|
//! | `agent` `model` `effort` `isolation` `image` `prompt` `state` `memory` `resume` | **OVERRIDE** — the derived step wins |
//! | `prompt` / `prompt_file` | **OVERRIDE AS A PAIR** — setting one clears the other |
//! | `role_prompt` / `role_prompt_file` | **OVERRIDE AS A PAIR** — setting one clears the other; neither set = the run's defaults, as in YAML |
//! | `readonly` | **ACCUMULATES** — a derived step cannot silently lose a deny its template set |
//! | `writable` | **SUBTRACTS** from what accumulated — re-grant exactly one deny, not the rest |
//! | `write_deny` | **ACCUMULATES**, and nothing subtracts from it — not even `writable` |
//!
//! That asymmetry is the whole reason templates are worth having. A YAML project repeats
//! `isolation: sandbox` + `readonly: [tests/, agg/judges/]` on every step that needs it, and the
//! fourth such step, added six months later, quietly forgets the line. A template makes the
//! protection the default of the family and the re-grant the exception a reader can see.

use crate::driver::{Agent, AgentSession, Effort};
use crate::isolation::{denied_paths, normalize_paths, Isolation};

/// One step of a driver's flow: which agent, on what model, confined how, with what prompt.
///
/// Construct it named ([`Step::new`]) or unnamed as a template ([`Step::template`], then
/// [`Step::create`]). Every setter CONSUMES `self`, so a step is one `let` binding and one chain.
///
/// Fields are `pub` (this crate has no facade), but the setters are what enforce the merge rules
/// and the path normalisation — assigning `step.readonly` directly replaces the list instead of
/// accumulating, and stores whatever spelling you wrote.
#[derive(Debug, Clone, Default)]
pub struct Step {
    /// `None` for a TEMPLATE. An unnamed step exists only to be [`Step::create`]d from; the name is
    /// what the banner, the dashboard's `cur_step` and `StepOutcome.step` all show.
    pub name: Option<String>,
    pub agent: Agent,
    /// `None` = the backend's own default model. A `String` and not an enum: new models arrive
    /// weekly and an enum would need an agg release for each one.
    pub model: Option<String>,
    pub effort: Effort,
    /// ⚠ [`Isolation::None`] (the default) means `readonly`/`writable` bind to NOTHING — the deny
    /// list is delivered by the OS wrapper and the wrapper only runs under `sandbox`/`container`.
    /// agg warns when it sees a list on an unconfined step; it does not silently pretend.
    pub isolation: Isolation,
    /// the image this step runs in under [`Isolation::Container`]. `None` = the run's
    /// ([`crate::driver::Agg::image`], else the base image). Inert on every other tier.
    pub image: Option<String>,
    /// paths this step may read but not write, normalised and accumulated. See [`Step::readonly`].
    pub readonly: Vec<String>,
    /// paths subtracted from [`Self::readonly`]. See [`Step::writable`].
    pub writable: Vec<String>,
    /// PROTECTED globs, accumulated and never subtracted. See [`Step::write_deny`].
    pub write_deny: Vec<String>,
    /// the step's ROLE — who the worker is — opening the composed brief. `None` (with
    /// [`Self::role_prompt_file`] also `None`) = the run's defaults. See [`Step::role_prompt`].
    pub role_prompt: Option<String>,
    /// [`Self::role_prompt`] read from a FILE at every session start — relative to `agg/`, or
    /// absolute. See [`Step::role_prompt_file`].
    pub role_prompt_file: Option<String>,
    /// ADDITIVE to the composed brief, never replacing it.
    pub prompt: Option<String>,
    /// [`Self::prompt`] read from a FILE at every session start — relative to `agg/`, or absolute.
    /// See [`Step::prompt_file`].
    pub prompt_file: Option<String>,
    /// the forward-state file this step's worker rewrites. `None` = `"state/STATE.md"` under the
    /// config base, as YAML's `defaults.state` resolves it.
    pub state: Option<String>,
    /// whether this step's sessions keep agg's working memory. `None` = the run's
    /// ([`crate::driver::Agg::memory`], on by default). See [`Step::memory`].
    pub memory: Option<bool>,
    /// the agent session this step's session continues, and whether it branches off it (`true`)
    /// instead of appending to it. `None` = a fresh session. See [`Step::resume`] and [`Step::fork`].
    pub resume: Option<(AgentSession, bool)>,
}

impl Step {
    /// A named step, with everything else at its default (Claude, the backend's own model and
    /// effort, [`Isolation::None`], no denies, no prompt).
    pub fn new(name: impl Into<String>) -> Step {
        Step { name: Some(name.into()), ..Step::default() }
    }

    /// An UNNAMED step — a family's shared shape. This is YAML's `defaults:` block, except YAML has
    /// exactly one and a driver can have as many families as its workflow has kinds of step.
    ///
    /// A template is never runnable: `agg.step()` takes a named step, and the way to name one is
    /// [`Step::create`].
    pub fn template() -> Step {
        Step::default()
    }

    /// Clone this template into a real, NAMED step. Takes `&self` so one template creates many.
    ///
    /// ⚠ It is `create`, not `named`: the template is not being renamed, it is being instantiated,
    /// and the two verbs mislead about whether the template survives (it does).
    pub fn create(&self, name: impl Into<String>) -> Step {
        Step { name: Some(name.into()), ..self.clone() }
    }

    // ---- scalars: OVERRIDE ----

    pub fn agent(mut self, a: Agent) -> Step {
        self.agent = a;
        self
    }
    pub fn model(mut self, m: impl Into<String>) -> Step {
        self.model = Some(m.into());
        self
    }
    pub fn effort(mut self, e: Effort) -> Step {
        self.effort = e;
        self
    }
    pub fn isolation(mut self, i: Isolation) -> Step {
        self.isolation = i;
        self
    }
    /// The image this step runs in under [`Isolation::Container`] — one with the step's agent CLI
    /// in it (the image contract, `docs/CONFIG.md`). The twin of YAML's `image:`.
    pub fn image(mut self, image: impl Into<String>) -> Step {
        self.image = Some(image.into());
        self
    }
    /// The step's role text — the twin of YAML's per-step `role_prompt:`. Replaces a
    /// [`Step::role_prompt_file`] the template set, and the run's defaults for this step.
    pub fn role_prompt(mut self, p: impl Into<String>) -> Step {
        self.role_prompt = Some(p.into());
        self.role_prompt_file = None;
        self
    }

    /// The step's role text, from a FILE (relative to `agg/`, or absolute) — the twin of YAML's
    /// per-step `role_prompt_file:`. Read at EVERY session start and hashed into
    /// `agg/private/prompts.jsonl`, like [`Step::prompt_file`]. Replaces a [`Step::role_prompt`]
    /// the template set; `agg.step()` refuses a step with both, or an unreadable file.
    pub fn role_prompt_file(mut self, rel: impl Into<String>) -> Step {
        self.role_prompt_file = Some(rel.into());
        self.role_prompt = None;
        self
    }

    /// The step's task text. Replaces a [`Step::prompt_file`] the template set.
    pub fn prompt(mut self, p: impl Into<String>) -> Step {
        self.prompt = Some(p.into());
        self.prompt_file = None;
        self
    }

    /// The step's task text, from a FILE (relative to `agg/`, or absolute) — the twin of YAML's
    /// `prompt_file:`. Read at EVERY session start, so an edit applies from the next session, and
    /// its SHA-256 is recorded per session in `agg/private/prompts.jsonl`. Replaces a
    /// [`Step::prompt`] the template set; `agg.step()` refuses a step with both, or an unreadable file.
    pub fn prompt_file(mut self, rel: impl Into<String>) -> Step {
        self.prompt_file = Some(rel.into());
        self.prompt = None;
        self
    }

    /// The forward-state file this step's worker rewrites, relative to the config base
    /// (`agg/`) — the twin of YAML's `state:` key, and a field rather than prose because four
    /// consumers a `.prompt()` cannot reach read it: the `{{STATE}}` footer, the staleness snapshot,
    /// the "worker did not update its forward state" warning, and the exit brief.
    ///
    /// ⚠ It can escape `agg/` (`"../docs/STATE.md"`). True of the YAML key today; inherited here,
    /// not created, and deliberately not validated.
    pub fn state(mut self, rel: impl Into<String>) -> Step {
        self.state = Some(rel.into());
        self
    }

    /// Whether this step's sessions keep agg's WORKING MEMORY: the forward state, the wiki, the
    /// memory log and the summary. On by default; it pays off where one step runs session after
    /// session. Off suits a step that runs once and hands its result on in its own files: its brief
    /// then names no state, wiki or memory, and no fold or summary runs after it. Off never deletes
    /// memory — a later step with it on sees what earlier sessions left. Overrides [`crate::driver::Agg::memory`].
    pub fn memory(mut self, on: bool) -> Step {
        self.memory = Some(on);
        self
    }

    /// CONTINUE an earlier step's agent session: this step's worker starts with that session's
    /// context instead of a fresh one, and appends to it (A-33). Only the agent that ran the session
    /// can continue it; on another agent, a backend that can't resume, or a session the agent no
    /// longer has, the step says so and starts fresh. The context is a cache, never the record: the
    /// step must still pass started fresh.
    pub fn resume(mut self, session: &AgentSession) -> Step {
        self.resume = Some((session.clone(), false));
        self
    }

    /// BRANCH OFF an earlier step's agent session: like [`Step::resume`], but the session itself
    /// stays as it was, so several steps can each start from it, and a rerun starts from the same
    /// context. A backend that can't fork starts fresh, as [`Step::resume`] does.
    pub fn fork(mut self, session: &AgentSession) -> Step {
        self.resume = Some((session.clone(), true));
        self
    }

    // ---- path lists: ACCUMULATE / SUBTRACT ----

    /// ADD paths this step may read but not write. Accumulates over whatever a template already set.
    ///
    /// Every entry is normalised on the way in ([`crate::isolation::normalize_path`]), so
    /// `"agg/judges/"`, `"agg/judges"` and `"./agg/judges"` are one path and [`Step::writable`]'s
    /// subtraction cannot miss by a trailing slash. An entry with no canonical form (one that climbs
    /// above the project root, or names the root itself) is dropped with a warning.
    pub fn readonly<I, S>(mut self, paths: I) -> Step
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        extend_unique(&mut self.readonly, normalize_paths(paths));
        self
    }

    /// RE-GRANT paths out of what [`Step::readonly`] accumulated — the exception a reader can see.
    ///
    /// Subtraction is exact on the normalised spelling, so `writable(["src/foo"])` does not carve a
    /// hole in `readonly(["src"])`. It only ever removes: it cannot widen a step's writable set
    /// beyond what the tier already grants.
    pub fn writable<I, S>(mut self, paths: I) -> Step
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        extend_unique(&mut self.writable, normalize_paths(paths));
        self
    }

    /// ADD protected paths: globs (`*`, `?`, `**`) this step may read but never change — the twin
    /// of YAML's `write_deny:`. Accumulates over the template's list and is NEVER reduced:
    /// [`Step::writable`] does not reach it, so a derived step cannot lose a protection.
    ///
    /// Unlike `readonly`, it binds on EVERY tier: the gate rolls back a session that changed a
    /// covered path even under [`Isolation::None`]; a confining tier also denies the write itself.
    /// See [`crate::protect`].
    pub fn write_deny<I, S>(mut self, patterns: I) -> Step
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let merged = crate::protect::union(&self.write_deny, &crate::protect::normalize(patterns));
        self.write_deny = merged;
        self
    }

    /// The paths this step may NOT write: `readonly` minus `writable`. What the OS wrapper is
    /// handed, beside — never instead of — its derived `agg/private/` carve-out.
    pub fn denied(&self) -> Vec<String> {
        denied_paths(&self.readonly, &self.writable)
    }
}

/// Append, skipping what is already there. A repeated path is not two denies, and an accumulating
/// template chain repeats by construction (`sandboxed.create(..).readonly([..])`).
fn extend_unique(dst: &mut Vec<String>, more: Vec<String>) {
    for p in more {
        if !dst.contains(&p) {
            dst.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `prompt` and `prompt_file` override AS A PAIR: a derived step's choice replaces the
    /// template's, so a step can never inherit both forms.
    #[test]
    fn prompt_and_prompt_file_replace_each_other() {
        let filed = Step::template().prompt_file("steps/w.md");
        let inline = filed.create("a").prompt("do it");
        assert_eq!((inline.prompt.as_deref(), inline.prompt_file.as_deref()), (Some("do it"), None));
        let back = inline.prompt_file("steps/x.md");
        assert_eq!((back.prompt.as_deref(), back.prompt_file.as_deref()), (None, Some("steps/x.md")));
    }

    /// `role_prompt` and `role_prompt_file` override AS A PAIR too, and independently of the task
    /// prompt's pair.
    #[test]
    fn role_prompt_and_role_prompt_file_replace_each_other() {
        let filed = Step::template().role_prompt_file("roles/tester.md").prompt("do it");
        let inline = filed.create("a").role_prompt("you are a tester");
        assert_eq!((inline.role_prompt.as_deref(), inline.role_prompt_file.as_deref()), (Some("you are a tester"), None));
        assert_eq!(inline.prompt.as_deref(), Some("do it"), "the task prompt is a separate pair");
        let back = inline.role_prompt_file("roles/x.md");
        assert_eq!((back.role_prompt.as_deref(), back.role_prompt_file.as_deref()), (None, Some("roles/x.md")));
    }

    /// A template survives being created from, and the child carries the template's shape.
    #[test]
    fn create_instantiates_a_template_without_consuming_it() {
        let sandboxed = Step::template()
            .agent(Agent::Claude)
            .isolation(Isolation::Sandbox)
            .readonly(["tests/", "agg/judges/"]);

        let a = sandboxed.create("implement");
        let b = sandboxed.create("fix");

        assert_eq!(a.name.as_deref(), Some("implement"));
        assert_eq!(b.name.as_deref(), Some("fix"));
        assert_eq!(sandboxed.name, None, "a template stays unnamed and reusable");
        assert_eq!(a.isolation, Isolation::Sandbox);
        assert_eq!(a.readonly, ["tests", "agg/judges"]);
        assert_eq!(b.readonly, ["tests", "agg/judges"]);
    }

    /// Scalars OVERRIDE: the derived step wins, and the template is untouched.
    #[test]
    fn scalars_override() {
        let tmpl = Step::template().agent(Agent::Claude).effort(Effort::Low).model("cheap");
        let derived = tmpl.create("spec").agent(Agent::Codex).effort(Effort::High).model("pricey");

        assert_eq!(derived.agent.name(), "codex");
        assert_eq!(derived.effort, Effort::High);
        assert_eq!(derived.model.as_deref(), Some("pricey"));
        assert_eq!(tmpl.agent.name(), "claude", "the template is not mutated by its child");
        assert_eq!(tmpl.effort, Effort::Low);
    }

    /// `readonly` ACCUMULATES: a derived step cannot silently drop a deny its template set.
    #[test]
    fn readonly_accumulates_down_a_template_chain() {
        let tmpl = Step::template().isolation(Isolation::Sandbox).readonly(["tests/", "agg/judges/"]);
        let derived = tmpl.create("author_judge").readonly(["src/"]);

        assert_eq!(derived.readonly, ["tests", "agg/judges", "src"]);
        assert_eq!(tmpl.readonly, ["tests", "agg/judges"], "the template keeps its own list");
        // repeating a path the template already denies is not a second deny
        assert_eq!(tmpl.create("x").readonly(["tests"]).readonly(["tests/"]).readonly, ["tests", "agg/judges"]);
    }

    /// `writable` SUBTRACTS from what accumulated — re-grant one deny, keep the rest.
    #[test]
    fn writable_subtracts_from_what_readonly_accumulated() {
        let sandboxed = Step::template().isolation(Isolation::Sandbox).readonly(["tests/", "agg/judges/"]);

        // the step that SHOULD add tests re-grants exactly that one; `agg/judges/` stays denied.
        let implement = sandboxed.create("implement").writable(["tests/"]);
        assert_eq!(implement.denied(), ["agg/judges"]);

        // the only step that may write graders — and `src/` is denied on top.
        let author = sandboxed.create("author_judge").readonly(["src/"]).writable(["agg/judges/"]);
        assert_eq!(author.denied(), ["tests", "src"]);

        // a sibling that re-grants nothing keeps both.
        assert_eq!(sandboxed.create("fix").denied(), ["tests", "agg/judges"]);
    }

    /// ⚠ THE TRAILING SLASH. Subtraction compares strings; without normalisation on input, this
    /// case subtracts NOTHING while reading to its author exactly like it worked.
    #[test]
    fn a_trailing_slash_cannot_break_the_subtraction() {
        let s = Step::template()
            .isolation(Isolation::Sandbox)
            .readonly(["agg/judges/", "src"])   // WITH the slash
            .create("author_judge")
            .writable(["agg/judges", "./src/"]); // WITHOUT it, and with a `./`

        assert_eq!(s.writable, ["agg/judges", "src"]);
        assert!(s.denied().is_empty(), "both denies were re-granted: {:?}", s.denied());
    }

    /// A path with no canonical form is dropped, not stored — storing it would put an entry in the
    /// deny list that no `writable` spelling can ever subtract.
    #[test]
    fn an_uncanonical_path_is_dropped() {
        let s = Step::new("x").readonly(["../escape", "src/", ".", "agg/judges/../judges"]);
        assert_eq!(s.readonly, ["src", "agg/judges"]);
    }

    /// `write_deny` ACCUMULATES and `writable` cannot touch it — the one list a derived step can
    /// never shrink.
    #[test]
    fn write_deny_accumulates_and_writable_cannot_subtract_from_it() {
        let tmpl = Step::template().write_deny(["tests/oracles/"]);
        let s = tmpl.create("implement").write_deny(["**/*.golden", "./tests/oracles/**"]).writable(["tests/oracles"]);
        assert_eq!(s.write_deny, ["tests/oracles/**", "**/*.golden"]);
        assert_eq!(tmpl.write_deny, ["tests/oracles/**"], "the template keeps its own list");
    }

    /// `Step::new` and `Step::template().create(..)` differ in exactly one thing: the template's
    /// accumulated shape.
    #[test]
    fn new_is_an_empty_template_that_was_named() {
        let a = Step::new("document").prompt("write the docs");
        let b = Step::template().create("document").prompt("write the docs");
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }
}

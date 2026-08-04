//! [`Step`] — what a driver hands to `agg.step(&s)`, and the TEMPLATE mechanism that keeps a
//! family of steps from repeating its own protections.
//!
//! # The merge rules, in one place
//!
//! | field | rule |
//! |---|---|
//! | `agent` `model` `effort` `isolation` `prompt` `state` | **OVERRIDE** — the derived step wins |
//! | `readonly` | **ACCUMULATES** — a derived step cannot silently lose a deny its template set |
//! | `writable` | **SUBTRACTS** from what accumulated — re-grant exactly one deny, not the rest |
//!
//! That asymmetry is the whole reason templates are worth having. A YAML project repeats
//! `isolation: sandbox` + `readonly: [tests/, agg/judges/]` on every step that needs it, and the
//! fourth such step, added six months later, quietly forgets the line. A template makes the
//! protection the default of the family and the re-grant the exception a reader can see.

use crate::driver::{Agent, Effort};
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
    /// paths this step may read but not write, normalised and accumulated. See [`Step::readonly`].
    pub readonly: Vec<String>,
    /// paths subtracted from [`Self::readonly`]. See [`Step::writable`].
    pub writable: Vec<String>,
    /// ADDITIVE to the composed brief, never replacing it.
    pub prompt: Option<String>,
    /// the forward-state file this step's worker rewrites. `None` = `"state/STATE.md"` under the
    /// config base, as YAML's `defaults.state` resolves it.
    pub state: Option<String>,
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
    pub fn prompt(mut self, p: impl Into<String>) -> Step {
        self.prompt = Some(p.into());
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

    /// `Step::new` and `Step::template().create(..)` differ in exactly one thing: the template's
    /// accumulated shape.
    #[test]
    fn new_is_an_empty_template_that_was_named() {
        let a = Step::new("document").prompt("write the docs");
        let b = Step::template().create("document").prompt("write the docs");
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }
}

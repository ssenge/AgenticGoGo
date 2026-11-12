//! `agg process <file> --check` (A-37 §3): everything about a process file that can be told without running it,
//! listed at once, each with where it is: unknown ids, keys a step's kind doesn't take, files that aren't there,
//! `{{ names }}` nobody fills in, steps the file refers to that it doesn't have. `agg process` refuses to run on
//! any finding.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::format::{Check, Kind, Phase, Process, Review, Section, Step};
use super::number::{number, Numbered};
use super::template;

/// Every problem of the process file at `file`, none when it is sound.
pub fn check(file: &Path) -> Vec<String> {
    let home = file.parent().map(Path::to_path_buf).unwrap_or_default();
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => return vec![format!("{}: {e}", file.display())],
    };
    let process = match Process::parse(&text) {
        Ok(p) => p,
        Err(e) => return vec![format!("{}: {e}", file.display())],
    };
    let list = number(&process);
    let vars = match template::variables(&process, &list, &home, "<project>") {
        Ok(v) => v,
        Err(e) => return vec![e],
    };
    let mut ctx = Ctx { process: &process, home, vars, ids: BTreeSet::new(), steps: BTreeSet::new(), out: vec![] };
    ctx.run(&list);
    ctx.out
}

struct Ctx<'a> {
    process: &'a Process,
    home: PathBuf,
    vars: BTreeMap<String, String>,
    /// every phase and step id
    ids: BTreeSet<String>,
    /// the ids of steps that run (not `each`)
    steps: BTreeSet<String>,
    out: Vec<String>,
}

impl Ctx<'_> {
    fn run(&mut self, list: &[Numbered]) {
        if self.process.phases.is_empty() {
            self.out.push("phases: none".into());
        }
        for phase in &self.process.phases {
            self.id(&phase.id, "phase");
            collect(&phase.steps, &mut |s| {
                if s.kind != Kind::Each {
                    self.steps.insert(s.id.clone());
                }
            });
        }
        let phases: Vec<&Phase> = self.process.phases.iter().collect();
        for phase in phases {
            self.phase(phase);
        }
        self.top_level(list);
    }

    fn id(&mut self, id: &str, what: &str) {
        let ok = id.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !ok {
            self.out.push(format!("{what} id `{id}`: an id is lower-case letters, digits and `-`, starting with a letter"));
        }
        if !self.ids.insert(id.to_string()) {
            self.out.push(format!("{what} id `{id}`: used twice (ids are unique over phases and steps)"));
        }
    }

    fn phase(&mut self, phase: &Phase) {
        if phase.steps.is_empty() {
            self.out.push(format!("phase `{}`: no steps", phase.id));
        }
        for c in phase.pre.iter().chain(&phase.post) {
            self.command(&format!("phase `{}`", phase.id), c, false);
        }
        for step in &phase.steps {
            self.step(step, false);
        }
    }

    fn step(&mut self, s: &Step, in_each: bool) {
        let at = format!("step `{}`", s.id);
        self.id(&s.id, "step");
        if s.name.trim().is_empty() {
            self.out.push(format!("{at}: no name"));
        }
        self.keys(s, &at);
        for c in s.pre.iter().chain(&s.post) {
            self.command(&at, c, in_each);
        }
        for w in s.when.iter().flat_map(|w| w.all()) {
            self.command(&at, w, in_each);
        }
        for d in &s.deny {
            self.deny(&at, d);
        }
        if let Some(t) = &s.constraints {
            self.text(&at, t, in_each);
        }
        if let Some(settings) = &s.settings {
            self.settings(&at, settings);
        }
        match s.kind {
            Kind::Script => self.opt_command(&at, s.run.as_deref(), in_each),
            Kind::Agent => self.agent(s, &at, in_each),
            Kind::Checkpoint => self.checkpoint(s, &at, in_each),
            Kind::Each => self.each(s, &at),
            Kind::Exam => self.exam(s, &at, in_each),
            Kind::Outside | Kind::Planned => {}
        }
    }

    /// The keys a step's kind takes: the others are a mistake, not an ignored setting.
    fn keys(&mut self, s: &Step, at: &str) {
        let set = |names: &[(&str, bool)]| -> Vec<String> {
            names.iter().filter(|(_, on)| *on).map(|(n, _)| (*n).to_string()).collect()
        };
        let mine = set(&[("run", s.run.is_some()), ("prompt", s.prompt.is_some()), ("checks", !s.checks.is_empty()),
                         ("review", s.review.is_some()), ("repairs", s.repairs.is_some()), ("sections", !s.sections.is_empty()),
                         ("quality", s.quality), ("on_fail", s.on_fail.is_some()), ("prepare", s.prepare.is_some()),
                         ("doc", s.doc.is_some()), ("question", s.question.is_some()), ("owner", s.owner.is_some()),
                         ("go_live", s.go_live), ("items", s.items.is_some()), ("steps", !s.steps.is_empty()),
                         ("examinee", s.examinee.is_some()), ("answers", s.answers.is_some()), ("grader", s.grader.is_some()),
                         ("repairer", s.repairer.is_some()), ("rounds", s.rounds.is_some()), ("fresh", !s.fresh.is_empty()),
                         ("findings_ids", s.findings_ids.is_some()), ("dispute", s.dispute.is_some())]);
        let takes: &[&str] = match s.kind {
            Kind::Script => &["run"],
            Kind::Agent => &["prompt", "checks", "review", "repairs", "sections", "quality", "on_fail"],
            Kind::Checkpoint => &["prepare", "doc", "question", "owner", "go_live"],
            Kind::Each => &["items", "steps"],
            Kind::Exam => &["checks", "examinee", "answers", "grader", "repairer", "rounds", "fresh", "findings_ids", "dispute", "repairs"],
            Kind::Outside | Kind::Planned => &[],
        };
        for key in mine.iter().filter(|k| !takes.contains(&k.as_str())) {
            self.out.push(format!("{at}: `{key}` doesn't belong to a {:?} step", s.kind));
        }
        let needs: &[&str] = match s.kind {
            Kind::Script => &["run"],
            Kind::Checkpoint => &["prepare", "doc", "question"],
            Kind::Each => &["items", "steps"],
            Kind::Exam => &["examinee", "answers", "grader", "repairer"],
            _ => &[],
        };
        for key in needs.iter().filter(|k| !mine.iter().any(|m| m == *k)) {
            self.out.push(format!("{at}: a {:?} step needs `{key}`", s.kind));
        }
    }

    fn agent(&mut self, s: &Step, at: &str, in_each: bool) {
        if self.process.roles.is_none() {
            self.out.push(format!("{at}: an agent step needs the process's `roles`"));
        }
        let prompt = s.prompt.clone().unwrap_or_else(|| format!("{}/{}.md", self.process.dirs.prompts, s.id));
        self.file(at, "prompt", &prompt, &[]);
        match &s.review {
            Some(Review::File(f)) => self.file(at, "review", f, &[]),
            Some(Review::On(true)) => self.file(at, "review", &format!("{}/{}-review.md", self.process.dirs.reviews, s.id), &[]),
            _ => {}
        }
        self.checks(at, &s.checks, in_each);
        for section in &s.sections {
            self.section(at, section, in_each);
        }
        self.opt_command(at, s.on_fail.as_deref(), in_each);
    }

    fn checkpoint(&mut self, s: &Step, at: &str, in_each: bool) {
        self.opt_command(at, s.prepare.as_deref(), in_each);
        if let Some(q) = &s.question {
            self.text(at, q, in_each);
        }
        if let Some(owner) = &s.owner {
            self.step_ref(at, "owner", owner);
            self.owner_reads_decisions(at, s, owner);
        }
    }

    /// A decision taken in a checkpoint's yes reaches its owner only as a change to what the owner reads: an owner with
    /// declared inputs that don't cover the decisions file would be refreshed without a session and never see it.
    fn owner_reads_decisions(&mut self, at: &str, cp: &Step, owner: &str) {
        let Some(step) = find(&self.process.phases.iter().flat_map(|p| p.steps.iter()).collect::<Vec<_>>(), owner) else { return };
        let Some(inputs) = step.settings.as_ref().and_then(|s| s.inputs.as_ref()) else { return };
        let doc = cp.doc.clone().unwrap_or_default();
        let dir = Path::new(&doc).parent().map(|p| p.display().to_string()).unwrap_or_default();
        let decisions = if dir.is_empty() { format!("{}-decisions.md", cp.id) } else { format!("{dir}/{}-decisions.md", cp.id) };
        let covers = |entry: &str| match entry.split_once('*') {
            Some((pre, post)) => decisions.starts_with(pre) && decisions.ends_with(post),
            None => entry == decisions || (entry.ends_with('/') && decisions.starts_with(entry)),
        };
        if !inputs.iter().any(|i| template::render(i, &self.vars).is_ok_and(|i| covers(&i))) {
            self.out.push(format!("{at}: its owner `{owner}` doesn't read `{decisions}` among its `settings.inputs`, so a decision \
                taken in the yes would never reach it"));
        }
    }

    fn each(&mut self, s: &Step, at: &str) {
        if let Some(items) = &s.items {
            self.text(at, &items.file, false);
        }
        for child in &s.steps {
            if child.kind == Kind::Each {
                self.out.push(format!("step `{}`: an `each` inside an `each`", child.id));
            }
            self.step(child, true);
        }
    }

    fn exam(&mut self, s: &Step, at: &str, in_each: bool) {
        if self.process.roles.is_none() {
            self.out.push(format!("{at}: an exam step needs the process's `roles`"));
        }
        self.checks(at, &s.checks, in_each);
        self.opt_command(at, s.answers.as_deref(), in_each);
        if let Some(g) = &s.grader {
            self.file(at, "grader", g, &[]);
        }
        for (what, w) in [("examinee", &s.examinee), ("repairer", &s.repairer)] {
            let Some(w) = w else { continue };
            self.file(at, &format!("{what}.prompt"), &w.prompt, &[]);
            for section in &w.sections {
                self.section(at, section, in_each);
            }
            for d in &w.deny {
                self.deny(at, d);
            }
        }
        if let Some(d) = &s.dispute {
            self.step_ref(at, "dispute.reenter", &d.reenter);
        }
        if let Some(f) = &s.findings_ids {
            self.text(at, &f.file, false);
        }
    }

    fn checks(&mut self, at: &str, checks: &[Check], in_each: bool) {
        let mut names = BTreeSet::new();
        for c in checks {
            self.command(at, c.command(), in_each);
            if c.last_only() && !in_each {
                self.out.push(format!("{at}: `when: last` only means something inside an `each`"));
            }
            if let Some(n) = c.name().filter(|n| !names.insert(n.to_string())) {
                self.out.push(format!("{at}: two checks are named `{n}`"));
            }
        }
    }

    fn section(&mut self, at: &str, s: &Section, in_each: bool) {
        let set = [s.text.is_some(), s.file.is_some(), s.run.is_some()].iter().filter(|b| **b).count();
        if set != 1 {
            self.out.push(format!("{at}: a section has exactly one of `text`, `file`, `run`"));
        }
        if let Some(t) = &s.text {
            self.text(at, t, in_each);
        }
        if let Some(f) = &s.file {
            self.file(at, "section file", f, &["this", "item", "before", "base"]);
        }
        self.opt_command(at, s.run.as_deref(), in_each);
    }

    fn settings(&mut self, at: &str, s: &super::settings::StepSettings) {
        for t in s.inputs.iter().chain(&s.outputs).flatten() {
            self.text(at, t, false);
        }
        if let Some(f) = &s.first {
            self.command(at, f, false);
        }
        let tool_tier = s.ladder.iter().flatten().any(|t| matches!(t, super::ladder::TierSpec::Tool { .. }));
        if tool_tier && self.process.ladder.is_none() {
            self.out.push(format!("{at}: a tool tier needs the process's `ladder`"));
        }
    }

    fn top_level(&mut self, _list: &[Numbered]) {
        let p = self.process;
        if !matches!(p.agg.on_regression.as_str(), "annotate" | "rollback") {
            self.out.push(format!("agg.on_regression: `{}` is `annotate` or `rollback`", p.agg.on_regression));
        }
        for id in p.settings.skip.iter().flatten() {
            if !self.ids.contains(id) {
                self.out.push(format!("settings.skip: `{id}` names no phase or step"));
            }
        }
        for (name, list) in &p.deny {
            for d in list {
                self.text(&format!("deny `{name}`"), d, false);
            }
        }
        let mut files: Vec<(&str, &Option<String>, &[&str])> = vec![("run.system_prompt", &p.run.system_prompt, &[]),
            ("run.critic_scope", &p.run.critic_scope, &["this", "outputs"]), ("settings.quality.guide", &p.settings.quality.guide, &[])];
        if let Some(h) = &p.run.hooks {
            files.push(("run.hooks.process", &h.process, &[]));
        }
        for (what, f, extra) in files {
            if let Some(f) = f {
                self.file("process", what, f, extra);
            }
        }
        for c in p.run.start.iter().chain(p.run.auto_note.iter()).chain(p.settings.quality.reports.iter()) {
            self.command("process", c, false);
        }
        for s in &p.run.brief.sections {
            self.section("run.brief", s, true);
        }
        self.task_force();
    }

    fn task_force(&mut self) {
        let Some(tf) = &self.process.task_force else { return };
        self.file("task_force", "prompt", &tf.prompt, &[]);
        for d in &tf.deny {
            self.deny("task_force", d);
        }
        for s in &tf.sections {
            self.section("task_force", s, false);
        }
        for t in &tf.trust {
            if !self.ids.contains(&t.failed_in) {
                self.out.push(format!("task_force.trust: `failed_in: {}` names no phase or step", t.failed_in));
            }
            self.step_ref("task_force.trust", "reenter", &t.reenter);
            for e in &t.except {
                self.step_ref("task_force.trust", "except", e);
            }
        }
    }

    fn step_ref(&mut self, at: &str, key: &str, id: &str) {
        if !self.steps.contains(id) {
            self.out.push(format!("{at}: `{key}: {id}` names no step"));
        }
    }

    fn deny(&mut self, at: &str, entry: &str) {
        let glob = entry.contains(['/', '*', '.', '?']);
        if !glob && !self.process.deny.contains_key(entry) {
            self.out.push(format!("{at}: deny `{entry}` is no list of `deny:` and no path"));
        } else if glob {
            self.text(at, entry, false);
        }
    }

    /// A string of the file: every name in it must be known (`item` names only inside an `each`, as `{{ item }}`).
    fn text(&mut self, at: &str, text: &str, in_each: bool) {
        let mut vars = self.vars.clone();
        vars.extend(["this", "this.id", "this.key", "this.name", "this.number"].map(|n| (n.to_string(), String::new())));
        if in_each {
            for n in ["item", "item.yaml", "before", "base"] {
                vars.insert(n.into(), String::new());
            }
            vars.extend(item_fields(text));
        }
        if let Err(e) = template::render(text, &vars) {
            self.out.push(format!("{at}: {e} in `{}`", short(text)));
        }
    }

    fn opt_command(&mut self, at: &str, c: Option<&str>, in_each: bool) {
        if let Some(c) = c {
            self.command(at, c, in_each);
        }
    }

    /// A command: its names are known and, where it starts with a path, that file is there.
    fn command(&mut self, at: &str, c: &str, in_each: bool) {
        self.text(at, c, in_each);
        let mut vars = self.vars.clone();
        vars.extend(["this", "this.id", "this.key", "this.name", "this.number"].map(|n| (n.to_string(), String::new())));
        if in_each {
            vars.extend(["item", "item.yaml", "before", "base"].map(|n| (n.to_string(), String::new())));
            vars.extend(item_fields(c));
        }
        let Ok(rendered) = template::render(c, &vars) else { return };
        if let Some(first) = rendered.split_whitespace().next().filter(|t| t.contains('/')) {
            self.exists(at, "command", first);
        }
    }

    /// A file of the process home (or absolute): it exists, and its own `{{ names }}` are known.
    fn file(&mut self, at: &str, what: &str, path: &str, extra: &[&str]) {
        let Ok(rendered) = template::render(path, &self.vars) else {
            self.text(at, path, false);
            return;
        };
        let full = self.exists(at, what, &rendered);
        let mut vars = self.vars.clone();
        vars.extend(extra.iter().map(|n| (n.to_string(), String::new())));
        if let Some(text) = full.and_then(|f| std::fs::read_to_string(f).ok()) {
            for name in template::unknown_in_file(&text, &vars) {
                self.out.push(format!("{at}: {what} `{rendered}` uses `{{{{ {name} }}}}`, which the process doesn't know"));
            }
        }
    }

    fn exists(&mut self, at: &str, what: &str, path: &str) -> Option<PathBuf> {
        let full = if Path::new(path).is_absolute() { PathBuf::from(path) } else { self.home.join(path) };
        if full.is_file() {
            Some(full)
        } else {
            self.out.push(format!("{at}: {what} `{path}` is not a file (looked in {})", full.display()));
            None
        }
    }
}

/// The `{{ item.<field> }}` names in `text`, each as an empty value.
fn item_fields(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let name = after[..end].trim();
        if name.starts_with("item.") {
            out.insert(name.to_string(), String::new());
        }
        rest = &after[end + 2..];
    }
    out
}

fn short(text: &str) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 80 { format!("{}…", one.chars().take(80).collect::<String>()) } else { one }
}

/// The step `id` among `steps`, children of an `each` included.
fn find<'a>(steps: &[&'a Step], id: &str) -> Option<&'a Step> {
    for s in steps {
        if s.id == id {
            return Some(s);
        }
        if let Some(c) = find(&s.steps.iter().collect::<Vec<_>>(), id) {
            return Some(c);
        }
    }
    None
}

/// Calls `f` on every step, children of an `each` included.
fn collect(steps: &[Step], f: &mut dyn FnMut(&Step)) {
    for s in steps {
        f(s);
        collect(&s.steps, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(files: &[(&str, &str)], process: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        std::fs::write(dir.path().join("process.yaml"), process).unwrap();
        dir
    }

    const GOOD: &str = "version: 1\nvars: { legacy: legacy }\nroles: { file: tools.yaml }\ndeny: { inputs: [task.md] }\n\
        task_force: { prompt: prompts/tf.md, trust: [{ failed_in: verify, reenter: build }] }\n\
        phases:\n- id: verify\n  name: Verify\n  steps:\n\
        \x20 - { id: build, name: Build, kind: agent, review: true, deny: [inputs, \"{{ legacy }}/**\"], checks: [scripts/check.sh] }\n\
        \x20 - { id: units, name: Units, kind: each, items: { file: plan.yaml }, steps: [\
        { id: merge, name: Merge, kind: script, run: \"scripts/merge.sh {{ item }} {{ item.scope }}\" }] }\n\
        \x20 - { id: cp-build, name: OK, kind: checkpoint, prepare: scripts/doc.sh, doc: a.md, question: \"{{ cp.cp-build }}: ok?\", owner: build }\n";

    fn files() -> Vec<(&'static str, &'static str)> {
        vec![("prompts/build.md", "Do {{ legacy }} for {{ step.build }}. ${{ github.sha }}"), ("judges/build-review.md", "review"),
             ("prompts/tf.md", "tf"), ("scripts/check.sh", "x"), ("scripts/merge.sh", "x"), ("scripts/doc.sh", "x")]
    }

    /// A sound process has no problem.
    #[test]
    fn a_sound_process_has_no_problem() {
        let dir = home(&files(), GOOD);
        assert_eq!(check(&dir.path().join("process.yaml")), Vec::<String>::new());
    }

    /// Each kind of mistake is listed, with where it is.
    #[test]
    fn every_kind_of_mistake_is_listed() {
        let bad = GOOD.replace("review: true", "review: judges/nope.md").replace("scripts/check.sh", "scripts/gone.sh")
            .replace("reenter: build", "reenter: nobody").replace("owner: build", "owner: ghost")
            .replace("{{ item.scope }}", "{{ typo }}").replace("id: merge", "id: Merge")
            .replace("deny: [inputs,", "deny: [inputz,");
        let dir = home(&files(), &bad);
        let problems = check(&dir.path().join("process.yaml")).join("\n");
        for expect in ["review `judges/nope.md` is not a file", "command `scripts/gone.sh` is not a file",
                       "`reenter: nobody` names no step", "`owner: ghost` names no step", "unknown variable `{{ typo }}`",
                       "step id `Merge`", "deny `inputz` is no list"] {
            assert!(problems.contains(expect), "missing {expect:?} in:\n{problems}");
        }
    }

    /// Keys of another kind, missing keys, a name a prompt uses that nobody fills in, and ids used twice.
    #[test]
    fn keys_files_and_ids_are_held_to_their_kind() {
        let bad = GOOD.replace("kind: script, run:", "kind: script, prompt: p.md, run:").replace("id: cp-build", "id: build")
            .replace("prepare: scripts/doc.sh, ", "");
        let mut f = files();
        f[0].1 = "uses {{ step.typo }} and {{ nope }}";
        let dir = home(&f, &bad);
        let problems = check(&dir.path().join("process.yaml")).join("\n");
        for expect in ["`prompt` doesn't belong to a Script step", "a Checkpoint step needs `prepare`", "used twice",
                       "uses `{{ step.typo }}`", "uses `{{ nope }}`"] {
            assert!(problems.contains(expect), "missing {expect:?} in:\n{problems}");
        }
        let dir = home(&[], "version: 1\nphases: []\n");
        assert_eq!(check(&dir.path().join("process.yaml")), ["phases: none"]);
        let reads = GOOD.replace("kind: agent, review: true,", "kind: agent, settings: { inputs: [task.md] }, review: true,");
        let dir = home(&files(), &reads);
        assert!(check(&dir.path().join("process.yaml")).join("\n").contains("its owner `build` doesn't read `cp-build-decisions.md`"));
        let covered = reads.replace("inputs: [task.md]", "inputs: [task.md, \"*-decisions.md\"]");
        let dir = home(&files(), &covered);
        assert_eq!(check(&dir.path().join("process.yaml")), Vec::<String>::new(), "a glob covers it");
        assert!(check(&dir.path().join("none.yaml"))[0].contains("none.yaml"));
    }
}

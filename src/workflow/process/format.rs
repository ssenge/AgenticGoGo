//! The process file's format (A-37): phases of steps, each step one of a closed set of kinds. The types only;
//! `check` validates what serde can't, `number` computes the numbers and `run` runs it. Every struct refuses
//! an unknown key, so a typo is an error at startup, never a silently ignored setting.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::settings::{Role, StepSettings};

/// The whole file.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Process {
    /// the format's version; only 1 exists
    pub version: u32,
    /// `{{ name }}` values for prompts, rubrics and every string of the file
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    /// a YAML map of more variables, in the process home; `vars` wins on a name both have
    #[serde(default)]
    pub vars_file: Option<String>,
    /// the roles file in the project: the worker's and the critic's agent, model and effort
    #[serde(default)]
    pub roles: Option<RolesSpec>,
    /// where prompts and rubrics are by convention
    #[serde(default)]
    pub dirs: Dirs,
    #[serde(default)]
    pub settings: ProcessSettings,
    /// run-level options of agg
    #[serde(default)]
    pub agg: AggSpec,
    #[serde(default)]
    pub run: RunSpec,
    /// named write-deny lists, referenced by name in `deny:` lists
    #[serde(default)]
    pub deny: BTreeMap<String, Vec<String>>,
    /// declared judges (A-38): a `metric:` or `score:` over swe-metrics, or a `command:` met by its exit code, by
    /// name; a step's `checks` and the hooks files name them as `{ judge: <name> }`
    #[serde(default)]
    pub judges: BTreeMap<String, crate::core::config::JudgeOverride>,
    #[serde(default)]
    pub task_force: Option<TaskForceSpec>,
    #[serde(default)]
    pub ladder: Option<LadderSpec>,
    pub phases: Vec<Phase>,
}

/// The roles file: its name in the project and the roles it must have.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RolesSpec {
    pub file: String,
    #[serde(default = "RolesSpec::default_names")]
    pub names: Vec<String>,
}

impl RolesSpec {
    fn default_names() -> Vec<String> {
        vec!["worker".into(), "critic".into()]
    }
}

/// Where a step's prompt and its rubric are unless it names them (relative to the process home).
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dirs {
    #[serde(default = "Dirs::prompts")]
    pub prompts: String,
    #[serde(default = "Dirs::reviews")]
    pub reviews: String,
}

impl Dirs {
    fn prompts() -> String {
        "prompts".into()
    }
    fn reviews() -> String {
        "judges".into()
    }
}

impl Default for Dirs {
    fn default() -> Dirs {
        Dirs { prompts: Dirs::prompts(), reviews: Dirs::reviews() }
    }
}

/// The process's own settings: defaults under the project's file.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProcessSettings {
    /// the project's overlay file (in the project); absent: none
    #[serde(default)]
    pub project: Option<String>,
    /// keys of the project's file that agg tolerates and ignores
    #[serde(default)]
    pub extra_keys: Vec<String>,
    #[serde(default)]
    pub defaults: StepSettings,
    /// ids of phases or steps kept in the order but not run; the project's list replaces it
    #[serde(default)]
    pub skip: Option<Vec<String>>,
    #[serde(default)]
    pub quality: QualitySpec,
    /// size names a ladder tier may use, mapped to models
    #[serde(default)]
    pub models: BTreeMap<String, String>,
}

/// The code-quality floor of the steps marked `quality: true`.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QualitySpec {
    pub definition: Option<String>,
    pub version: Option<String>,
    pub min: Option<f64>,
    /// the workers' guideline: a file with `{min}`, `{definition}`, `{version}`, `{since}`, `{reports}` filled in
    pub guide: Option<String>,
    /// a command that reads the build's reports the score reads
    pub reports: Option<String>,
    /// the folder whose changed files are scored
    pub scope: Option<String>,
}

/// Options of the agg run itself.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AggSpec {
    /// working memory between sessions (agg's STATE.md, wiki, summary): off by default for a process
    #[serde(default)]
    pub memory: bool,
    /// the operator's own plugins, hooks and connectors in the workers' sessions: off by default
    #[serde(default)]
    pub user_settings: bool,
    /// `annotate` (record a regression, keep the work) or `rollback`
    #[serde(default = "AggSpec::annotate")]
    pub on_regression: String,
    /// a judge's time limit, seconds
    #[serde(default = "AggSpec::timeout")]
    pub judge_timeout: u64,
}

impl AggSpec {
    fn annotate() -> String {
        "annotate".into()
    }
    fn timeout() -> u64 {
        3600
    }
}

impl Default for AggSpec {
    fn default() -> AggSpec {
        AggSpec { memory: false, user_settings: false, on_regression: AggSpec::annotate(), judge_timeout: AggSpec::timeout() }
    }
}

/// How the run goes about its steps.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunSpec {
    /// the step history, relative to the project
    pub history: Option<String>,
    /// files `--reset` never takes back
    #[serde(default)]
    pub own: Vec<String>,
    /// commands before agg opens the project, every run; their last stdout line is logged
    #[serde(default)]
    pub start: Vec<String>,
    /// a file in the project with `switched_off: { <id>: <why> }`, written by a step
    pub config: Option<String>,
    /// a file whose change drops every step's base and the kept results of the run before
    pub bases_reset_when_changed: Option<String>,
    pub system_prompt: Option<super::standing::Text>,
    #[serde(default)]
    pub brief: BriefSpec,
    /// the critic's scope section, with `{{ outputs }}` filled in
    pub critic_scope: Option<super::standing::Text>,
    /// a notes file between steps (A-38): each step reads it first and writes its own section `## <step id>` of at most five
    /// lines at the end; agg's brief section says so and checks the length after the step
    pub notes: Option<String>,
    /// the file the reviewers' open points are kept in until the next checkpoint
    pub open_points: Option<String>,
    /// end processes a step left running
    #[serde(default)]
    pub leftovers: bool,
    #[serde(default)]
    pub judge_cache: Option<JudgeCache>,
    #[serde(default)]
    pub hooks: Option<HooksSpec>,
    /// a command whose stdout is appended to an auto-approval's record entry
    pub auto_note: Option<String>,
    /// check the inputs and outputs the steps declare (A-38): before a step, every input an earlier step of this run
    /// declares as an output exists; after it, every output it declares exists
    #[serde(default)]
    pub declared: bool,
    /// commit what the project folder holds (the inputs a human filled in) on a branch before the run (A-38), so
    /// every run names the inputs it used and its results land on top of them
    #[serde(default)]
    pub commit_inputs: Option<CommitInputs>,
    /// folders in which a script a step wrote or changed must be able to run (A-38): a `#!` file executable, a shell
    /// script parsing
    #[serde(default)]
    pub scripts_runnable: Vec<String>,
}

/// Where and how the run's inputs are committed.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommitInputs {
    #[serde(default = "CommitInputs::branch")]
    pub branch: String,
    #[serde(default = "CommitInputs::message")]
    pub message: String,
}

impl CommitInputs {
    fn branch() -> String {
        "main".into()
    }
    fn message() -> String {
        "Inputs at run start".into()
    }
}

/// The standing sections of every agent's brief and the small inputs put into it.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BriefSpec {
    /// a section for steps that share the machine with other runs: own temp folder, end only what you started
    #[serde(default)]
    pub shared_machine: bool,
    #[serde(default)]
    pub sections: Vec<Section>,
    #[serde(default)]
    pub inputs: Option<InputsSpec>,
}

/// Which small input files go into a brief, and how much of them.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputsSpec {
    /// files every brief gets when they exist
    #[serde(default)]
    pub always: Vec<String>,
    #[serde(default = "InputsSpec::max_file")]
    pub max_file: usize,
    #[serde(default = "InputsSpec::max_all")]
    pub max_all: usize,
}

impl InputsSpec {
    fn max_file() -> usize {
        16_000
    }
    fn max_all() -> usize {
        48_000
    }
}

/// A script check's verdict cache: a met verdict is reused on the same content within a run.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JudgeCache {
    /// files whose changes don't change what a check reads (a worker's notes)
    #[serde(default)]
    pub ignore: Vec<String>,
}

/// Hook files: the process's and the project's.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HooksSpec {
    pub process: Option<String>,
    pub project: Option<String>,
}

/// The task force: what looks for a failed step's cause.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskForceSpec {
    /// agg's default (the goal, how to find the cause, the answer's form), or a file that replaces it, or `{ append: <file> }`
    pub prompt: Option<super::standing::Text>,
    /// re-entries and repairs per failed step
    #[serde(default = "TaskForceSpec::max")]
    pub max: u32,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub sections: Vec<Section>,
    /// proposals that go ahead without a human
    #[serde(default)]
    pub trust: Vec<Trust>,
}

impl TaskForceSpec {
    fn max() -> u32 {
        3
    }
}

/// A rule over the task force's proposals: a failure in `failed_in` (a phase or step id) that re-enters `reenter`.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Trust {
    pub failed_in: String,
    pub reenter: String,
    /// failed steps the rule leaves out
    #[serde(default)]
    pub except: Vec<String>,
}

/// How a ladder's tool tiers run.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LadderSpec {
    /// the command of a tool tier: `{{ item }}` the unit, `{tool}` the tier's tool
    pub tool_run: String,
    /// what a tool tier may change: committed after it (the paths that exist)
    #[serde(default)]
    pub tool_commits: Vec<String>,
    /// the tools a decider may pick, besides the project's `managed` ones
    #[serde(default)]
    pub tools: Vec<String>,
}

/// A phase: a named run of steps.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Phase {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub pre: Vec<String>,
    #[serde(default)]
    pub post: Vec<String>,
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    pub meta: Option<serde_yaml::Value>,
    pub steps: Vec<Step>,
}

/// The kinds of step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Script,
    Agent,
    Checkpoint,
    Each,
    Exam,
    Outside,
    Planned,
}

/// A step. One struct for every kind: `check` refuses a key that doesn't fit the step's kind.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// a command, or commands that must all be met; unmet: the step is skipped
    #[serde(default)]
    pub when: Option<OneOrMany>,
    /// write-deny entries: names of top-level `deny:` lists, or globs
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub pre: Vec<String>,
    #[serde(default)]
    pub post: Vec<String>,
    #[serde(default)]
    pub settings: Option<StepSettings>,
    /// a text for the task force's brief when this step fails
    #[serde(default)]
    pub constraints: Option<String>,
    /// free-form, ignored by agg, handed through by `--list` (descriptions for docs and slides)
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    pub meta: Option<serde_yaml::Value>,
    // script
    #[serde(default)]
    pub run: Option<String>,
    // agent
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub checks: Vec<Check>,
    #[serde(default)]
    pub review: Option<Review>,
    #[serde(default)]
    pub repairs: Option<usize>,
    #[serde(default)]
    pub sections: Vec<Section>,
    #[serde(default)]
    pub quality: bool,
    /// run once when the step finally fails, before the task force
    #[serde(default)]
    pub on_fail: Option<String>,
    // checkpoint
    #[serde(default)]
    pub prepare: Option<String>,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub go_live: bool,
    // each
    #[serde(default)]
    pub items: Option<Items>,
    #[serde(default)]
    pub steps: Vec<Step>,
    // exam
    #[serde(default)]
    pub examinee: Option<Worker>,
    #[serde(default)]
    pub answers: Option<String>,
    #[serde(default)]
    pub grader: Option<String>,
    #[serde(default)]
    pub repairer: Option<Worker>,
    #[serde(default)]
    pub rounds: Option<usize>,
    #[serde(default)]
    pub fresh: Vec<String>,
    #[serde(default)]
    pub findings_ids: Option<FindingsIds>,
    #[serde(default)]
    pub dispute: Option<Dispute>,
}

/// One command or several.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    /// The commands, in order.
    pub fn all(&self) -> Vec<&str> {
        match self {
            OneOrMany::One(c) => vec![c.as_str()],
            OneOrMany::Many(cs) => cs.iter().map(String::as_str).collect(),
        }
    }
}

/// A script check: a command, or a command with a name and a condition.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Check {
    Command(String),
    /// a judge the process file declares under `judges:`
    Declared(DeclaredCheck),
    Full(CheckSpec),
}

/// A check that names a declared judge: `{ judge: <name> }`, optionally `when: last` inside an `each`.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeclaredCheck {
    pub judge: String,
    pub when: Option<String>,
}

/// A check with more than its command.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckSpec {
    pub run: String,
    pub name: Option<String>,
    /// `last`: only the last item's instance of an `each` runs it
    pub when: Option<String>,
}

impl Check {
    /// The command of a check that runs one; none for a declared judge.
    pub fn command(&self) -> Option<&str> {
        match self {
            Check::Command(c) => Some(c),
            Check::Declared(_) => None,
            Check::Full(f) => Some(&f.run),
        }
    }
    /// The declared judge a check names, if it does.
    pub fn judge(&self) -> Option<&str> {
        match self {
            Check::Declared(d) => Some(&d.judge),
            _ => None,
        }
    }
    pub fn name(&self) -> Option<&str> {
        match self {
            Check::Full(f) => f.name.as_deref(),
            _ => None,
        }
    }
    pub fn last_only(&self) -> bool {
        matches!(self, Check::Full(CheckSpec { when: Some(w), .. }) | Check::Declared(DeclaredCheck { when: Some(w), .. }) if w == "last")
    }
}

/// The critic's rubric: `true` for the conventional file, else its path.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Review {
    On(bool),
    File(String),
}

/// A section of a brief: a text, a file's text or a command's output; exactly one.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub text: Option<String>,
    pub file: Option<String>,
    pub run: Option<String>,
}

/// Where an `each` takes its items from.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Items {
    pub file: String,
    /// the key holding the order: a list of item keys
    pub list: Option<String>,
    /// the key holding the items (maps); default `units`
    pub items: Option<String>,
    /// the field of an item that is its key; default `id`
    pub key: Option<String>,
}

/// A worker of an exam: its prompt, sections and write limits.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    pub prompt: String,
    #[serde(default)]
    pub sections: Vec<Section>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// How an exam's rounds are told apart: the ids a verdict names in these lists of a file.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindingsIds {
    pub file: String,
    pub lists: Vec<String>,
}

/// A dispute of the repairer's: the step it sends the run back to, once.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dispute {
    pub reenter: String,
}

/// The roles a step may set, for a convenience of other modules.
pub type Roles = BTreeMap<String, Role>;

impl Process {
    /// Parses a process file's text.
    pub fn parse(text: &str) -> anyhow::Result<Process> {
        let process: Process = serde_yaml::from_str(text)?;
        if process.version != 1 {
            anyhow::bail!("version: {} is not a version of the process format (1)", process.version);
        }
        Ok(process)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small file parses with its defaults; a typo is refused by name.
    #[test]
    fn a_process_parses_and_a_typo_is_refused() {
        let text = "version: 1\nphases:\n- id: a\n  name: A\n  steps:\n  - { id: s, name: S, kind: script, run: x.sh }\n";
        let p = Process::parse(text).unwrap();
        assert_eq!((p.phases[0].steps[0].kind, p.dirs.prompts.as_str(), p.agg.judge_timeout), (Kind::Script, "prompts", 3600));
        let err = Process::parse(&text.replace("run:", "runn:")).unwrap_err().to_string();
        assert!(err.contains("runn"), "{err}");
        assert!(Process::parse(&text.replace("version: 1", "version: 2")).is_err());
    }

    /// The forms of `when`, `checks` and `review`.
    #[test]
    fn the_short_forms_parse() {
        let step: Step = serde_yaml::from_str("{ id: s, name: S, kind: agent, when: [a, b], review: true, \
            checks: [x.sh, { run: y.sh, when: last }] }").unwrap();
        assert_eq!(step.when.unwrap().all(), ["a", "b"]);
        assert!(matches!(step.review, Some(Review::On(true))));
        assert!(!step.checks[0].last_only() && step.checks[1].last_only() && step.checks[1].command() == Some("y.sh"));
        let one: Step = serde_yaml::from_str("{ id: s, name: S, kind: script, when: x, review: judges/r.md }").unwrap();
        assert_eq!(one.when.unwrap().all(), ["x"]);
    }

    /// A check names a declared judge, alone or with `when: last`; an unknown key of it is refused.
    #[test]
    fn a_check_can_name_a_declared_judge() {
        let step: Step = serde_yaml::from_str("{ id: s, name: S, kind: agent, checks: [{ judge: tests }, { judge: p, when: last }] }").unwrap();
        assert_eq!((step.checks[0].judge(), step.checks[0].command()), (Some("tests"), None));
        assert!(step.checks[1].last_only() && !step.checks[0].last_only());
        assert!(serde_yaml::from_str::<Step>("{ id: s, name: S, kind: agent, checks: [{ judge: t, run: x }] }").is_err());
    }
}

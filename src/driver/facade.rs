//! [`Relentless`] — the facade a Rust driver holds, and the eleven calls it makes on it.
//!
//! # The load-bearing premise
//!
//! `rlt.step(&s)?` **IS** one iteration of `rlt run`'s loop body — the same hook dispatch, in the
//! same order, hooks and all. It calls [`crate::loop_::step_once`]; it does not re-implement a loop.
//! Any other answer would mean re-plumbing git session isolation, keep/rollback, memory, the
//! summariser, notify, rate-limit backoff, per-agent accounting, `verdicts.jsonl`, `state.json`, the
//! watchdog and the isolation tiers a second time, correctly, forever.
//!
//! # Why every call takes `&self`
//!
//! A driver holds ONE binding and calls it from inside its own `for`/`if`. `&mut self` would force
//! the borrow checker into the driver's control flow for no gain: one thread works on an instance at a time, so
//! the interior mutability below (`Slot`/`Locked`/`OnceLock`, locks that are never contended) is free and invisible.

mod fork;

use crate::core::asks::AskCase;
use super::locked::{Locked, Slot};
use std::sync::OnceLock;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::core::calls::{CallRecord, NoteLevel};
use crate::core::config::{RelentlessConfig, CustomBackend, Limits, ResolvedStep};
use crate::core::engine::GoalRuntime;
use crate::core::model::{Verifier, VerifierKind, VerifierSource, Verdict};
use crate::driver::{Agent, Fatal, GateFailure, GateOutcome, Group, GroupVerdict, OnRegression, Opts, Step, StepOutcome};
use crate::git::StagedSession;
use crate::loop_::{
    run_hook, RLTState, End, Flow, Handler, Lifecycle, LoopState, RunPidGuard, RunOutcome, StopHooks,
};

/// How long `block()` naps between re-evaluating the ceilings. Short for the reason
/// `RateLimitBackoff` polls: a long wait must stay interruptible and must not outlive the run's own
/// ceilings.
const BLOCK_CHUNK_SECS: u64 = 5;

/// The Rust driver's handle on a run.
///
/// Built by [`Relentless::open`], configured by the self-consuming builder chain
/// ([`Relentless::limits`] / [`Relentless::on_regression`] / [`Relentless::instructions`]), then used through `&self`.
///
/// ⛔ **A stray `rlt.yaml` in the project is IGNORED** — not merged, not a fallback.
/// [`RelentlessConfig::load`] is never called on this path. The two entry points share FILES
/// (`rlt/verifiers/`, the instructions file, `rlt/state/`), never configuration.
///
/// Fields are private, against this crate's no-facade convention, because `Relentless` *is* the facade and
/// its invariants are not expressible as field types: `started` must be filled exactly once and
/// only after the chain, and `cfg` must stop being writable the moment it has been.
pub struct Relentless {
    cfg: RelentlessConfig,
    dir: PathBuf,
    /// this run CONTINUES a previous one. Today it reaches exactly one decision — `GitSetup`
    /// discards a crashed session's dirty tree instead of refusing to start (§3.9 rule 2).
    resume: bool,
    /// the deferred run-level setup. `None` until the first spending call.
    started: OnceLock<Started>,
    /// THE LATCH. Once set, every call is a no-op that spends nothing (BUILD.md §3.3).
    ended: Slot<Option<RunOutcome>>,
    /// the ledger cursor (Phase 1).
    ord: Slot<u64>,
    /// the per-STEP lazy verdict cache — what makes `rlt.verifier(&x)` memoized and `&&` a gate.
    verdicts: Locked<HashMap<String, Verdict>>,
    /// every verdict consulted since the last `gate()`; `gate()` reads and clears it.
    span: Locked<HashMap<String, Verdict>>,
    /// the verifiers the driver asked BY NAME (`verifier()`) since the last gate — held one by one there.
    span_direct: Locked<HashSet<String>>,
    /// the groups asked (`group()`) since the last gate: a member asked only through one is verified
    /// with its group, net (`core::groups`).
    span_groups: Locked<BTreeMap<String, Vec<String>>>,
    /// the engine's goal truth as of the moment the open span began — what a rollback restores.
    ///
    /// On this path it is provably EMPTY (§3.6 item 3: the run-set is empty, so the engine has no
    /// goals), and it is carried anyway so the shared keep/rollback body gets the same argument from
    /// both callers rather than a hard-coded `&[]` that would quietly rot if a driver ever grew one.
    span_goals: Locked<Vec<GoalRuntime>>,
    /// the strongest isolation tier any step of this run has declared — the driver path's answer to
    /// `cfg.run_isolation()`.
    ///
    /// ⚠ It exists because `cfg.run_isolation()` reads `cfg.steps`, which a DRIVER NEVER POPULATES:
    /// its steps are values in its own binary, not config entries. So that call returns `None` here
    /// unconditionally, and §2.5's "verifiers take the RUN tier" silently meant "verifiers take no tier at
    /// all" — every driver verifier ran unconfined, which is the exact escape §2.5 exists to close.
    /// Proven by a real-kernel test that watched a driver's verifier write outside the project.
    iso_seen: Slot<crate::isolation::Isolation>,
    /// the label path — see [`Relentless::pos`].
    pos: Locked<Vec<PosItem>>,
    /// the run's lane services and this instance's lane key, when it is one of several lanes of a run (`None`: an ordinary run)
    lane: Option<(std::sync::Arc<crate::lanes::Shared>, String)>,
    /// the name of an [`Agent::Custom`] handed to [`Relentless::verifier_agent`], refused when the run boots:
    /// the builder chain cannot fail, and the ruler is resolved BY NAME (`verifier.agent`).
    custom_ruler: Option<String>,
    /// the working-memory default of a step that doesn't set its own — see [`Relentless::memory`].
    memory: bool,
    /// whether workers load the operator's own agent settings — see [`Relentless::user_settings`].
    user_settings: bool,
    /// the keys of the operator's settings `env` block the Claude workers get — see [`Relentless::user_env`].
    user_env: crate::backend::user_env::UserEnv,
    /// text every worker's agent adds to its system prompt — see [`Relentless::system_prompt`].
    system_prompt: Option<String>,
    /// the workflow's variables — see [`Relentless::vars`]. The instance carries them, and every call installs them on
    /// the thread it runs on: a fork is built on one thread and may run on another.
    vars: BTreeMap<String, String>,
    /// the thread working on this instance, and how deep its calls are nested — see [`Relentless::enter`].
    busy: std::sync::Mutex<Option<(std::thread::ThreadId, u32)>>,
    /// where this instance came from when it is a fork — see [`Relentless::fork`]
    fork_of: Option<fork::ForkOf>,
    /// the services every fork of this run shares, made at the first fork — see [`Relentless::fork`]
    forks: OnceLock<std::sync::Arc<crate::lanes::Shared>>,
    /// paths the worktrees of this instance's forks are given — see [`Relentless::fork_seed`]
    fork_seed: Vec<String>,
    /// the end of the run was written (a merge writes a fork's before its work is merged; the drop writes the rest once)
    wound: std::sync::atomic::AtomicBool,
}

/// Everything that only exists once the run-level setup has run.
///
/// A `OnceLock` is what makes the builder-shaped `open()` honest: before the first spending call
/// there IS no `LoopState`, no `Lifecycle` and no guards, and the struct says so instead of
/// declaring always-present fields that cannot be constructed until the chain is complete.
struct Started {
    st: Locked<LoopState>,
    lc: Lifecycle,
    /// the recorded calls a resume fast-forwards through, already truncated to the last KEPT gate.
    ///
    /// Empty on a fresh run. Indexed by ORDINAL: `replay[n]` answers the run's n-th rlt call, and
    /// the driver walks itself back to the interruption point by *running* — identical inputs
    /// produce identical branches, so its position never has to be serialized.
    ///
    /// ⚠ It lives HERE, not on `Relentless`, because deciding it means WRITING the ledger (truncating it,
    /// or clearing it for a fresh run) and that must happen behind the double-run guard. Doing it in
    /// `open()` meant a second driver opening a live project wiped the RUNNING driver's ledger before
    /// failing the guard a moment later.
    replay: Vec<CallRecord>,
    /// the Drop guards live HERE, so dropping the `Relentless` fires them on return, early return AND
    /// panic-unwind.
    _stop: StopHooks,
    /// the run's confinement policy — AFTER `_stop`, so it is still registered while the `on_stop`
    /// hooks fire (fields drop in declaration order).
    _policy: crate::isolation::policy::PolicyGuard,
    _pid: RunPidGuard,
    /// this run's report handle (`run_id`, the write latch). `Drop for Relentless` writes the report.
    report: crate::report::RunHandle,
    /// what the run's Claude processes get of the operator's settings (`user_env`), installed on each thread that calls in
    passed: Vec<(String, String)>,
}

/// A call in progress on an instance (see `Relentless::enter`): the instance stays this thread's until the outermost
/// guard drops, which also puts back the variables and settings the thread had.
struct Entered<'a> {
    owner: &'a Relentless,
    saved: Option<Saved>,
}

/// What a thread had of a run's variables and passed settings before the outermost call on an instance.
type Saved = (BTreeMap<String, String>, Vec<(String, String)>);

impl Drop for Entered<'_> {
    fn drop(&mut self) {
        let mut held = self.owner.busy.lock().unwrap_or_else(|e| e.into_inner());
        *held = match *held {
            Some((owner, depth)) if depth > 1 => Some((owner, depth - 1)),
            _ => None,
        };
        drop(held);
        if let Some((vars, passed)) = self.saved.take() {
            crate::core::vars::set(vars);
            crate::backend::user_env::install(passed);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// construction + the builder chain
// ---------------------------------------------------------------------------------------------

impl Relentless {
    /// Open `dir` as a driver project. Validates the directory and nothing else — the config is
    /// filled by the chain that follows, and the run-level setup is deferred to the first
    /// `step()`/`verifier()`/`gate()`/`check_limits()`.
    pub fn open(dir: impl AsRef<Path>) -> Result<Relentless, Fatal> {
        Relentless::open_with(dir, Opts::default())
    }

    /// [`Relentless::open`] with the run-level options that must be known before any config is applied.
    pub fn open_with(dir: impl AsRef<Path>, opts: Opts) -> Result<Relentless, Fatal> {
        let dir = dir.as_ref();
        if !dir.is_dir() {
            return Err(Fatal::Other(anyhow::anyhow!(
                "`{}` is not a directory — Relentless::open takes the PROJECT ROOT",
                dir.display()
            )));
        }
        // canonicalize so the sandbox carve-out, the git commands and the label paths all agree on
        // one spelling of the project root.
        let dir = std::fs::canonicalize(dir).map_err(Fatal::Io)?;
        // The project NAME is what branch names, the run ledger and the dashboard are keyed on.
        // There is no `.project()` builder call: the directory already names the project, and a
        // second spelling is a second thing to get wrong.
        let cfg = RelentlessConfig {
            project: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "driver".into()),
            ..RelentlessConfig::default()
        };
        Ok(Relentless::bare(cfg, dir, opts.resume))
    }

    /// An instance with nothing set but its config, its folder and whether it resumes.
    pub(super) fn bare(cfg: RelentlessConfig, dir: PathBuf, resume: bool) -> Relentless {
        Relentless {
            cfg,
            dir,
            resume,
            started: OnceLock::new(),
            ended: Slot::new(None),
            ord: Slot::new(0),
            verdicts: Locked::new(HashMap::new()),
            span: Locked::new(HashMap::new()),
            span_direct: Locked::new(HashSet::new()),
            span_groups: Locked::new(BTreeMap::new()),
            span_goals: Locked::new(Vec::new()),
            iso_seen: Slot::new(crate::isolation::Isolation::None),
            pos: Locked::new(Vec::new()),
            custom_ruler: None,
            lane: None,
            memory: true,
            user_settings: true,
            user_env: Default::default(),
            system_prompt: None,
            vars: BTreeMap::new(),
            busy: std::sync::Mutex::new(None),
            fork_of: None,
            forks: OnceLock::new(),
            fork_seed: Vec::new(),
            wound: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The run-level ceilings. **Opt-in**: they are enforced where the driver calls
    /// [`Relentless::check_limits`], and nowhere else. A driver that never calls it has no ceilings.
    pub fn limits(mut self, limits: Limits) -> Relentless {
        self.cfg.sequence.limits = limits;
        self
    }

    /// What a regression across a span MEANS to this project — the policy `gate()` applies.
    pub fn on_regression(mut self, policy: OnRegression) -> Relentless {
        self.cfg.sequence.gate_regressions = policy == OnRegression::Rollback;
        self
    }

    /// Hold `verifier`'s `value` to a numeric RATCHET at every [`Relentless::gate`]: it may not get worse than
    /// at the last kept gate by more than the tolerance, or the gate counts a regression (and, under
    /// [`OnRegression::Rollback`], discards the span).
    ///
    /// The reference before the first keep is the [`Ratchet::baseline`] seed, else the first value
    /// the driver asks for BEFORE its first `step()` — the driver's baseline pass. Like the boolean
    /// rule, it applies to the verifiers asked since the last gate; a ratcheted verifier that errored or
    /// emitted no number never passes.
    pub fn ratchet(mut self, verifier: impl Into<String>, spec: crate::core::ratchet::Ratchet) -> Relentless {
        self.cfg.sequence.ratchet.insert(verifier.into(), spec.into());
        self
    }

    /// The most verifier evidence (MiB) this run copies into `rlt/private/evidence/` — the twin of
    /// YAML's `verifier.evidence_max_mb` (default 100). A disk bound, not a run limit: a file that does
    /// not fit is skipped and recorded on its verdict's ledger row, and the run goes on.
    pub fn evidence_max_mb(mut self, mb: u64) -> Relentless {
        self.cfg.verifier.evidence_max_mb = mb;
        self
    }

    /// Merge a span at [`Relentless::gate`] only if something IMPROVED — a verifier asked since the last gate
    /// went unmet→met (a group asked through [`Relentless::group`]: gained ground net), or a ratcheted value
    /// moved the good way beyond its tolerance. Otherwise the span is discarded as
    /// [`GateOutcome::NoImprovement`]: not a regression, so it never counts for `rollbacks`. The
    /// driver's spelling of `sequence.merge_requires_improvement`; off by default.
    pub fn merge_requires_improvement(mut self, on: bool) -> Relentless {
        self.cfg.sequence.merge_requires_improvement = on;
        self
    }

    /// The run's default for [`Step::memory`]: whether a step's sessions keep rlt's working memory
    /// (forward state, wiki, memory log, summary). On by default; a step's own setting wins. The
    /// driver's twin of YAML's `memory.enabled`.
    pub fn memory(mut self, on: bool) -> Relentless {
        self.memory = on;
        self
    }

    /// Whether the workers load the operator's OWN agent settings: their plugins, MCP servers, hooks and
    /// personal instructions (A-33). On by default. Off, a worker sees only the project's settings, so
    /// what the operator set up for their own sessions (a memory plugin, a command-rewriting hook, a mail
    /// connector) neither changes nor reads the run's work. Claude: `--setting-sources project,local`; an
    /// agent that can't leave them out runs with them and says so.
    pub fn user_settings(mut self, on: bool) -> Relentless {
        self.user_settings = on;
        self
    }

    /// Which keys of the operator's settings `env` block (`~/.claude/settings.json`, or `$CLAUDE_CONFIG_DIR/settings.json`)
    /// the Claude workers and one-shot calls (verifiers, summaries) get as environment variables, without loading the
    /// plugins, hooks and MCP servers of the same file: `false` (the default) none, `true` every key, or a list of
    /// names. A variable rlt's own environment already has wins, the keys rlt manages (`RLT_*`) are never taken, and
    /// only string values are. For a host whose model gateway is set only in those settings, while
    /// [`Relentless::user_settings`] is off. Process-wide, like `RLT_MODELS`; other agents take nothing from it.
    pub fn user_env(mut self, which: impl Into<crate::backend::user_env::UserEnv>) -> Relentless {
        self.user_env = which.into();
        self
    }

    /// Whether `step`'s agent can be reached: one tiny call, with the flags and the environment of its worker
    /// (`user_settings` and `user_env` as this run has them), made without starting the run. `Ok` with what answered,
    /// or the agent's error text, with the setting that would fix it where the operator's settings hold a gateway.
    /// Where it comes before the run's first step, an unreachable agent stops the run with its cause.
    pub fn reachable(&self, step: &Step) -> Result<String, Fatal> {
        let _call = self.enter();
        let passed = crate::backend::user_env::take(&self.user_env);
        let agent: &dyn crate::backend::AgentBackend = match &step.agent {
            Agent::Custom(b) => &**b,
            a => crate::backend::for_name(a.name())?,
        };
        let model = step.model.clone().or_else(|| self.cfg.defaults.model.clone()).unwrap_or_else(|| agent.default_model().to_string());
        let user_settings = self.user_settings || !agent.capabilities().supports_project_settings_only;
        match crate::backend::reach::probe(agent, &model, user_settings, &self.dir) {
            Ok(()) => Ok(format!("{} ({model}) answered", step.agent.name())),
            Err(said) => {
                let missing: Vec<String> = crate::backend::user_env::gateway_keys().into_iter()
                    .filter(|k| !passed.contains(k) && std::env::var_os(k).is_none()).collect();
                Err(Fatal::Other(anyhow::anyhow!("the {} agent ({model}) is not reachable:\n  {}",
                    step.agent.name(), crate::backend::reach::explain(&said, &missing))))
            }
        }
    }

    /// Text every worker's agent adds to its system prompt (A-34): a rule that must hold for the whole
    /// session, where a brief is read once in its first turn and fades over a long session (say, "end with
    /// one line"). Claude: `--append-system-prompt`; an agent that can't runs without it and says so.
    pub fn system_prompt(mut self, text: impl Into<String>) -> Relentless {
        self.system_prompt = Some(text.into());
        self
    }

    /// The standing project-instructions file the worker's brief POINTS AT, relative to the project
    /// dir. Default `rlt/RLT.md`.
    ///
    /// rlt never reads its bytes — it checks that the file exists and emits "Read `<path>`" into the
    /// brief. A missing file produces no pointer at all.
    pub fn instructions(mut self, rel: impl Into<String>) -> Relentless {
        self.cfg.instructions = rel.into();
        self
    }

    /// The image every [`Isolation::Container`](crate::isolation::Isolation::Container) step runs in
    /// unless it names its own ([`Step::image`]) — YAML's `defaults.image`. It must follow the
    /// image contract (`docs/CONFIG.md`): the step's agent CLI on PATH for any uid, `/bin/sh`.
    pub fn image(mut self, image: impl Into<String>) -> Relentless {
        self.cfg.defaults.image = Some(image.into());
        self
    }

    /// The RULER — the backend that runs the rubric verifiers and the summarizer. YAML's `verifier.agent`;
    /// default [`Agent::Claude`]. Its CLI is preflighted at boot like every step's.
    ///
    /// ⛔ [`Agent::Custom`] is REFUSED when the run boots (the first `step()`/`verifier()`/`gate()`/
    /// `check_limits()` returns the error): the ruler is resolved by NAME, exactly as YAML's
    /// `verifier.agent` is, and a custom backend has no name rlt could resolve — a custom backend
    /// that called itself `claude` would otherwise silently become the real Claude.
    pub fn verifier_agent(mut self, agent: Agent) -> Relentless {
        match &agent {
            Agent::Custom(b) => self.custom_ruler = Some(b.name().to_string()),
            a => {
                self.cfg.verifier.agent = a.name().to_string();
                self.custom_ruler = None;
            }
        }
        self
    }

    /// The model the ruler runs the rubric verifiers and the summarizer on — YAML's `verifier.model`.
    /// Unset = the ruler's own cheap default.
    pub fn verifier_model(mut self, model: impl Into<String>) -> Relentless {
        self.cfg.verifier.model = Some(model.into());
        self
    }

    /// Names the project `name` instead of its folder's name: the branches (`rlt/<name>/session-N`), the run ledger and
    /// the dashboard are keyed on it. A lane takes `<project>.<each>-<key>`, so its refs are a subtree of its own.
    pub fn project_name(mut self, name: &str) -> Relentless {
        self.cfg.project = name.to_string();
        self
    }

    /// Makes this instance one lane of a run: its spend counts toward the run's ceilings, its sessions wait at the gate
    /// a rate limit closes, and a ceiling the run broke ends it. `key` names the lane. The scheduler of a parallel `each`
    /// sets it; a fork has it from [`Relentless::fork`].
    pub(crate) fn lanes(mut self, services: std::sync::Arc<crate::lanes::Shared>, key: &str) -> Relentless {
        self.lane = Some((services, key.to_string()));
        self
    }

    /// Ignored or untracked paths of the project (dependencies, build caches) copied into the worktree of every fork this
    /// instance makes, as a fork starts from what git holds and nothing else.
    pub fn fork_seed<I, S>(mut self, paths: I) -> Relentless
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.fork_seed = paths.into_iter().map(Into::into).collect();
        self
    }

    /// The workflow's variables: `{{ name }}` in every step's prompt file (the role prompt's too) and every
    /// rubric verifier's rubric (its `inputs:` too) is filled in from `vars`; a name it lacks fails the step or
    /// the verifier. Inline prompts are not templates. One map per process: the last call wins.
    pub fn vars(mut self, vars: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>) -> Relentless {
        self.vars = vars.into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        // also this thread's own, for the code of a flow that renders a brief outside of a call of the instance
        crate::core::vars::set(self.vars.clone());
        self
    }

    /// EVERY verifier's timeout in seconds, script and LLM alike — YAML's `verifier.timeout` (default
    /// 300). A single verifier overrides it with [`Verifier::timeout`].
    pub fn verifier_timeout(mut self, secs: u64) -> Relentless {
        self.cfg.verifier.timeout = secs;
        self
    }

    /// The least free disk space the run needs (YAML's `min_free_disk`): below it [`Relentless::check_limits`] and the
    /// next worker session end the run (exit 7), which can be resumed. Default 10 GB; [`Floor::Off`] turns it off.
    ///
    /// [`Floor::Off`]: crate::os::disk::Floor::Off
    pub fn min_free_disk(mut self, floor: crate::os::disk::Floor) -> Relentless {
        self.cfg.min_free_disk = floor;
        self
    }

    /// The image verifiers run in when a step of the run is containerized — YAML's `verifier.image`.
    /// `None` set = [`Relentless::image`], else the base image.
    pub fn verifier_image(mut self, image: impl Into<String>) -> Relentless {
        self.cfg.verifier.image = Some(image.into());
        self
    }

    /// Environment variables passed INTO every container of the run by NAME — YAML's
    /// `isolation_policy.pass_env`. The values come from this process's environment and never
    /// appear on a command line. Each agent's own credential variables need not be listed.
    pub fn pass_env<I, S>(mut self, names: I) -> Relentless
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.cfg.isolation_policy.pass_env.extend(names.into_iter().map(Into::into));
        self
    }

    /// Host files or directories mounted READ-ONLY into every container of the run — YAML's
    /// `isolation_policy.credentials` (under the container's `HOME` when they live under `~`).
    pub fn credentials<I, S>(mut self, paths: I) -> Relentless
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.cfg.isolation_policy.credentials.extend(paths.into_iter().map(Into::into));
        self
    }

    /// The run's network EGRESS policy — YAML's `isolation_policy.network`. `Network::Allow(hosts)`
    /// confines every confined process to those `host[:port]` destinations through rlt's proxy;
    /// every step must then run on a tier that enforces it, and its agent's endpoint must be listed
    /// (`step()` refuses otherwise).
    pub fn network(mut self, network: crate::isolation::egress::Network) -> Relentless {
        self.cfg.isolation_policy.network = network;
        self
    }

    /// A READ-ONLY context directory from outside the project, seen by every step and verifier of the
    /// run — the driver's spelling of a YAML `context:` entry. `path` is absolute, `~/`-relative or
    /// relative to the project; `name` is the short `as:` (`/context/<name>` in a container,
    /// `RLT_CONTEXT_<NAME>` everywhere). Validated when the run boots; every step must then be
    /// confined (`Isolation::Sandbox` or `Container`), or `step()` refuses it.
    pub fn context(mut self, path: impl Into<String>, name: impl Into<String>) -> Relentless {
        self.cfg.context.push(crate::core::config::ContextDir {
            path: path.into(),
            name: name.into(),
            mode: crate::core::config::ContextMode::Ro,
        });
        self
    }
}

// ---------------------------------------------------------------------------------------------
// the deferred run-level setup
// ---------------------------------------------------------------------------------------------

impl Relentless {
    /// Starts a call on this instance: one thread works on it at a time (the same thread may nest calls), and the run's
    /// variables and the operator's settings it passes are this thread's until the outermost call ends. A second thread
    /// meeting a busy instance panics, as a `RefCell` did, instead of interleaving two sessions in one worktree.
    fn enter(&self) -> Entered<'_> {
        let me = std::thread::current().id();
        let mut held = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        let depth = match *held {
            None => 1,
            Some((owner, depth)) if owner == me => depth + 1,
            Some(_) => {
                drop(held);
                panic!("Relentless: this instance is busy on another thread; give each thread its own fork (Relentless::fork)");
            }
        };
        *held = Some((me, depth));
        drop(held);
        let saved = (depth == 1).then(|| {
            let saved = (crate::core::vars::current(), crate::backend::user_env::passed());
            crate::core::vars::set(self.vars.clone());
            if let Some(started) = self.started.get() {
                crate::backend::user_env::install(started.passed.clone());
            }
            saved
        });
        Entered { owner: self, saved }
    }

    /// The started run, performing the run-level setup on first use.
    ///
    /// Both latch checks are needed: one for a run already ended, one for a setup that ended it —
    /// the `Baseline` pass can stop the run at launch, exactly as it does on the YAML path.
    fn ready(&self) -> Result<&Started, Fatal> {
        if let Some(o) = self.ended.get() {
            return Err(Fatal::Ended(o));
        }
        if self.started.get().is_none() {
            if let Some(fork) = &self.fork_of {
                // a fork's worktree is made at its first call, not at `fork()`: forking fifty items runs four at a time
                fork.begin(self.lane.as_ref().map(|(svc, _)| svc)).map_err(Fatal::from_message)?;
            }
            let started = self.boot()?;
            let _ = self.started.set(started);
            if let (Some(fork), Some((services, _))) = (&self.fork_of, &self.lane) {
                // from now on the operator's commands are sent to this fork's folder
                services.register(&fork.name, &self.dir);
                if services.stop_requested() {
                    self.ended.set(Some(RunOutcome::Stopped));
                    return Err(Fatal::Ended(RunOutcome::Stopped));
                }
            }
        }
        if let Some(o) = self.ended.get() {
            return Err(Fatal::Ended(o));
        }
        Ok(self.started.get().expect("just set"))
    }

    /// `loop_::run_with`'s whole pre-loop, in the same order (BUILD.md §3.2).
    ///
    /// It is NOT in `open()` because at `open()` time the config is empty: the builder chain has not
    /// run yet, so `assemble`, `capability::check` and the banner would all read placeholder values.
    ///
    /// `run_with`'s `register` host-plugin seam is deliberately not exposed — a driver IS the host
    /// program. Re-add it the first time one wants a third-party handler.
    fn boot(&self) -> Result<Started, Fatal> {
        if let Some(name) = &self.custom_ruler {
            return Err(Fatal::Other(anyhow::anyhow!(
                "verifier_agent(Agent::Custom(`{name}`)): the ruler must be a built-in agent ({}) — it is \
                 resolved by name, as YAML's `verifier.agent` is. Run a custom backend as a step's agent",
                crate::backend::KNOWN.join(", ")
            )));
        }
        let cfg = self.cfg.clone();
        let dir = self.dir.clone();
        let config_base = crate::paths::config_base(&dir);

        // the driver variant: an empty `sequence.steps` means no entry list to validate, no
        // `done_if`, and an EMPTY run-set — verifiers are lazy and the driver asks.
        let asm = crate::assembly::assemble(&cfg, &config_base)?;
        crate::capability::check(&cfg, &asm.engine.verifiers, &dir)?;
        for name in cfg.agent_names() {
            crate::backend::for_name(&name)?.preflight()?;
        }

        if let Some(pid) = crate::os::detach::live_pid(&dir) {
            if pid != std::process::id() {
                return Err(Fatal::Other(anyhow::anyhow!(
                    "a loop is already running in this project (pid {pid}).\n  \
                     watch it:   rlt dashboard\n  stop it:    rlt stop\n  \
                     (if you're sure it's dead, remove rlt/private/run.pid and retry.)"
                )));
            }
        }
        crate::os::detach::write_run_pid(&dir);
        let pid_guard = RunPidGuard { dir: dir.clone() };
        let policy_guard = crate::isolation::run::register(&cfg, &dir)?;
        crate::os::signals::install();
        // behind the guard, like the YAML path: it deletes the previous run's report.
        // a resume reports under the id the run started with (A-18.3).
        let prior = if self.resume { crate::core::clock::Clock::recorded_run_id(&dir) } else { None };
        let report = crate::report::begin_as(&dir, prior);

        // THE LEDGER, decided here and nowhere else — and deliberately AFTER the double-run guard
        // above, because both branches WRITE. A resume truncates to the last KEPT gate and replays
        // what survives; a fresh run CLEARS the file, since fast-forwarding against a previous run's
        // ledger would answer this run's calls with another run's results. Doing this in `open()`
        // meant a second driver wiped a LIVE driver's ledger microseconds before the guard rejected
        // it. Both are `?`-hard: a ledger we can neither read nor reset is not one to continue past.
        let replay = if self.resume {
            let recs = crate::core::calls::truncate_to_base(&dir).map_err(Fatal::Other)?;
            eprintln!("  [resume] {} recorded call(s) will be fast-forwarded (no work, no tokens).", recs.len());
            recs
        } else {
            crate::core::calls::truncate(&dir).map_err(Fatal::Other)?;
            // …and a fresh run has no incumbent until its first kept gate: a previous run's swe-metrics
            // report describes a tree this run never kept.
            crate::metrics::measure::forget_incumbent(&dir);
            Vec::new()
        };

        let ruler = cfg.ruler_backend()?;
        let verifier_model = cfg.verifier_model(ruler);
        let verifier_timeout = cfg.verifier.timeout;

        let mut lifecycle = Lifecycle::default_pipeline(&cfg, &dir);
        let mut boot =
            crate::plugin::Bootstrap { dir: &dir, cfg: &cfg, resume: self.resume, iso_base: None };
        crate::registry::run_pre_start(&lifecycle.pre_start, &mut boot)?;
        let iso_base = boot.iso_base.expect("ResolveIsoBase set iso_base");

        #[cfg(not(unix))]
        eprintln!("  ⚠ Windows: unix-first build — the CPU-flat watchdog and process-group spawn protection are NOT active here.");
        for h in &lifecycle.on_start {
            h.fire();
        }
        let stop_hooks = StopHooks { handlers: std::mem::take(&mut lifecycle.on_stop) };

        // Same rule as the YAML path: a queue only exists while a workflow runs (see `bus::purge`).
        match crate::bus::purge(&dir) {
            0 => {}
            n => eprintln!("  [bus] purged {n} stale command(s) queued to a previous run"),
        }

        let loop_start = std::time::Instant::now();
        // ⚠ `fresh = !resume` is the load-bearing half of the e2e definition: a resumed run must
        // inherit the original `started_at_epoch` and the human wait already banked, or every
        // answered question would hand the run a fresh full allowance.
        let mut clock = crate::core::clock::Clock::open(&dir, crate::util::now_epoch(), !self.resume);
        // what an interrupted session's partial ref is named after (A-18.3).
        clock.run_id = report.run_id().to_string();
        clock.save(&dir);
        eprintln!(
            "════════════════════════════════════════════════════════════\n\
             Relentless — project {} (rust driver)\n\
             ════════════════════════════════════════════════════════════\n\
             ▶ watch live:  run `rlt dashboard` in another terminal\n\
             ⏹ stop anytime: `rlt stop`",
            cfg.project
        );

        let limits = cfg.sequence.limits.clone();
        let gate_regressions = cfg.sequence.gate_regressions;
        let worker_model_display = cfg
            .defaults
            .model
            .clone()
            .unwrap_or_else(|| cfg.worker_backend().map(|b| b.default_model().to_string()).unwrap_or_default());
        let dash = crate::state::DashboardState {
            project: cfg.project.clone(),
            model: worker_model_display,
            budget_total: limits.tokens,
            cost_limit: limits.cost,
            phase: crate::state::Phase::Starting,
            // RECORDED for the next run, not for a reader (§3.9 rule 1): a run that starts with HEAD
            // stranded on a crashed session branch recovers its real base from here.
            iso_base: iso_base.clone(),
            run_id: report.run_id().to_string(),
            ..Default::default()
        };
        let live = crate::state::LiveState::new(&dir, loop_start, dash.clone());
        let ledger =
            crate::project::RunLedger::begin(&dir, &cfg.project, std::process::id(), crate::util::now_epoch());
        let lifetime_base = ledger.prior_lifetime_sessions();

        let mut st = LoopState {
            cfg,
            ruler,
            verifier_model,
            verifier_timeout,
            dir: dir.clone(),
            config_base,
            eng: asm.engine,
            cursor: crate::core::walk::Walk::new(asm.steps),
            cur_step: None,
            next_step: None,
            dash,
            live,
            ledger,
            bus: None,
            lane: self.lane.clone(),
            budget_total: limits.tokens,
            cost_limit: limits.cost,
            // ⛔ `max_sessions` stays 0 (= the loop's "unlimited" sentinel) even when
            // `limits.sessions` is set: `over_max_sessions` is the SCHEDULER's ceiling and the
            // facade has no scheduler. All four limits are opt-in TOGETHER, through
            // `check_limits()` — enforcing this one anyway would make the surface incoherent.
            max_iter: limits.sessions,
            max_sessions: 0,
            gate_regressions,
            loop_start,
            clock,
            lifetime_base,
            session: 0,
            tokens_spent: 0,
            cost_spent: 0.0,
            per_agent: std::collections::BTreeMap::new(),
            ext: crate::plugin::Extensions::default(),
            scratch: crate::plugin::Extensions::default(),
        };
        st.ext.get::<RLTState>().git.iso_base = iso_base;
        crate::features::evidence::begin(&mut st, report.run_id());
        crate::report::record_session(&mut st, &report, self.user_settings, crate::backend::user_env::take(&self.user_env));
        st.publish();
        st.dash.lifetime_session = lifetime_base;
        // the first span opens here, against the engine's launch truth.
        *self.span_goals.borrow_mut() = st.eng.snapshot_goal_state();

        run_hook(&lifecycle.background, &mut st)?;
        // the `Baseline` pass can stop the run at launch (abort_if already true / done_if already
        // satisfied). It finalizes dash + ledger itself, so the facade only latches the outcome.
        if let Some(End::Stop(outcome)) = run_hook(&lifecycle.on_run_start, &mut st)? {
            self.ended.set(Some(outcome));
        }

        Ok(Started {
            st: Locked::new(st),
            lc: lifecycle,
            replay,
            _stop: stop_hooks,
            _policy: policy_guard,
            _pid: pid_guard,
            report,
            passed: crate::backend::user_env::passed(),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// step
// ---------------------------------------------------------------------------------------------

impl Relentless {
    /// Run ONE worker session for `step` — the same hook dispatch a YAML lap runs.
    ///
    /// The work is COMMITTED on the session branch and STAGED on the open span; nothing merges
    /// until `gate()` says so.
    pub fn step(&self, step: &Step) -> Result<StepOutcome, Fatal> {
        let _call = self.enter();
        // 1. the latch, before anything else.
        if let Some(o) = self.ended.get() {
            return Err(Fatal::Ended(o));
        }
        // 2. a fresh step is a fresh world: this is what makes memoization PER-STEP.
        self.verdicts.borrow_mut().clear();
        let body = self.resolve(step)?;
        // the run's tier is the STRONGEST any step declared — recorded here because a driver's steps
        // never reach `cfg.steps`, so `cfg.run_isolation()` cannot see them (see `iso_seen`).
        self.iso_seen.set(self.iso_seen.get().strongest(body.isolation));
        let started = self.ready()?;

        // 3. THE FAST-FORWARD. A recorded step returns its recorded outcome with no worker launched,
        //    no git touched and no tokens spent.
        if let Some(CallRecord::Step { outcome, .. }) = self.replayed(started, &body.name)? {
            {
                let mut st = started.st.borrow_mut();
                // ⛔ COUNTER RESTORATION, and it is not optional: a resume that launders the ceilings
                // is a moat hole. These are CUMULATIVE as of that step, so they are ASSIGNED, not
                // added — `over_budget`/`over_cost`/`over_iterations` then survive the restart
                // instead of getting the whole budget back for free.
                st.session = outcome.session;
                st.tokens_spent = outcome.tokens;
                st.cost_spent = outcome.cost;
                st.dash.pos = self.label_path();
                st.publish();
            }
            eprintln!("  [resume] ord {} · step `{}` — fast-forwarded", self.ord.get(), body.name);
            self.skip();
            return Ok(outcome);
        }

        let mut st = started.st.borrow_mut();
        // Between two `step()` calls the DRIVER ran — its own code, trusted, writing its history and reports.
        // The protected-path check's "between sessions" reference can't tell that from a process a worker left
        // running, and would put the driver's writes back; a new step starts from the world the driver left.
        // Between the sessions of ONE step (the `NextSession` retry below) the check still applies.
        st.ext.get::<RLTState>().protect.last = None;
        // The breadcrumb, stamped where the driver's flow ENTERS rlt. rlt cannot observe the
        // hand-written `for` between two `step()` calls, so this is the only moment the live
        // `PosFrame` stack and the snapshot are in the same place at the same time. `step_once`
        // publishes several times below and every one of them carries it.
        st.dash.pos = self.label_path();
        loop {
            // ⚠ RE-SEEDED INSIDE THE LOOP. `PickStep` `take()`s `next_step`, so a `NextSession`
            // retry (the rate-limit path) would otherwise find `None` and fall through to the
            // sequence cursor — which on a driver project is empty.
            st.next_step = Some(body.clone());
            let end = crate::loop_::step_once(&mut st, &started.lc)?;
            match end.end {
                Some(End::NextSession) => continue,
                Some(End::Stop(outcome)) => {
                    drop(st);
                    self.ended.set(Some(outcome));
                    return Err(Fatal::Ended(outcome));
                }
                None => {
                    let outcome = end.outcome.expect("a step that ran to the end reports its outcome");
                    // Recorded ON COMPLETION, and after `st` is released: a torn line means this call
                    // did not finish, which is exactly right — it must re-execute.
                    let label = self.label_path();
                    drop(st);
                    self.record(CallRecord::Step {
                        ord: self.ord.get(),
                        label,
                        outcome: outcome.clone(),
                        ts: crate::util::now_epoch(),
                    })?;
                    return Ok(outcome);
                }
            }
        }
    }

    /// A driver [`Step`] as the pipeline's [`ResolvedStep`], with the run's defaults filled in.
    fn resolve(&self, step: &Step) -> Result<ResolvedStep, Fatal> {
        let Some(name) = step.name.clone() else {
            return Err(Fatal::Other(anyhow::anyhow!(
                "rlt.step() was handed a TEMPLATE (an unnamed Step). Name it first: \
                 `template.create(\"implement\")`"
            )));
        };
        // a driver's steps never reach `assembly`, so the prompt-file refusals happen HERE: one form
        // per step, and a file that can be read now (it is read again at every session start).
        let config_base = crate::paths::config_base(&self.dir);
        let at = format!("step `{name}`");
        let mut problems =
            crate::core::prompts::pair_problems(&at, "prompt", step.prompt.as_deref(), step.prompt_file.as_deref(), &config_base);
        problems.extend(crate::core::prompts::pair_problems(
            &at,
            "role_prompt",
            step.role_prompt.as_deref(),
            step.role_prompt_file.as_deref(),
            &config_base,
        ));
        let d = &self.cfg.defaults;
        problems.extend(crate::core::prompts::pair_problems(
            "defaults",
            "role_prompt",
            d.role_prompt.as_deref(),
            d.role_prompt_file.as_deref(),
            &config_base,
        ));
        if !problems.is_empty() {
            return Err(Fatal::Other(anyhow::anyhow!("{}", crate::problem::render(&problems))));
        }
        // The run's context dirs bind only on a confining tier; this is `capability::assess`'s rule,
        // which a driver's code-built steps never reach.
        if !self.cfg.context.is_empty() && step.isolation == crate::isolation::Isolation::None {
            return Err(Fatal::Other(anyhow::anyhow!(
                "step `{name}` runs under `Isolation::None`, but the run names context directories — \
                 nothing would stop it from writing them. Confine it: `.isolation(Isolation::Sandbox)` \
                 or `Isolation::Container`"
            )));
        }
        // …and so does the network allow-list's: a tier that can enforce it, an agent that can
        // still reach its model behind it.
        if let crate::isolation::egress::Network::Allow(entries) = &self.cfg.isolation_policy.network {
            let allow = crate::isolation::egress::allow::AllowList::parse(entries).map_err(|bad| {
                let why: Vec<String> = bad.into_iter().map(|(i, w)| format!("allow[{i}]: {w}")).collect();
                Fatal::Other(anyhow::anyhow!("the network allow-list is malformed: {}", why.join("; ")))
            })?;
            let agent: &dyn crate::backend::AgentBackend = match &step.agent {
                Agent::Custom(b) => &**b,
                a => crate::backend::for_name(a.name())?,
            };
            let (on_tier, on_endpoint) = crate::isolation::egress::step_problems(step.isolation, agent, &allow);
            if let Some(m) = on_tier.into_iter().chain(on_endpoint).next() {
                return Err(Fatal::Other(anyhow::anyhow!("step `{name}`: {m}")));
            }
        }
        let write_deny = crate::protect::union(&self.cfg.defaults.write_deny, &step.write_deny);
        // The driver's steps are built in code, so `capability::check` never sees them: the
        // write_deny refusal for this tier happens HERE, before the step can run unprotected.
        match crate::protect::check_step(&name, step.isolation, &write_deny) {
            Err(problems) => return Err(Fatal::Other(anyhow::anyhow!("{}", problems.join("\n")))),
            Ok(Some(warning)) => eprintln!("  ⚠ {warning}"),
            Ok(None) => {}
        }
        // the role pair inherits AS A PAIR, as `RelentlessConfig::resolve_step` does for YAML: a step naming
        // either form replaces both of the defaults'.
        let role = if step.role_prompt.is_some() || step.role_prompt_file.is_some() {
            (&step.role_prompt, &step.role_prompt_file)
        } else {
            (&d.role_prompt, &d.role_prompt_file)
        };
        let resume = resume_of(&name, step);
        let news = step.news.clone().filter(|_| matches!(resume, Some((_, false))));
        let user_settings = self.user_settings || !leaves_out_user_settings(&name, step);
        let system_prompt = self.system_prompt.clone().filter(|_| takes_system_prompt(&name, step));
        let worker_args = worker_args(&self.cfg.defaults.worker_args, &name, step);
        Ok(ResolvedStep {
            name,
            agent: step.agent.name().to_string(),
            model: step.model.clone(),
            effort: step.effort.as_str().map(String::from),
            worker_args,
            state: step.state.clone().unwrap_or_else(|| self.cfg.defaults.state.clone()),
            retry: crate::retry::resolved(step.retry.as_ref().or(self.cfg.defaults.retry.as_ref())),
            timeout_secs: step.timeout.map(|d| d.as_secs()).or(self.cfg.defaults.timeout.map(|d| d.0)),
            memory: step.memory.unwrap_or(self.memory),
            resume,
            news,
            user_settings,
            system_prompt,
            role_prompt: role.0.clone(),
            prompt: step.prompt.clone(),
            role_prompt_file: role.1.clone(),
            prompt_file: step.prompt_file.clone(),
            // ⛔ ALWAYS. `step()` STAGES and `gate()` decides, so the driver path takes today's
            // `skip_verifiers` route unconditionally: `StageSpan` keeps the branch and extends the
            // span, while `StageMerge`/`RunVerifiers`/`GateKeepRollback` become no-ops. There is no
            // run-set to run here — verifiers are lazy and the driver asks.
            skip_verifiers: true,
            isolation: step.isolation,
            image: step
                .image
                .clone()
                .or_else(|| self.cfg.defaults.image.clone())
                .unwrap_or_else(|| crate::isolation::DEFAULT_IMAGE.to_string()),
            readonly: step.readonly.clone(),
            writable: step.writable.clone(),
            write_deny,
            custom: match &step.agent {
                Agent::Custom(b) => Some(CustomBackend(b.clone())),
                _ => None,
            },
        })
    }
}

// ---------------------------------------------------------------------------------------------
// gate — close the span, apply the policy
// ---------------------------------------------------------------------------------------------

impl Relentless {
    /// CLOSE the open span: merge everything `step()` staged since the last gate into the base
    /// branch, or discard it, according to [`OnRegression`].
    ///
    /// This is where the design's split lands. `step()` says WHEN work is produced; `gate()` says
    /// when it may LAND; and the project's `on_regression` policy says what a regression means. A
    /// driver that never gates loses nothing — every session is committed on its own branch and the
    /// span tip holds them all — but base is never touched either.
    ///
    /// # What a regression is
    ///
    /// Every verifier asked since the last gate that is now `!met`, did not error, and whose last
    /// LANDED verdict said met. **That is the whole rule** — no exclusion list, no opt-in marker. It
    /// is sound only because of the one convention this path depends on: **a verifier's `met` means
    /// GOOD** (BUILD.md §0.2 rule 3), so met→unmet always means worse. An inverted detector
    /// (met-when-bad) must be inverted before it is used as a driver verifier.
    ///
    /// Because verifiers are LAZY, the verdicts that count are the ones the driver actually asked for —
    /// including one asked for the first time long after the steps it verifiers. A gate inside `step()`
    /// could not see those, which is the reason `gate()` is a separate call at all.
    pub fn gate(&self) -> Result<GateOutcome, Fatal> {
        let _call = self.enter();
        // 1. the latch, before anything else.
        if let Some(o) = self.ended.get() {
            return Err(Fatal::Ended(o));
        }
        // 2. THE FAST-FORWARD, and this is the one that makes resume SAFE rather than merely cheap:
        //    a recorded gate must not re-execute, because re-running it would perform a real merge
        //    against a base that already has it.
        let started = self.ready()?;
        if let Some(CallRecord::Gate { outcome, gate, .. }) = self.replayed(started, "gate")? {
            // the gate's own state AS OF that gate — the ratchet references and the rollback count
            // survive the restart instead of starting clean.
            if let Some(state) = gate {
                for w in started.st.borrow_mut().eng.gate.restore(state) {
                    eprintln!("  ⚠ {w}");
                }
            }
            // the span this gate closed is behind us; the next one opens against today's tree.
            *self.span_goals.borrow_mut() = started.st.borrow().eng.snapshot_goal_state();
            self.clear_span_asks();
            self.verdicts.borrow_mut().clear();
            eprintln!("  [resume] ord {} · gate — fast-forwarded ({outcome:?})", self.ord.get());
            self.skip();
            return Ok(outcome);
        }
        let mut st = started.st.borrow_mut();

        // 3. nothing staged ⇒ nothing to decide. No ref is touched, no record is appended and no
        //    ordinal is consumed — a gate on an empty span must be free to call unconditionally at
        //    the bottom of a driver's `for`.
        let Some(tip) = st.ext.get::<RLTState>().git.span_tip.clone() else {
            // no span means nothing to roll back; a violation recorded without one (its paths were
            // restored already) must not poison the NEXT span's gate.
            st.ext.get::<RLTState>().protect.span_violation.clear();
            return Ok(GateOutcome::Nothing);
        };

        // 4. FIVE results, and four of them are not `Staged`.
        let iso_base = st.ext.get::<RLTState>().git.iso_base.clone();
        let red_file = st.cfg.session_isolation.red_file.clone();
        let staged = crate::git::stage_session(&st.dir, &iso_base, &tip, &red_file);

        // ⚠ `Conflict` and `CheckoutFailed` KEEP the span, so they return HERE, above the shared
        // body (which closes the span unconditionally). git aborted and left base untouched with
        // the tip in place: the work is still there and still gateable once the operator resolves
        // it. Reporting it as `RolledBack` would be a lie twice over — nothing was discarded, and
        // no policy decided anything.
        let failure = match staged {
            StagedSession::Conflict => Some(GateFailure::Conflict),
            StagedSession::CheckoutFailed => Some(GateFailure::CheckoutFailed),
            _ => None,
        };
        if let Some(f) = failure {
            let outcome = GateOutcome::Failed(f);
            let label = self.label_path();
            let gate = Some(st.eng.gate.state.clone());
            drop(st);
            self.record(CallRecord::Gate { ord: self.ord.get(), label, outcome, gate, ts: crate::util::now_epoch() })?;
            return Ok(outcome);
        }

        // 5. the regression set: every verifier asked since the last gate (§0.2 rule 3 is what makes
        //    an unscoped rule safe here). Sorted so `verdicts.jsonl` does not inherit a HashMap's
        //    iteration order.
        let mut fresh: Vec<(String, Verdict)> =
            self.span.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        fresh.sort_by(|a, b| a.0.cmp(&b.0));
        //    A verifier asked ONLY through `group()` is verified with its group, net (`core::groups`);
        //    everything else one by one — which, for a driver that never asks a group, is all of it.
        let landed = crate::core::verdicts::landed_met(&st.dir);
        let progress = {
            let direct = self.span_direct.borrow();
            let groups = self.span_groups.borrow();
            let members: Vec<&[String]> = groups.values().map(Vec::as_slice).collect();
            let grouped = |n: &str| members.iter().any(|m| m.iter().any(|x| x == n));
            crate::core::groups::assess(&fresh, &landed, |n| direct.contains(n) || !grouped(n), &members)
        };
        // …and the numeric half: every ratcheted verifier asked since the last gate, held to its
        // reference. Same scope as the boolean rule — the driver decides what it asks.
        let ratchet =
            if matches!(staged, StagedSession::Staged) { st.eng.gate.check(&fresh) } else { Vec::new() };
        let signature = crate::core::ratchet::signature(&fresh, |_| true, &ratchet);

        // 6/7/8 — the policy, the merge, the `verdicts.jsonl` row and the span teardown, all in the
        // body the YAML path's `GateKeepRollback` runs. One decision, two call sites.
        let pre_goals = std::mem::take(&mut *self.span_goals.borrow_mut());
        let improved = progress.improved || !st.eng.gate.improved(&fresh).is_empty();
        let verified = crate::features::gate::Verified {
            fresh: &fresh,
            regressed: progress.regressed || !ratchet.is_empty(),
            improved,
            ratchet,
            signature,
        };
        let gated = crate::features::gate::keep_or_rollback(&mut st, Some(&(tip, staged.clone())), verified, &pre_goals)?;

        let outcome = match (&staged, gated) {
            (StagedSession::Staged, crate::features::gate::Gated::Merged) => GateOutcome::Kept,
            (StagedSession::Staged, crate::features::gate::Gated::NoImprovement) => GateOutcome::NoImprovement,
            (StagedSession::Staged, _) => GateOutcome::RolledBack,
            // the worker vetoed: git already deleted the tip and the span is closed.
            (StagedSession::Vetoed, _) => GateOutcome::Failed(GateFailure::Vetoed),
            // no commits in the whole span — the tip is deleted and base is exactly as it was.
            _ => GateOutcome::Nothing,
        };

        // 9. the next span opens now, and it opens against the tree this gate just decided on.
        *self.span_goals.borrow_mut() = st.eng.snapshot_goal_state();
        let gate = Some(st.eng.gate.state.clone());
        drop(st);
        self.clear_span_asks();
        // ⚠ and the PER-STEP cache too. A gate is a state discontinuity inside a step window: after
        // a rollback the tree is base again while a memoized verdict still describes the span that
        // was just discarded, and the driver's next `if` would read it as current.
        self.verdicts.borrow_mut().clear();
        // 10. THE TRUNCATE BOUNDARY. A `Kept` row here is what `truncate_to_base` searches for: it is
        //     the only point at which this run's work is on BASE rather than parked on a per-run span
        //     branch the ledger cannot carry. Everything appended after it is provisional.
        self.record(CallRecord::Gate {
            ord: self.ord.get(),
            label: self.label_path(),
            outcome,
            gate,
            ts: crate::util::now_epoch(),
        })?;
        Ok(outcome)
    }

    /// The gate's own state: the ratchet references, the rollback counts and the oscillation
    /// history — the driver's reading of what `ratchet_regressed`, `<verifier>.ref`, `rollbacks`,
    /// `consecutive_rollbacks` and `oscillating` mean on the YAML path. Branch on it in Rust:
    /// `if rlt.gate_state().consecutive_rollbacks >= 2 { … }`, `rlt.gate_state().oscillating(4)`.
    ///
    /// Before the run has started it is the configured seeds and nothing else.
    pub fn gate_state(&self) -> crate::core::ratchet::GateState {
        let _call = self.enter();
        match self.started.get() {
            Some(s) => s.st.borrow().eng.gate.state.clone(),
            None => crate::core::ratchet::Gate::new(crate::core::ratchet::resolve_explicit(&self.cfg.sequence.ratchet), 0).state,
        }
    }

    /// The driver's baseline pass: a ratcheted verifier asked before the first `step()` sets its
    /// reference (or is checked against its seed). Replayed asks take the same path, so a resume
    /// re-derives the same reference.
    fn observe_pre_step(&self, name: &str, v: &Verdict) {
        let Some(started) = self.started.get() else { return };
        let mut st = started.st.borrow_mut();
        if st.session != 0 {
            return;
        }
        if let Some(w) = st.eng.gate.observe_pre_step(name, v) {
            eprintln!("  ⚠ {w}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// verifier — lazy, memoized per step
// ---------------------------------------------------------------------------------------------

impl Relentless {
    /// This step's verdict for `j`, running it if this step has not asked for it yet.
    ///
    /// Memoized per step, which is what makes `&&` a cost gate:
    /// `rlt.verifier(&build).met() && rlt.verifier(&load_test).met()` never reaches the 40-minute load
    /// test on a cycle where the build is red.
    ///
    /// ⚠ Returns a [`Verdict`], not a `Result` — deliberately. `Result` is `#[must_use]`, so
    /// `if rlt.verifier(&x).met()` would become `if rlt.verifier(&x)?.met()` at every site in every
    /// driver. After the latch this returns a NOT-MET verdict naming the outcome; never a
    /// fabricated `met: true`.
    pub fn verifier(&self, j: &Verifier) -> Verdict {
        let _call = self.enter();
        self.consult(j, true)
    }

    /// A condition's verdict: whether something applies (a step of a workflow, a branch of a driver), not a
    /// quality the gate guards. Run, recorded and memoized exactly like [`Relentless::verifier`], so a resumed run
    /// replays it; but it never enters the gate's regression set. A condition flips with the world (a tag
    /// that exists now and didn't when the last gate landed), and the gate would take that flip for a
    /// regression and roll back the work that follows it. A verifier the span also asked by name stays held.
    pub fn condition(&self, j: &Verifier) -> Verdict {
        let _call = self.enter();
        let held = self.span.borrow().contains_key(&j.name) || self.span_direct.borrow().contains(&j.name);
        let verdict = self.consult(j, false);
        if !held {
            self.span.borrow_mut().remove(&j.name);
        }
        verdict
    }

    /// This step's verdict for every member of `g`, each through the same lazy, memoized cache as
    /// [`Relentless::verifier`] — so a member already asked this step is not run again. Count over the result:
    /// `rlt.group(&quality).at_least(2)` is "two of three".
    ///
    /// At the next [`Relentless::gate`], a member asked ONLY through a group is verified with the group, net:
    /// one member flipping met→unmet while another goes unmet→met is not a regression. A member the
    /// driver also asked by name is held one by one. After the latch every member reads not-met.
    pub fn group(&self, g: &Group) -> GroupVerdict {
        let _call = self.enter();
        if self.ended.get().is_none() {
            self.span_groups.borrow_mut().insert(g.name.clone(), g.member_names());
        }
        let verdicts = g.members.iter().map(|j| (j.name.clone(), self.consult(j, false))).collect();
        GroupVerdict { name: g.name.clone(), verdicts }
    }

    /// One ask, by name (`verifier()`) or as a group member (`group()`): the latch, the span's by-name
    /// mark, then the lazy, memoized verdict.
    fn consult(&self, j: &Verifier, by_name: bool) -> Verdict {
        if let Some(o) = self.ended.get() {
            return Verdict::binary(false)
                .with_rationale(format!("run ended ({o:?}) — verifier `{}` was not run", j.name));
        }
        if by_name {
            self.span_direct.borrow_mut().insert(j.name.clone());
        }
        let lazy = Lazy { rlt: self, asking: Locked::new(Vec::new()) };
        lazy.verdict_for(j)
    }

    /// Forget which verifiers and groups the closed span asked for — the gate decided on them.
    fn clear_span_asks(&self) {
        self.span.borrow_mut().clear();
        self.span_direct.borrow_mut().clear();
        self.span_groups.borrow_mut().clear();
    }

    /// Run one verifier for real: resolve its paths against the project, dispatch it, and charge its
    /// ruler spend to the run exactly as `RunVerifiers` does on the YAML path.
    ///
    /// `src` is threaded through so a NATIVE verifier's `VerifierCtx` consults the same lazy cache — the
    /// `LoopState` borrow is deliberately DROPPED before the verifier runs, so a nested ask cannot
    /// panic on a double borrow.
    fn run_verifier(&self, j: &Verifier, src: &dyn VerifierSource) -> Verdict {
        let started = match self.ready() {
            Ok(s) => s,
            Err(e) => return Verdict::failed(format!("verifier `{}`: {e}", j.name)),
        };
        // THE FAST-FORWARD. A recorded verifier returns its recorded verdict — no ruler call, no script
        // fork. Its spend is ADDED (not assigned): a `Step`'s counters are cumulative as of that
        // step, but a verifier's are its own bill, and dropping them would hand `over_budget` fresh
        // headroom on every resume. The §5.4 flagship example is a three-hour verifier.
        match self.replayed(started, &j.name) {
            Err(e) => return Verdict::failed(format!("verifier `{}`: {e}", j.name)),
            Ok(Some(CallRecord::Verify { verdict, tokens, cost, .. })) => {
                let mut st = started.st.borrow_mut();
                st.tokens_spent += tokens;
                st.cost_spent += cost;
                st.publish();
                drop(st);
                eprintln!("  [resume] ord {} · verifier `{}` — fast-forwarded", self.ord.get(), j.name);
                self.skip();
                return verdict;
            }
            Ok(_) => {}
        }
        let (run_ruler, timeout, session, step, isolation, iso_base) = {
            let mut st = started.st.borrow_mut();
            let step = st.cur_step.as_ref().map(|s| s.name.clone()).unwrap_or_else(|| "driver".into());
            // §2.5: the RUN tier, never `cur_step.isolation`. A driver's verifier is an evaluator whose
            // confinement follows from its ROLE, not from whichever step happened to run last —
            // otherwise a verifier asked after a `Isolation::None` step runs unconfined, and one asked
            // after a hardened step inherits deny paths it exists to read and execute.
            let isolation = st.cfg.run_isolation().strongest(self.iso_seen.get());
            let base = st.ext.get::<RLTState>().git.iso_base.clone();
            (
                (st.ruler, st.verifier_model.clone(), st.cfg.verifier.agent.clone()),
                j.timeout.unwrap_or(st.verifier_timeout),
                st.session,
                step,
                isolation,
                base,
            )
        };
        let kind = self.resolve_kind(&j.kind);
        // an unknown own agent is a failed verdict, recorded like any other (resume counts the call).
        let (verdict, usage, agent) = match verifier_ruler(j, run_ruler) {
            Ok((ruler, model, effort, agent)) => {
                let once = || crate::core::verifier::run(
                    &kind,
                    &j.name,
                    &self.dir,
                    ruler,
                    &model,
                    &effort,
                    timeout,
                    Some(session),
                    &step,
                    isolation,
                    src,
                    Some(&iso_base),
                );
                // a verifier that failed to RUN (a spawn error, a ruler that didn't answer) has graded nothing:
                // asked once more before its error stands; the second call's spend counts too
                let (mut verdict, first) = once();
                let mut usage: Vec<crate::usage::Usage> = first.into_iter().collect();
                if let Some(error) = verdict.error() {
                    eprintln!("  verifier `{}` failed to run, asked once more: {error}", j.name);
                    let (again, more) = once();
                    usage.extend(more);
                    verdict = again;
                }
                (verdict, usage, agent)
            }
            Err(e) => (Verdict::failed(format!("verifier `{}`: {e}", j.name)), vec![], String::new()),
        };
        // §5.6: verifier spend counts against the ceilings — and against the RULER's per-agent tally.
        {
            let mut st = started.st.borrow_mut();
            st.charge_ruler_as(&agent, &usage);
            // …and the VERDICT reaches the snapshot, not just the spend. Charging alone was the whole
            // of this block, which is why a driver run published `verifiers: []` to every reader while
            // its verifiers ran. `publish()` leaves `dash.verifiers` alone when the engine has none, so
            // this upsert is what the TUI, the web BFF and Progress actually render.
            Self::publish_verdict(&mut st.dash.verifiers, &j.name, &kind, &verdict);
            // verified in THIS session's scratch, so the evidence is kept now, not at the gate.
            crate::features::evidence::keep(&mut st, Some(session), &[(j.name.clone(), verdict.clone())], isolation);
            st.dash.pos = self.label_path();
            st.publish();
        }
        // …and the call is recorded, carrying ITS OWN spend. A verifier failing to record is not a
        // reason to fail the verifier — the verdict is real and the driver should get it — but the
        // ledger is then short one row, so say so loudly rather than resume against it silently.
        if let Err(e) = self.record(CallRecord::Verify {
            ord: self.ord.get(),
            label: self.label_path(),
            verifier: j.name.clone(),
            verdict: verdict.clone(),
            tokens: usage.iter().map(|u| u.tokens.output_or_zero()).sum(),
            cost: usage.iter().filter_map(|u| u.cost_usd).sum(),
            ts: crate::util::now_epoch(),
        }) {
            eprintln!("  ⚠ could not record verifier `{}` in the call ledger — resume will re-run it: {e}", j.name);
        }
        verdict
    }

    /// Upsert one driver verdict into the published verifier scoreboard, keeping the run-lifetime LATEST
    /// per verifier — the same shape `verifiers_from_engine` produces for the YAML path, so every reader
    /// (TUI, web BFF, `rlt status`) renders both paths through one code path and one wire format.
    ///
    /// `delta` is computed against the value this verifier published last, which is why the row is
    /// updated in place rather than pushed: a driver asks the same verifier many times across a run.
    /// `in_dod`/`invariant` are FALSE by construction — a driver declares no DoD and no invariants;
    /// it decides it is done by returning.
    fn publish_verdict(views: &mut Vec<crate::state::VerifierView>, name: &str, kind: &VerifierKind, v: &Verdict) {
        let prev = views.iter().find(|p| p.name == name).and_then(|p| p.value);
        let delta = match (v.value, prev) {
            (Some(now), Some(was)) => now - was,
            _ => 0.0,
        };
        let row = crate::state::VerifierView {
            name: name.to_string(),
            verified: true,
            kind: kind.tag().to_string(),
            in_dod: false,
            invariant: false,
            // a driver has no `Lifecycle` machine (that is the engine's), so the state IS the verdict:
            // errored verifiers read `pending` because they measured nothing, not because they passed.
            state: match (v.error.is_some(), v.met) {
                (true, _) => "pending",
                (false, true) => "met",
                (false, false) => "in_progress",
            }
            .to_string(),
            met: v.met,
            value: v.value,
            max: v.max,
            target: v.target,
            delta,
            rationale: v.rationale.clone(),
            error: v.error.clone(),
        };
        match views.iter_mut().find(|p| p.name == name) {
            Some(slot) => *slot = row,
            None => views.push(row),
        }
    }

    /// A driver-constructed [`VerifierKind`] with its paths resolved against the project dir.
    ///
    /// The constructors (`Verifier::rubric`/`script`) take the path VERBATIM, because a driver builds
    /// its verifiers above `Relentless::open` (the chain is self-consuming, so it must) and cannot yet know
    /// the project root. A rubric's `inputs:` frontmatter is read here, at the same point.
    fn resolve_kind(&self, kind: &VerifierKind) -> VerifierKind {
        match kind {
            VerifierKind::Script { path } => VerifierKind::Script { path: self.dir.join(path) },
            VerifierKind::Llm { path, .. } => crate::core::verifiers::kind_for(&self.dir.join(path)),
            VerifierKind::Native { f } => VerifierKind::Native { f: f.clone() },
            // a driver's scoring definition FILE is relative to the project dir, like its scripts.
            VerifierKind::Score(s) => match &s.definition {
                crate::metrics::spec::Definition::File(p) if p.is_relative() => VerifierKind::Score(Box::new(
                    crate::metrics::ScoreVerifier { definition: crate::metrics::spec::Definition::File(self.dir.join(p)), ..(**s).clone() },
                )),
                _ => kind.clone(),
            },
            VerifierKind::Metric(_) | VerifierKind::Command(_) => kind.clone(),
        }
    }
}

/// The lazy per-step verdict store, shared by `Relentless::verifier()` and by every `VerifierCtx` a native verifier
/// receives — one memoization, one answer.
///
/// It owns the recursion stack rather than `Relentless` because a nested ask goes through the SAME
/// instance: the ctx holds `&dyn VerifierSource`, so `ctx.met(&other)` re-enters here with the stack
/// intact.
struct Lazy<'a> {
    rlt: &'a Relentless,
    asking: Locked<Vec<String>>,
}

impl VerifierSource for Lazy<'_> {
    fn verdict_for(&self, j: &Verifier) -> Verdict {
        if let Some(v) = self.rlt.verdicts.borrow().get(&j.name) {
            return v.clone();
        }
        // ⛔ Recursion is REFUSED, not overflowed: a verifier that asks for itself (directly or round
        // a cycle) names the cycle in its own verdict.
        if self.asking.borrow().iter().any(|n| n == &j.name) {
            let mut path = self.asking.borrow().clone();
            path.push(j.name.clone());
            return Verdict::failed(format!("verifier recursion: {}", path.join(" → ")));
        }
        self.asking.borrow_mut().push(j.name.clone());
        let verdict = self.rlt.run_verifier(j, self);
        self.asking.borrow_mut().pop();
        self.rlt.observe_pre_step(&j.name, &verdict);
        // ⚠ THE CACHE IS POPULATED WHETHER THE VERDICT WAS RUN OR REPLAYED. A ledger hit that skipped
        // the cache would let a second ask in the same step reach the ledger AGAIN and consume a
        // second ordinal — every later call would then answer from the wrong record, silently, by
        // one. Memoization and the ledger have to agree on what counts as ONE call.
        self.rlt.verdicts.borrow_mut().insert(j.name.clone(), verdict.clone());
        // …and into the span, so `gate()` sees every verdict asked for since the last gate — which
        // is the whole reason `gate()` exists (a gate inside `step()` could not see a lazy ask).
        self.rlt.span.borrow_mut().insert(j.name.clone(), verdict.clone());
        verdict
    }
}

// ---------------------------------------------------------------------------------------------
// ceilings, the operator bus, and the three notification levels
// ---------------------------------------------------------------------------------------------

impl Relentless {
    /// Enforce the run's ceilings HERE. **Opt-in**: a driver that never calls this has none.
    ///
    /// # Why opt-in does not weaken the moat
    ///
    /// The moat is about the **worker**, which cannot reach any of this — `rlt/private/` is carved
    /// out of its writable set, so it can neither forge a verdict nor raise its own budget. The
    /// driver is a binary the operator compiled; it could always have written different `.limits()`
    /// or not run rlt at all.
    ///
    /// Cheap by construction (it reads counters already in memory), so calling it every cycle costs
    /// nothing. Idempotent: after the latch is set every `rlt.*` call is already a no-op.
    ///
    /// On EVERY call, breach or not, it also drains the operator bus (so `rlt stop` is seen) and
    /// checks for Ctrl-C — both end the run exactly as the shipped loop does.
    pub fn check_limits(&self) -> Result<(), Fatal> {
        let _call = self.enter();
        let started = self.ready()?;
        self.operator_check(started)?;

        let limits = self.cfg.sequence.limits.clone();
        let now = crate::util::now_epoch();
        let (session, tokens, cost, wall, work) = {
            let st = started.st.borrow();
            (st.session, st.tokens_spent, st.cost_spent, st.clock.wall_secs(now), st.clock.work_secs(now))
        };
        // what this run's forks spent counts toward its ceilings
        let (fork_tokens, fork_cost) = self.forks.get().map_or((0, 0.0), |f| f.spent());
        let (tokens, cost) = (tokens + fork_tokens, cost + fork_cost);
        // the whole run's spend, when this is one of its lanes: a ceiling any lane's spend broke ends every lane
        let forks = self.forks.get().and_then(|f| f.ceiling_hit());
        if let Some(why) = self.lane.as_ref().and_then(|(l, _)| l.ceiling_hit()).or(forks) {
            return Err(self.end_now(started, RunOutcome::Halt, why, "abort:over_budget"));
        }
        // The order is the table's. A `None` limit is not checked.
        if let Some(max) = limits.sessions {
            if session >= max {
                return Err(self.end_now(
                    started,
                    RunOutcome::MaxSessions,
                    format!("reached limits.sessions={max}"),
                    "max-sessions",
                ));
            }
        }
        if let Some(max) = limits.tokens {
            if tokens >= max {
                return Err(self.end_now(started, RunOutcome::Halt, format!("over_budget: {tokens}/{max} tokens"), "abort:over_budget"));
            }
        }
        if let Some(max) = limits.cost {
            if cost >= max {
                return Err(self.end_now(started, RunOutcome::Halt, format!("over_cost: ${cost:.4}/${max:.4}"), "abort:over_cost"));
            }
        }
        // The two clock ceilings, in SECONDS, both read from the PERSISTED clock rather than from
        // process uptime — a resumed run must not be handed a fresh allowance.
        if let Some(max) = limits.wall_time {
            if wall >= max {
                return Err(self.end_now(started, RunOutcome::Halt, format!("wall_time: {wall:.0}s/{max:.0}s"), "abort:wall_time"));
            }
        }
        // ⚠ `work_time` is what makes an indefinite `hil_*` block survivable: ceilings keep firing
        // while blocked, so a run whose only ceiling is `wall_time` dies on an overnight question.
        if let Some(max) = limits.work_time {
            if work >= max {
                return Err(self.end_now(started, RunOutcome::Halt, format!("work_time: {work:.0}s/{max:.0}s"), "abort:work_time"));
            }
        }
        // not a ceiling of the run's own making: the machine is full. The run stops, and resumes when space is back.
        if let Some(why) = crate::os::disk::run_shortfall(self.cfg.min_free_disk, &self.dir) {
            return Err(self.end_now(started, RunOutcome::Disk, why, "disk"));
        }
        Ok(())
    }

    /// LATCH FIRST, then `Err` — so a driver that swallows the error still spends nothing after it.
    fn end_now(&self, started: &Started, outcome: RunOutcome, reason: String, tag: &str) -> Fatal {
        eprintln!("\n⚠ {reason} — stopping the run.");
        {
            let mut st = started.st.borrow_mut();
            st.emit(crate::plugin::LifecycleEvent::Finished { reason, ledger_tag: tag.to_string() });
        }
        self.ended.set(Some(outcome));
        Fatal::Ended(outcome)
    }

    /// Ctrl-C and the operator bus, on every ceiling check and every notification call.
    ///
    /// The bus is drained through the SHIPPED `BusDrain` handler, so `rlt send inject|budget|pause|
    /// stop|resume` cannot come to mean one thing on the YAML path and another here.
    fn operator_check(&self, started: &Started) -> Result<(), Fatal> {
        if crate::os::signals::interrupted() {
            let outcome = started.st.borrow_mut().finish_interrupted();
            self.ended.set(Some(outcome));
            return Err(Fatal::Ended(outcome));
        }
        // a run with forks: its bus is drained for them too, and what they drained is put back for the run itself
        if let Some(forks) = self.forks.get() {
            forks.pump();
            for cmd in forks.take_root_inbox() {
                let _ = crate::bus::Bus::open(&self.dir).and_then(|b| b.send(&cmd, &format!("{:013}", crate::util::now_epoch() * 1000)));
            }
        }
        let flow = {
            let mut st = started.st.borrow_mut();
            crate::features::inject::BusDrain.run(&mut st)?
        };
        if let Flow::Stop(outcome) = flow {
            self.ended.set(Some(outcome));
            return Err(Fatal::Ended(outcome));
        }
        Ok(())
    }

    /// FYI. Lands in the log and the reader; nothing is expected back.
    pub fn info(&self, msg: &str) {
        let _call = self.enter();
        self.note("info", msg);
    }

    /// A response would help; **the loop CONTINUES regardless**. This is `notify_if`'s non-terminal
    /// contract, made explicit and driver-invocable.
    pub fn ask(&self, msg: &str) {
        let _call = self.enter();
        self.note("ask", msg);
    }

    /// A line for the log and the reader. Delivers no notification.
    pub fn log(&self, msg: &str) {
        let _call = self.enter();
        self.note("log", msg);
    }

    /// The shared body of `info`/`ask`/`log`: the latch test (latch + Ctrl-C + a bus drain), then
    /// delivery.
    ///
    /// ⚠ These return `()`, so they can SET the latch but cannot RETURN the ceiling: the stop lands
    /// at the next `step()`/`gate()`/`check_limits()`/`block()`. That is still strictly better than
    /// nothing — the bus is drained and `rlt stop` is *seen* — but it does not interrupt a driver
    /// that logs and then sleeps for an hour. Only a returning call could.
    fn note(&self, level: &str, msg: &str) {
        if self.ended.get().is_some() {
            return;
        }
        let Ok(started) = self.ready() else { return };
        // A recorded note replays as a NO-OP: it consumes its ordinal and delivers nothing. Not
        // re-notifying is the point — the driver contract tells authors that an `rlt.*` call is a
        // safe place for a once-only side effect, which is exactly why `log` is recorded like the
        // rest. A `Result`-less signature means a divergence here cannot be returned, so it is
        // reported and the note is delivered again; the next returning call raises it properly.
        match self.replayed(started, &format!("note:{level}")) {
            Ok(Some(_)) => {
                self.skip();
                return;
            }
            Ok(None) => {}
            Err(e) => eprintln!("  ⚠ {e}"),
        }
        let _ = self.operator_check(started);
        if self.ended.get().is_some() {
            return;
        }
        eprintln!("  [{level}] {msg}");
        if level != "log" {
            let mut st = started.st.borrow_mut();
            crate::features::notify::driver_ping(&mut st, level, msg);
        }
        if let Err(e) = self.record(CallRecord::Note {
            ord: self.ord.get(),
            label: self.label_path(),
            level: match level {
                "info" => NoteLevel::Info,
                "ask" => NoteLevel::Ask,
                "block" => NoteLevel::Block,
                _ => NoteLevel::Log,
            },
            msg: msg.to_string(),
            ts: crate::util::now_epoch(),
        }) {
            eprintln!("  ⚠ could not record this note in the call ledger: {e}");
        }
    }

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // hil_* — the human-in-the-loop calls (`internal/HUMAN_LOOP.md` §4.5)
    //
    // THREE calls, one behaviour: **block until a human answers.** No timeout, no default, no
    // ending the run. The stopping in an earlier design existed only to avoid holding a process
    // open, which is an aesthetic worry rather than a cost — an idle rlt process spends no tokens
    // and no money, and waiting is cheaper than the machinery that avoids waiting.
    //
    // ⛔ THE WORKER CANNOT REACH THESE. `rlt hil` records an ask and exits; only a driver author,
    // at a call site they wrote, can make the loop wait. With no bound on the wait that asymmetry
    // is the ONLY thing standing between this feature and the interactive agent rlt replaces — so
    // do not add a `--wait` to `rlt hil`, and do not give `rlt.yaml` a `hil` key.
    //
    // Two things make an indefinite wait safe rather than reckless:
    //   · `rlt stop` / Ctrl-C interrupt it — the bus is drained on every poll (verified on the wire);
    //   · `work_time` excludes the waiting, so a ceiling meant to measure the agent's effort is not
    //     consumed by a human who was asleep. `wall_time` still fires: a deadline is a deadline.
    // ═════════════════════════════════════════════════════════════════════════════════════════

    /// Ask a human to pick one of `options`; returns the INDEX chosen.
    ///
    /// The base form. `options` is a **closed set**: it is recorded with the ask, so `rlt send
    /// answer` refuses anything not on the list and re-prints it, and this call cannot hand you a
    /// value you did not offer. That boundary check is the reason `hil_choose` exists next to
    /// [`Relentless::hil_input`], which cannot have one.
    pub fn hil_choose(&self, question: &str, options: &[&str]) -> Result<usize, Fatal> {
        let _call = self.enter();
        if options.len() < 2 {
            return Err(Fatal::Other(anyhow::anyhow!(
                "hil_choose needs at least two options (got {}) — a question with one answer is not a question",
                options.len()
            )));
        }
        let opts: Vec<String> = options.iter().map(|s| s.to_string()).collect();
        let answer = self.hil(AskCase::Choose, question, Some(opts.clone()))?;
        // The CLI validated it, so a miss here means a hand-edited ledger. Fall back to a hard error
        // rather than to index 0: silently picking the first option is how a "no" becomes a "yes".
        opts.iter().position(|o| o == &answer).ok_or_else(|| {
            Fatal::Other(anyhow::anyhow!("answer {answer:?} is not one of {opts:?}"))
        })
    }

    /// Ask a human a yes/no question. Sugar for a two-option [`Relentless::hil_choose`].
    ///
    /// Carries most real usage, which is why it has a name: authorize a deploy, confirm a real-world
    /// change was made, hand a failed task over. A `false` is a legitimate outcome, not an error —
    /// the human said no, and a workflow that asked should handle it.
    pub fn hil_bool(&self, question: &str) -> Result<bool, Fatal> {
        let _call = self.enter();
        Ok(self.hil(AskCase::Bool, question, Some(vec!["yes".into(), "no".into()]))? == "yes")
    }

    /// Ask a human for a value. Returns what they typed, verbatim.
    ///
    /// ⚠ An OPEN answer set: rlt cannot validate free text, so **pair it with a verifier** — ask, check,
    /// re-ask with the failure in the question. `examples/` shows the loop.
    ///
    /// ⛔ **Never route a secret through this.** The value would land in `asks.jsonl`, in the next
    /// session's `INSTRUCTIONS.md`, in `run.log`, and — because the bus is files on disk — in
    /// `rlt/private/bus/in/`. Ask the human to put the credential where credentials go and confirm
    /// with [`Relentless::hil_bool`] instead: **an answer may NAME a secret, never CONTAIN one.**
    pub fn hil_input(&self, question: &str) -> Result<String, Fatal> {
        let _call = self.enter();
        self.hil(AskCase::Input, question, None)
    }

    /// The open asks a human still owes an answer to, oldest first.
    ///
    /// Exists for the WORKER's asks, which a driver cannot otherwise see: `rlt hil` records and
    /// exits, so the id is minted somewhere this driver never was. Without a reader, a question the
    /// worker asked is invisible to the flow that has to react to it.
    ///
    /// A driver MAY choose to block on one — and that choice being the author's, at a call site, is
    /// what keeps the asymmetry above honest: the worker asked, the AUTHOR decided to wait.
    pub fn open_asks(&self) -> Vec<crate::core::asks::Ask> {
        let _call = self.enter();
        crate::core::asks::open(self.asks_dir())
    }

    /// The shared body: record the ask, page the human, wait for the ledger to carry an answer.
    fn hil(&self, case: AskCase, question: &str, options: Option<Vec<String>>) -> Result<String, Fatal> {
        let started = self.ready()?;

        // REPLAY. A recorded answer is returned verbatim, so a resumed run does not ask a human the
        // same question twice. ⚠ Only reachable for an ask BEFORE the last kept gate — `truncate_to_base`
        // drops what follows it — which is why the ledger is keyed by ask id: the fix for the rest is
        // to read `asks.jsonl` instead of the call ledger, and it stays a few lines away.
        if let Some(CallRecord::Hil { answer, .. }) = self.replayed(started, "hil")? {
            eprintln!("  [resume] ord {} · hil — answered {answer:?} already, not asking again", self.ord.get());
            self.skip();
            return Ok(answer);
        }
        self.operator_check(started)?;

        // two forks asking at once get two ids: the list is the run's, and an id is minted from what it holds
        let asking = self.lane.as_ref().map(|(lane, _)| lane.asks_lock());
        let id = self.mint_ask_id();
        let (session, step) = {
            let st = started.st.borrow();
            (st.session, st.cur_step.as_ref().map(|s| s.name.clone()))
        };
        let shown = match &self.fork_of {
            Some(fork) => format!("[fork {}] {question}", fork.name),
            None => question.to_string(),
        };
        crate::core::asks::append(
            self.asks_dir(),
            &crate::core::asks::Row::Open {
                id: id.clone(),
                case,
                question: shown,
                options: options.clone(),
                origin: crate::core::asks::Origin::Driver,
                session,
                step,
                ts: crate::util::now_epoch(),
            },
        )?;

        drop(asking);

        // The question goes on the OPERATOR'S QUEUE. This is the primary channel — `notify.cmd`
        // below is an optional push adapter on top of it, not the mechanism.
        if let Some(a) = crate::core::asks::get(self.asks_dir(), &id) {
            crate::core::asks::emit_to_operator(self.asks_dir(), &a);
        }

        // ⚠ THE ONE PLACEMENT RULE. `step()` only STAGES — work sits on a session branch until a
        // gate lands it — so a human who edits the tree while a span is open collides with staged
        // work. rlt knows when that is, so it warns; it does not refuse, because most asks (a
        // decision, a choice) touch nothing at all.
        let span_open = started.st.borrow_mut().ext.get::<RLTState>().git.span_tip.is_some();
        if span_open {
            eprintln!(
                "  ⚠ blocking with STAGED work on a session branch. If the human is going to touch \
                 the tree, `gate()` FIRST — their edits will collide with the open span."
            );
        }

        {
            let mut st = started.st.borrow_mut();
            crate::features::notify::driver_ping(&mut st, "block", &format!("[{id}] {question}"));
        }
        eprintln!("  [hil {id}] {question}");
        match &options {
            Some(o) => eprintln!("      answer: `rlt answer {id} <{}>`", o.join("|")),
            None => eprintln!("      answer: `rlt answer {id} \"<value>\"`"),
        }
        eprintln!("      or end the run with `rlt stop` — a block is interruptible.");

        // ⚠ PUBLISH, and keep publishing. Found on the wire, not in a test: without this the ask is
        // in `asks.jsonl` but NOT in `state.json`, so `rlt status`, the TUI and `rlt serve` all show
        // nothing at the one moment a human is needed — which defeats the entire mitigation for "a
        // forgotten ask hangs the run forever" (`internal/HUMAN_LOOP.md` §5.7). The age has to keep
        // moving too, or a reader shows "waiting 0s" for twenty minutes.
        started.st.borrow_mut().publish();

        let wait_from = crate::util::now_epoch();
        let answer = loop {
            // Ceilings + Ctrl-C + the bus drain that records the answer, every `BLOCK_CHUNK_SECS`.
            self.check_limits()?;
            if let Some(a) = crate::core::asks::get(self.asks_dir(), &id).and_then(|a| a.answer) {
                break a;
            }
            std::thread::sleep(Duration::from_secs(BLOCK_CHUNK_SECS));
            // Banked as we go, not at the end: a crash mid-wait must not lose the waiting time, or
            // the resumed run's `work_time` silently gains everything the human spent thinking.
            let mut st = started.st.borrow_mut();
            st.clock.add_human_wait(&self.dir, BLOCK_CHUNK_SECS as f64);
            st.publish();
        };
        let waited = crate::util::now_epoch().saturating_sub(wait_from);
        eprintln!("  [hil {id}] answered {answer:?} after {waited}s");

        self.record(CallRecord::Hil {
            ord: self.ord.get(),
            label: self.label_path(),
            case,
            msg: question.to_string(),
            answer: answer.clone(),
            ts: crate::util::now_epoch(),
        })?;
        Ok(answer)
    }

    /// Where this run's asks are: its own folder, or the folder of the run it is a fork of (one list for the operator).
    fn asks_dir(&self) -> &Path {
        self.fork_of.as_ref().map_or(&self.dir, |f| &f.root)
    }

    /// A short id an operator can retype off a phone, unique against what the ledger already holds.
    fn mint_ask_id(&self) -> String {
        let now = crate::util::now_epoch();
        let taken = crate::core::asks::all(self.asks_dir());
        (0..u64::MAX)
            .map(|seq| crate::core::asks::mint_id(seq, now))
            .find(|id| !taken.iter().any(|a| &a.id == id))
            .unwrap_or_else(|| format!("{now:x}"))
    }

    /// Cannot proceed without a human: deliver `msg` and WAIT on the operator bus until
    /// `rlt send resume` (or `rlt stop`) arrives.
    ///
    /// ⛔ **The worker cannot reach this.** The opt-in is the driver author's, in source, at one
    /// call site — which is what makes stop-and-wait legitimate here and never a mechanism.
    ///
    /// Ceilings keep firing while it waits: the loop below re-evaluates them every 5 seconds, as
    /// the rate-limit wait does, so `wall_time`/`over_budget` END the run rather than hanging until
    /// morning.
    pub fn block(&self, msg: &str) -> Result<(), Fatal> {
        let _call = self.enter();
        let started = self.ready()?;
        // A recorded block replays as a no-op — it was already answered. ⚠ This is only reachable
        // for a block BEFORE the last kept gate: `truncate_to_base` drops everything after it, so a
        // block answered later in the run is dropped with the rest and WILL ask again, with the
        // original answer unrecoverable. That cost is real, which is why the truncate prints it.
        if self.replayed(started, "note:block")?.is_some() {
            eprintln!("  [resume] ord {} · block — already answered, not asking again", self.ord.get());
            self.skip();
            return Ok(());
        }
        self.operator_check(started)?;
        {
            let mut st = started.st.borrow_mut();
            crate::features::notify::driver_ping(&mut st, "block", msg);
            // a stale Resume from before the block is not this block's answer.
            st.ext.get::<RLTState>().operator.resumed = false;
        }
        eprintln!("  [block] {msg}\n    answer with `rlt send resume`, or end the run with `rlt stop`.");
        loop {
            // the ceilings + Ctrl-C + the bus drain (which records the Resume), then the nap.
            self.check_limits()?;
            if std::mem::take(&mut started.st.borrow_mut().ext.get::<RLTState>().operator.resumed) {
                eprintln!("  [block] resumed by the operator.");
                // recorded only once ANSWERED — a block that never got its answer did not complete,
                // so a resume must ask again rather than walk past a human decision nobody made.
                self.record(CallRecord::Note {
                    ord: self.ord.get(),
                    label: self.label_path(),
                    level: NoteLevel::Block,
                    msg: msg.to_string(),
                    ts: crate::util::now_epoch(),
                })?;
                return Ok(());
            }
            std::thread::sleep(Duration::from_secs(BLOCK_CHUNK_SECS));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// pos + summary — the two calls that never spend
// ---------------------------------------------------------------------------------------------

/// One declared position in the label path.
struct PosItem {
    id: u64,
    label: String,
    max: u64,
    value: u64,
}

/// An RAII breadcrumb: "cycle 7/20". Declared once, dropped when its scope ends.
///
/// rlt cannot see a hand-written `for`, so this is how a bound reaches the reader at all. A loop
/// with no `pos` shows NO counter — never a wrong one.
pub struct PosFrame<'a> {
    id: u64,
    frames: &'a Locked<Vec<PosItem>>,
}

impl PosFrame<'_> {
    /// Where the loop is now. The VALUE only — the bound was declared once, at `pos()`.
    pub fn update(&self, i: u64) {
        if let Some(f) = self.frames.borrow_mut().iter_mut().find(|f| f.id == self.id) {
            f.value = i;
        }
    }
}

impl Drop for PosFrame<'_> {
    /// ⚠ Removes THIS frame BY ID, never by popping the top. Nothing forces guards to drop LIFO —
    /// an explicit `drop(outer)` before an inner one, or a guard stored in a struct, would pop the
    /// WRONG frame and silently corrupt the label path.
    fn drop(&mut self) {
        self.frames.borrow_mut().retain(|f| f.id != self.id);
    }
}

/// Frame ids. A plain counter: ids only need to be distinct within one process.
static NEXT_POS_ID: AtomicU64 = AtomicU64::new(1);

impl Relentless {
    /// Declare where the driver is: a label and its DECLARED bound. Spends nothing, and is
    /// unaffected by the latch.
    pub fn pos(&self, label: impl Into<String>, max: u64) -> PosFrame<'_> {
        let id = NEXT_POS_ID.fetch_add(1, Ordering::Relaxed);
        self.pos.borrow_mut().push(PosItem { id, label: label.into(), max, value: 0 });
        PosFrame { id, frames: &self.pos }
    }

    /// The label path as a reader sees it — `cycle 3/20 › attempt 2/3`.
    pub fn label_path(&self) -> String {
        let _call = self.enter();
        self.pos
            .borrow()
            .iter()
            .map(|f| format!("{} {}/{}", f.label, f.value, f.max))
            .collect::<Vec<_>>()
            .join(" › ")
    }

    /// The record this call would be answered from, if the ledger has one at this ordinal.
    ///
    /// ⚠ It also enforces the CONSISTENCY CHECK, which is the whole reason a record carries a label
    /// path. Fast-forward is sound only if the resumed driver is the same driver in the same place:
    /// ordinals alone would happily answer `rlt.verifier(&security)` with a recorded `rlt.verifier(&lint)`
    /// verdict if the flow had changed, and the run would proceed on a fabricated result. A mismatch
    /// is therefore a HARD error naming both sides — never a silent re-execution, because "the
    /// driver changed" and "the ledger is stale" are the operator's call, not rlt's.
    fn replayed(&self, started: &Started, want: &str) -> Result<Option<CallRecord>, Fatal> {
        let Some(rec) = started.replay.get(self.ord.get() as usize).cloned() else {
            return Ok(None);
        };
        let here = self.label_path();
        if rec.label() != here || rec.what() != want {
            return Err(Fatal::Other(anyhow::anyhow!(
                "resume DIVERGED at ordinal {}: the ledger recorded `{}` at \"{}\", but this run \
                 asked for `{}` at \"{}\".\n  \
                 The driver's control flow is not what it was when the ledger was written — either \
                 the binary changed, or something it branches on did.\n  \
                 Re-run WITHOUT resume to start fresh (this discards the recorded work), or restore \
                 the driver that wrote it.",
                rec.ord(),
                rec.what(),
                rec.label(),
                want,
                here
            )));
        }
        Ok(Some(rec))
    }

    /// Whether a resume's next recorded call is step `name`'s: the run replays what the step did then instead of
    /// deciding again on today's files (a refresh that finds its inputs unchanged now ran a session then).
    pub fn replays_step(&self, name: &str) -> bool {
        let _call = self.enter();
        self.resume && self.ready().is_ok_and(|s| matches!(s.replay.get(self.ord.get() as usize),
            Some(rec @ CallRecord::Step { .. }) if rec.what() == name))
    }

    /// Append a completed call and advance the ordinal. **Not called on a fast-forward** — the
    /// record is already there, and re-appending it would double every ordinal on the next resume.
    fn record(&self, rec: CallRecord) -> Result<(), Fatal> {
        crate::core::calls::append(&self.dir, &rec).map_err(Fatal::Other)?;
        self.ord.set(self.ord.get() + 1);
        Ok(())
    }

    /// Advance past a fast-forwarded call without writing anything.
    fn skip(&self) {
        self.ord.set(self.ord.get() + 1);
    }

    /// The run's human-readable status — the same view `rlt status` renders, from the same
    /// published `state.json`. Spends nothing and works before the run has started.
    pub fn summary(&self) -> String {
        let _call = self.enter();
        crate::ui::status::render(&self.dir)
    }

    /// The ledger cursor. Phase 1 reads it; today it only counts completed steps.
    pub fn ord(&self) -> u64 {
        self.ord.get()
    }

    /// The outcome the run is LATCHED to, if it has ended.
    pub fn ended(&self) -> Option<RunOutcome> {
        self.ended.get()
    }

    /// The ceilings set on this run.
    pub(crate) fn cfg_limits(&self) -> Limits {
        self.cfg.sequence.limits.clone()
    }

    /// Whether this run continues an earlier one (`--resume`): its ledger replays what already ran.
    pub fn resumes(&self) -> bool {
        self.resume
    }

    /// The project root, canonicalized.
    pub fn dir(&self) -> &Path {
        if let Some(fork) = &self.fork_of {
            // the folder of a fork is its worktree: it exists from the first look at it (an error is told by the first call)
            let _ = fork.make();
        }
        &self.dir
    }

    /// THE UNGATED-SPAN WARNING this run would print if it ended now, or `None` if everything the
    /// driver staged has been gated.
    ///
    /// Deliberately NOT one of the eleven calls, and it exists for one reason: "is the run about to
    /// tell the operator their work is stranded, and does it name the right branch" is a question
    /// only the run's own git state can answer, and a test that rebuilt the sentence from its own
    /// idea of the branch names would assert nothing.
    pub fn ungated_span(&self) -> Option<String> {
        let _call = self.enter();
        let started = self.started.get()?;
        let mut st = started.st.borrow_mut();
        let git = &mut st.ext.get::<RLTState>().git;
        crate::features::finalize::stranded_span_message(
            &git.span_branches,
            git.span_tip.as_deref(),
            &git.iso_base,
        )
    }

    /// End the run EXPLICITLY, with the outcome the driver means.
    ///
    /// The optional twelfth call. A clean drop records [`RunOutcome::GoalsMet`], because `Drop` cannot
    /// see whether `main` returned `Ok` or `Err` — this is how a driver that cares says which it was.
    /// Idempotent, and a no-op once a ceiling has already latched an outcome.
    pub fn finish(&self, outcome: RunOutcome) {
        let _call = self.enter();
        self.end(outcome, None);
    }

    /// [`Relentless::finish`], saying WHY — the stop condition the driver's own code saw fire
    /// (`"rollbacks >= 2"`, `"stalled"`). It lands where `rlt run` puts the `abort_if` term that
    /// fired: for [`RunOutcome::Halt`], `ABORT: <reason>` in the report's `stop_reason` (and
    /// `state.json`'s `finish_reason`) and `abort:<reason>` as the run ledger's end reason, word for
    /// word the YAML path's. Any other outcome records the reason as given under its usual ledger
    /// tag. A blank reason records `finish`'s wording.
    pub fn finish_with(&self, outcome: RunOutcome, reason: impl Into<String>) {
        let _call = self.enter();
        self.end(outcome, Some(reason.into()));
    }

    /// The body of [`Relentless::finish`] / [`Relentless::finish_with`].
    fn end(&self, outcome: RunOutcome, reason: Option<String>) {
        if self.ended.get().is_some() {
            return;
        }
        // NOT `ready()`: booting a whole run — preflight, guards, the banner — just to record that it
        // is over would be absurd. A driver that never spent anything simply latches.
        if let Some(started) = self.started.get() {
            let mut st = started.st.borrow_mut();
            record_end(&mut st, outcome, reason.as_deref());
        }
        self.ended.set(Some(outcome));
    }

    /// How many worker sessions this run has launched. `0` before the first `step()`.
    ///
    /// Deliberately NOT one of the eleven calls: a driver reads per-step numbers off
    /// [`StepOutcome`] and the run's own view off [`Relentless::summary`]. It exists because "did the latch
    /// really stop the pipeline" is a question only the counter can answer.
    pub fn sessions(&self) -> u32 {
        let _call = self.enter();
        self.started.get().map(|s| s.st.borrow().session).unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------------------------
// run end — BUILD.md §3.10
// ---------------------------------------------------------------------------------------------

/// Stamp the run's end into `state.json` (`dash.finished` + the reason) and the run ledger
/// (`ledger.finish`), through the SAME `emit(Finished)` every `Flow::Stop` on the YAML path uses —
/// so `rlt status`, the dashboard and `rlt history` cannot come to mean one thing per entry point.
fn record_end(st: &mut LoopState, outcome: RunOutcome, why: Option<&str>) {
    let (reason, ledger_tag) = end_wording(outcome, st.session, why);
    st.emit(crate::plugin::LifecycleEvent::Finished { reason, ledger_tag });
}

/// The run end's `(reason, ledger tag)`. A driver's own reason for a [`RunOutcome::Halt`] is worded
/// exactly as the YAML path words the `abort_if` term that fired (`features::finalize`), so a
/// report reader cannot tell which entry point stopped the run; a blank one is no reason.
fn end_wording(outcome: RunOutcome, n: u32, why: Option<&str>) -> (String, String) {
    match (outcome, why.map(str::trim).filter(|w| !w.is_empty())) {
        (RunOutcome::Halt, Some(w)) => (format!("ABORT: {w}"), format!("abort:{w}")),
        (o, Some(w)) => (w.to_string(), default_wording(o, n).1.to_string()),
        (o, None) => {
            let (reason, tag) = default_wording(o, n);
            (reason, tag.to_string())
        }
    }
}

/// [`Relentless::finish`]'s fixed wording, per outcome.
fn default_wording(outcome: RunOutcome, n: u32) -> (String, &'static str) {
    match outcome {
        // ⚠ NOT "goals-met". A driver declares no DoD — it decides it is done by RETURNING — so
        // claiming its goals were met is a statement about something that does not exist, and
        // `rlt history` rendered it beside `goals_met: 0, goals_total: 0`: vacuously true and read by
        // a human as "it finished having achieved nothing". `driver-returned` says the one thing rlt
        // actually knows. The OUTCOME stays `GoalsMet` (exit 0, the success path); only the recorded
        // wording changes, because the ledger is the human-facing artefact.
        RunOutcome::GoalsMet => (format!("driver returned after {n} session(s)"), "driver-returned"),
        RunOutcome::Halt => (format!("driver ended the run after {n} session(s)"), "abort:driver"),
        RunOutcome::MaxSessions => (format!("driver stopped at its session ceiling ({n})"), "max-sessions"),
        RunOutcome::Stopped => (format!("driver stopped the run after {n} session(s)"), "stopped"),
        RunOutcome::Disk => (format!("driver stopped the run after {n} session(s): free disk below the floor"), "disk"),
    }
}

/// RUN END. For a driver, "run end" is `main` returning or panicking — there is no post-loop
/// finalize to inherit, because on the YAML path all of it happens inside `CheckRunStop`.
///
/// Order is load-bearing: this body runs FIRST, then `Relentless`'s fields drop, which fires `StopHooks`
/// (the `on_stop` shell hook) and `RunPidGuard` (releases the double-run guard). So the run is
/// recorded as ended before anything the operator wired to `on_stop` observes it.
///
/// | how the driver ends | how rlt learns | recorded |
/// |---|---|---|
/// | a ceiling fired | the latch | the latched [`RunOutcome`] |
/// | panic | `std::thread::panicking()` | the ledger's own pessimistic `crashed` |
/// | clean drop, no latch | `Drop` | [`RunOutcome::GoalsMet`] |
///
/// The last row's `Err`-return ambiguity is ACCEPTED: `Drop` cannot see whether `main` returned
/// `Ok` or `Err`. [`Relentless::finish`] is the explicit form for a driver that wants the distinction.
impl Drop for Relentless {
    fn drop(&mut self) {
        // ⚠ ONE `eprintln!` AND NOTHING ELSE while unwinding. A panic inside `Drop` aborts the
        // process, and the frame that panicked may well have been holding a `Locked` cell — so a
        // `borrow_mut()` here would BE that second panic. The ledger already carries a pessimistic
        // `crashed` end_reason from `RunLedger::begin` and stamps the end time in its own `Drop`, so
        // the run is still recorded as failed without rlt touching anything.
        if std::thread::panicking() {
            eprintln!("  ⚠ rlt: the driver PANICKED — any staged span is left on its branch, un-merged.");
            return;
        }
        self.wind_down("discarded");
        // the run's guards and ledger write their last lines as they drop: before a fork's worktree goes, not after
        drop(self.started.take());
        if let Some(fork) = &self.fork_of {
            fork.settle(self);
        }
    }
}

impl Relentless {
    /// Writes the end of the run: the stranded span named, the ledger's end, the report. Once: a merge writes a fork's before
    /// it merges its work, and the fork's drop then has nothing left to write. `how` is what a fork's last ledger record says.
    pub(super) fn wind_down(&self, how: &str) {
        if self.wound.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let Some(started) = self.started.get() else {
            return; // nothing ever spent: there is no run to finalize.
        };
        // `try_borrow_mut` and not `borrow_mut`: a Drop that panics is strictly worse than a Drop
        // that silently skips a report, and nothing else can be holding this borrow here.
        let Some(mut st) = started.st.try_borrow_mut() else { return };

        // the ungated span, named, with the command that lands it. ⛔ Never auto-merged (rlt must not
        // make a call it was not given) and never auto-rolled-back (discarding an overnight run over a
        // late regression is far worse than keeping it).
        crate::features::finalize::report_stranded_span(&mut st);
        let base = st.ext.get::<RLTState>().git.iso_base.clone();
        if let Some(moved) = crate::git::back_to_base(&st.dir, &base) {
            eprintln!("{moved}");
        }

        // `dash.finished`, not the latch: a `Flow::Stop` that already emitted `Finished` must not be
        // re-recorded, while a latched outcome that never got emitted still must be.
        let outcome = self.ended.get().unwrap_or(RunOutcome::GoalsMet);
        if !st.dash.finished {
            record_end(&mut st, outcome, None);
        }
        if let Some(forks) = self.forks.get() {
            let (tokens, cost) = forks.spent();
            st.tokens_spent += tokens;
            st.cost_spent += cost;
        }
        // the report, from the state the run ended in — before `on_stop` fires, as on the YAML path.
        // A driver that returns `Err` reads as `done` here; `driver::run` amends it to `error`.
        let report = crate::report::from_state(&mut st, &started.report, &Ok(outcome));
        if let Err(e) = started.report.finish(&report) {
            eprintln!("  ⚠ rlt: cannot write {}: {e}", crate::paths::report_json(&st.dir).display());
        }
        crate::core::verifier::sweep_scratch(&st.dir);
        if self.fork_of.is_some() {
            // the last record of a fork's own ledger: a settled fork replays whole on a resume (see `CallRecord::Settled`)
            let _ = crate::core::calls::append(&self.dir, &CallRecord::Settled {
                ord: self.ord.get(), label: self.label_path(), how: how.to_string(), ts: crate::util::now_epoch(),
            });
        }
    }
}

/// The ruler an rubric verifier asks: its own agent, model and effort ([`Verifier::agent`] …) where it names
/// them, else the run's. A verifier with its own agent and no model gets that agent's default verifier model.
/// Returns the backend, model, effort and the agent's name (what its spend is charged to).
fn verifier_ruler(
    j: &Verifier,
    (ruler, model, agent): (&'static dyn crate::backend::AgentBackend, String, String),
) -> anyhow::Result<(&'static dyn crate::backend::AgentBackend, String, String, String)> {
    let Some(own) = &j.ruler else { return Ok((ruler, model, String::new(), agent)) };
    let effort = own.effort.clone().unwrap_or_default();
    let Some(name) = &own.agent else {
        return Ok((ruler, own.model.clone().unwrap_or(model), effort, agent));
    };
    let backend = crate::backend::for_name(name)?;
    let model = own.model.clone().unwrap_or_else(|| backend.default_summary_model().to_string());
    Ok((backend, model, effort, name.clone()))
}

/// Whether the step's agent can start a worker without the operator's own settings; says so when it can't.
fn leaves_out_user_settings(name: &str, step: &Step) -> bool {
    let can = match &step.agent {
        Agent::Custom(b) => b.capabilities().supports_project_settings_only,
        a => crate::backend::for_name(a.name()).is_ok_and(|b| b.capabilities().supports_project_settings_only),
    };
    if !can {
        eprintln!("  step `{name}`: {} can't leave out the operator's own settings; its worker loads them", step.agent.name());
    }
    can
}

/// The run's worker arguments, then those that limit the worker to the step's tools; says so where its agent
/// can't.
fn worker_args(run: &[String], name: &str, step: &Step) -> Vec<String> {
    let mut args = run.to_vec();
    let Some(tools) = &step.tools else { return args };
    let limits = match &step.agent {
        Agent::Custom(b) => b.tool_args(tools),
        a => crate::backend::for_name(a.name()).map(|b| b.tool_args(tools)).unwrap_or_default(),
    };
    if limits.is_empty() {
        eprintln!("  step `{name}`: {} can't limit its tools; its worker runs with all of them", step.agent.name());
    }
    args.extend(limits);
    args
}

/// Whether the step's agent can add text to its system prompt; says so when it can't.
fn takes_system_prompt(name: &str, step: &Step) -> bool {
    let can = match &step.agent {
        Agent::Custom(b) => b.capabilities().supports_system_prompt,
        a => crate::backend::for_name(a.name()).is_ok_and(|b| b.capabilities().supports_system_prompt),
    };
    if !can {
        eprintln!("  step `{name}`: {} can't take a system prompt; its worker runs without it", step.agent.name());
    }
    can
}

/// The agent session a step starts from — its id and whether to fork it — or `None` for a fresh
/// one, saying why when the step asked for a session (A-33). A session belongs to the agent that
/// ran it, and only a backend that can resume (or fork) can start from it.
fn resume_of(name: &str, step: &Step) -> Option<(String, bool)> {
    let (session, fork) = step.resume.as_ref()?;
    let agent = step.agent.name();
    let caps = match &step.agent {
        Agent::Custom(b) => b.capabilities(),
        a => crate::backend::for_name(a.name()).ok()?.capabilities(),
    };
    let why = if session.agent != agent {
        format!("it ran on {}, this step runs on {agent}", session.agent)
    } else if !caps.supports_resume {
        format!("{agent} can't resume a session")
    } else if *fork && !caps.supports_fork {
        format!("{agent} can't fork a session")
    } else {
        return Some((session.id.clone(), *fork));
    };
    eprintln!("  step `{name}` starts fresh, not from session {}: {why}", session.id);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn open() -> (tempfile::TempDir, Relentless) {
        let tmp = tempfile::tempdir().unwrap();
        let rlt = Relentless::open(tmp.path()).unwrap();
        (tmp, rlt)
    }

    /// A-25: an rubric verifier without its own ruler asks the run's; with its own model the run's agent;
    /// with its own agent that agent (its default verifier model unless named), charged to it.
    #[test]
    fn an_llm_verifier_picks_its_own_ruler() {
        let claude = crate::backend::for_name("claude").unwrap();
        let run = || (claude, "run-model".to_string(), "claude".to_string());
        let rubric = || Verifier::rubric("r", "r.md");
        let (b, m, e, a) = verifier_ruler(&rubric(), run()).unwrap();
        assert_eq!((b.name(), m.as_str(), e.as_str(), a.as_str()), ("claude", "run-model", "", "claude"));
        let (b, m, e, _) = verifier_ruler(&rubric().model("own").effort(crate::driver::Effort::High), run()).unwrap();
        assert_eq!((b.name(), m.as_str(), e.as_str()), ("claude", "own", "high"));
        let codex = crate::backend::for_name("codex").unwrap();
        let (b, m, _, a) = verifier_ruler(&rubric().agent(Agent::Codex), run()).unwrap();
        assert_eq!((b.name(), m.as_str(), a.as_str()), ("codex", codex.default_summary_model(), "codex"));
        let mut unknown = rubric();
        unknown.ruler = Some(crate::core::model::VerifierRuler { agent: Some("nope".into()), ..Default::default() });
        assert!(verifier_ruler(&unknown, run()).is_err());
    }

    /// The ruler builders set exactly the fields YAML's `verifier:` block sets, and nothing is set
    /// until asked: the defaults stay `RelentlessConfig::default()`'s.
    #[test]
    fn the_ruler_builders_set_the_verifier_block() {
        let (_tmp, rlt) = open();
        let untouched = RelentlessConfig::default().verifier;
        assert_eq!((rlt.cfg.verifier.agent.as_str(), rlt.cfg.verifier.model.as_deref()), (untouched.agent.as_str(), None));
        assert_eq!(rlt.cfg.verifier.timeout, untouched.timeout);

        let rlt = rlt.verifier_agent(Agent::Codex).verifier_model("gpt-cheap").verifier_timeout(900);
        assert_eq!(rlt.cfg.verifier.agent, "codex");
        assert_eq!(rlt.cfg.verifier.model.as_deref(), Some("gpt-cheap"));
        assert_eq!(rlt.cfg.verifier.timeout, 900);
        assert_eq!(rlt.custom_ruler, None);
    }

    /// A custom ruler is refused when the run boots — even one that NAMES a built-in agent, which
    /// would otherwise silently become that agent — and a later built-in ruler withdraws the refusal.
    #[test]
    fn a_custom_ruler_is_refused_at_boot_and_a_built_in_one_clears_it() {
        let (_tmp, rlt) = open();
        let custom = Agent::Custom(Arc::new(crate::backend::claude::Claude));
        let rlt = rlt.verifier_agent(Agent::Codex).verifier_agent(custom.clone());
        assert_eq!(rlt.cfg.verifier.agent, "codex", "the refused agent never reaches the config");
        let err = rlt.check_limits().unwrap_err().to_string();
        assert!(err.contains("Agent::Custom(`claude`)") && err.contains("built-in agent"), "{err}");
        assert!(rlt.started.get().is_none(), "nothing booted");

        let (_tmp, rlt) = open();
        let rlt = rlt.verifier_agent(custom).verifier_agent(Agent::Copilot);
        assert_eq!((rlt.custom_ruler.as_deref(), rlt.cfg.verifier.agent.as_str()), (None, "copilot"));
    }

    /// The resolved step's role pair: the step's own when it sets either form, else the defaults' —
    /// AS A PAIR, exactly as `RelentlessConfig::resolve_step` merges a YAML step.
    #[test]
    fn a_steps_role_prompt_replaces_the_defaults_as_a_pair() {
        let (tmp, mut rlt) = open();
        std::fs::create_dir_all(tmp.path().join("rlt/roles")).unwrap();
        std::fs::write(tmp.path().join("rlt/roles/tester.md"), "you are a tester\n").unwrap();
        rlt.cfg.defaults.role_prompt = Some("you are the default".into());
        let role = |s: &Step| {
            let r = rlt.resolve(s).unwrap();
            (r.role_prompt, r.role_prompt_file)
        };

        assert_eq!(role(&Step::new("a")), (Some("you are the default".into()), None));
        assert_eq!(role(&Step::new("b").role_prompt("you are a reviewer")), (Some("you are a reviewer".into()), None));
        assert_eq!(role(&Step::new("c").role_prompt_file("roles/tester.md")), (None, Some("roles/tester.md".into())));
    }

    /// A step carrying both role forms, or a role file that cannot be read, is refused before it runs.
    #[test]
    fn a_bad_step_role_prompt_is_refused() {
        let (_tmp, rlt) = open();
        let mut both = Step::new("a").role_prompt("inline");
        both.role_prompt_file = Some("roles/r.md".into());
        let err = rlt.resolve(&both).unwrap_err().to_string();
        assert!(err.contains("are alternatives"), "{err}");
        let err = rlt.resolve(&Step::new("b").role_prompt_file("roles/gone.md")).unwrap_err().to_string();
        assert!(err.contains("cannot read"), "{err}");
    }

    /// `finish_with`'s wording: a halt reads as the YAML path's `abort_if` stop, any other outcome
    /// keeps its tag, and no reason (or a blank one) is `finish`'s fixed wording.
    #[test]
    fn the_end_wording_names_the_drivers_reason() {
        let w = |o, why| end_wording(o, 3, why);
        assert_eq!(w(RunOutcome::Halt, Some("rollbacks >= 2")), ("ABORT: rollbacks >= 2".into(), "abort:rollbacks >= 2".into()));
        assert_eq!(w(RunOutcome::Stopped, Some("operator said so")), ("operator said so".into(), "stopped".into()));
        assert_eq!(w(RunOutcome::Halt, Some("  ")), ("driver ended the run after 3 session(s)".into(), "abort:driver".into()));
        assert_eq!(w(RunOutcome::Halt, None), w(RunOutcome::Halt, Some("")));
        assert_eq!(w(RunOutcome::GoalsMet, None), ("driver returned after 3 session(s)".into(), "driver-returned".into()));
        assert_eq!(w(RunOutcome::MaxSessions, None).1, "max-sessions");
    }

    /// `finish_with` latches like `finish`, and once latched neither records again.
    #[test]
    fn finish_with_latches_and_is_idempotent() {
        let (_tmp, rlt) = open();
        rlt.finish_with(RunOutcome::Halt, "stalled");
        assert_eq!(rlt.ended(), Some(RunOutcome::Halt));
        rlt.finish(RunOutcome::GoalsMet);
        assert_eq!(rlt.ended(), Some(RunOutcome::Halt), "the first end wins");
    }
}

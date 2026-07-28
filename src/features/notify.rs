//! Stuck-notification delivery (STUCK_NOTIFY.md) — the non-terminal half of `abort_if`.
//!
//! The two load-bearing rules, both enforced here:
//!  1. **Notify ≠ block.** This handler ALWAYS returns [`Flow::Continue`]. A flagged loop keeps
//!     running; the human is a side-channel, never a gate. That is the whole reason the feature is
//!     not just "put the detector in `abort_if`".
//!  2. **A worker-authored reason must not become worker-authored CODE.** `{{reason}}` is routinely
//!     written by the worker (the `blocked` detector reads `agg/state/BLOCKED.md`), and delivery runs
//!     through `sh -c`, so every substituted value is POSIX-shell-quoted — see [`shq`].

use anyhow::Result;

use crate::loop_::{AGGScratch, AGGState, Flow, Handler, LoopState};

/// POSIX-quote a value so the shell parses it as exactly ONE argv element, whatever it contains
/// (`;`, `$(…)`, backticks, newlines, `&&`). Nothing inside `'…'` is special to `sh`, so there is no
/// escape table — only the close-reopen trick for a literal quote.
///
/// This replaces STUCK_NOTIFY §5's "substitute after shell-splitting": `hooks::run` execs
/// `sh -c "<string>"`, so there is no argv to substitute into and the string IS re-parsed after
/// substitution. Quoting gets the same guarantee without writing a shell splitter.
///
/// ponytail: POSIX only. The `cmd /C` path in `hooks.rs` would need its own quoting rules; agg's
/// isolation and e2e suite are already unix-only. Upgrade when Windows is actually supported.
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Substitute the `{{…}}` vars into one command string, each value shell-quoted.
///
/// SINGLE PASS over the raw string: a value that happens to contain `{{project}}` must not itself be
/// expanded, so this scans once rather than calling `replace` per variable.
fn template(cmd: &str, vars: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(cmd.len());
    let mut rest = cmd;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            break; // unterminated `{{` — leave the remainder verbatim
        };
        let key = after[..close].trim();
        match vars.iter().find(|(k, _)| *k == key) {
            Some((_, val)) => {
                out.push_str(&rest[..open]);
                out.push_str(&shq(val));
            }
            // an unknown placeholder is passed through untouched rather than blanked: a silent empty
            // string in a `curl -d` is a notification that looks delivered and says nothing.
            None => out.push_str(&rest[..open + 2 + close + 2]),
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    out
}

/// Fire `notify.cmd` when `notify_if` is true (cooldown-debounced) or when `abort_if` is stopping the
/// run (§8.5 — once, cooldown ignored, `{{reason}}` = the halt expression).
///
/// Registered LAST on `on_gate`, after `GateKeepRollback`: a rolled-back session's judging is undone
/// there, so running earlier would notify about work that no longer exists.
pub struct NotifyOnStuck;

impl Handler for NotifyOnStuck {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        // `res` is still in scratch here — only `CheckRunStop` (on_session_end) takes it.
        let res = ctx.scratch.get::<AGGScratch>().res.as_ref().expect("an on_verify handler set scratch.res");
        let halting = res.halt;
        // read before `ctx` is re-borrowed mutably below
        let stopping = res.stop;
        let notify = res.notify.clone();
        let halt_reason = res.halt_reason.clone();

        // A halt is terminal, so it pings once regardless of the debounce and does not consume it.
        if halting {
            halt_ping(ctx, halt_reason.as_deref());
            return Ok(Flow::Continue);
        }

        let Some(cfg) = ctx.cfg.sequence.notify.clone() else {
            return Ok(Flow::Continue);
        };
        if cfg.cmd.is_empty() {
            return Ok(Flow::Continue);
        }
        // `!stopping` FIRST, before the cooldown is consulted: a run that just succeeded needs no
        // human (§12.8), and burning the debounce on a suppressed cycle would silence a real
        // notification later. `done_if` and `notify_if` measure different axes, so a session can
        // easily satisfy both — a 3am page for a finished run is exactly the nagging this feature
        // exists to avoid.
        let reason = match notify {
            Some(r) if !stopping && self.cooled_down(ctx, cfg.cooldown_sessions) => r,
            _ => return Ok(Flow::Continue),
        };
        ctx.ext.get::<AGGState>().notify.last_notify_session = Some(ctx.session);
        deliver(ctx, &cfg, &reason, "stuck");

        // ALWAYS Continue — notify is pure signal. A halt still stops the run, but via
        // `CheckRunStop` reading `res.halt`, exactly as it did before this handler existed.
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "NotifyOnStuck"
    }
}

/// The §8.5 halt ping: fired ONCE when `abort_if` stops the run, ignoring (and not consuming) the
/// cooldown, because a terminal event is not something to debounce.
///
/// A free fn because there are TWO halt paths that can reach a configured `notify:` — the gate
/// (`NotifyOnStuck`, a mid-run abort) and the baseline pass (`setup::Baseline`, `abort_if` already
/// true at launch). The baseline one is the likelier of the two in practice: a stale
/// `agg/state/BLOCKED.md` survives a crash, a reboot and a rollback, so the very config a user
/// writes to be paged when the loop stops would have stopped and paged nobody. Owning the reason
/// composition here is what keeps the two paths from drifting.
pub(crate) fn halt_ping(ctx: &mut LoopState, halt_reason: Option<&str>) {
    let Some(cfg) = ctx.cfg.sequence.notify.clone() else {
        return;
    };
    if cfg.cmd.is_empty() {
        return;
    }
    // §8.5 says `{{reason}} = halt_reason`, which is the raw expression. Kept — but a bare
    // `blocked OR over_iterations` tells a human nothing, and "stop + notify" (§4) exists precisely
    // to tell them WHY. So append the winning judge's rationale when the expression names one:
    // `blocked OR over_iterations — BLOCKED: need the prod key`. A ceiling-only expression names no
    // judge, so `notify_reason` echoes the expression back and the message stays exactly as §8.5
    // specifies. Compare SANITIZED forms, using the very function `notify_reason` ends with: it
    // normalises whitespace, strips control chars AND caps the length, so a raw
    // `blocked  OR  over_iterations` — or an `abort_if` past the 400-char cap, where a
    // whitespace-only collapse still never equals the truncated echo — cannot be appended to itself.
    // It also makes the "capped at 400 chars, always one line" contract hold on the halt path, not
    // just on the `notify_if` one.
    let expr = halt_reason.unwrap_or("abort_if").to_string();
    let detail = ctx.eng.notify_reason(&expr);
    let clean = crate::core::engine::sanitize_reason(&expr, &expr);
    let reason = if detail == clean { clean } else { format!("{clean} — {detail}") };
    deliver(ctx, &cfg, &reason, "abort");
}

/// Template the `{{…}}` vars into every command and run them. Shared by both callers so the
/// variable set, the quoting and the jail cannot diverge between the live and terminal paths.
fn deliver(ctx: &mut LoopState, cfg: &crate::core::config::NotifyCfg, reason: &str, kind: &str) {
    let step = ctx.cur_step.as_ref().map(|s| s.name.clone()).unwrap_or_default();
    let vars = [
        ("reason", reason.to_string()),
        ("project", ctx.cfg.project.clone()),
        ("session", ctx.session.to_string()),
        ("step", step),
    ];
    let cmds: Vec<String> = cfg.cmd.iter().map(|c| template(c, &vars)).collect();

    // Same jail as the step that just ran (ISOLATION.md §14): `notify.cmd` lives in agg.yaml and
    // typically execs a project script — both inside the worker's writable cwd. `hooks::run`
    // SKIPS a command it cannot confine rather than running it unconfined. At baseline there is no
    // step yet, and none has run, so `None` is both the fallback and the correct tier.
    let tier = ctx.cur_step.as_ref().map(|s| s.isolation).unwrap_or(crate::isolation::Isolation::None);
    let dir = ctx.dir.to_path_buf();
    // Surface it to the TUI / `agg status` / the web app BEFORE running the command: delivery is
    // foreground and a slow `curl` must not delay the operator's own dashboard learning about it.
    ctx.dash.notify_session = Some(ctx.session);
    ctx.dash.notify_reason = reason.to_string();
    ctx.publish();
    eprintln!("  [notify:{kind}] {reason}");
    // ponytail: foreground and untimed, exactly like every other agg hook — a notification you
    // want DELIVERED before the loop moves on. The ceiling is a delivery command that hangs (a
    // `curl` to a dead host with no `--max-time`), which stalls the loop until the watchdog or the
    // operator intervenes; the docs tell users to bound their own command. Upgrade path if that
    // proves insufficient: `hooks::spawn_background` here, at the cost of losing the exit status.
    crate::hooks::run("notify", &cmds, &dir, tier);
}

impl NotifyOnStuck {
    /// `cooldown_sessions` = the MINIMUM gap between two fires. `0` ⇒ every qualifying cycle.
    fn cooled_down(&self, ctx: &mut LoopState, cooldown: u32) -> bool {
        match ctx.ext.get::<AGGState>().notify.last_notify_session {
            None => true,
            Some(last) => ctx.session.saturating_sub(last) >= cooldown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The security property, asserted on what the SHELL actually does — not on the composed string.
    /// A worker writing this into `agg/state/BLOCKED.md` must get a notification containing the text,
    /// never an execution of it.
    #[test]
    fn a_hostile_reason_is_one_argv_element_not_code() {
        let dir = std::env::temp_dir().join(format!("agg-notify-shq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pwned = dir.join("PWNED");
        let out = dir.join("got.txt");

        let hostile = format!("'; touch {} ; echo '$(id)`id`&& ohno", pwned.display());
        let cmd = template(&format!("printf %s {{{{reason}}}} > {}", out.display()), &[("reason", hostile.clone())]);
        crate::hooks::run("test", &[cmd], &dir, crate::isolation::Isolation::None);

        let got = std::fs::read_to_string(&out).unwrap_or_default();
        let escaped = pwned.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!escaped, "the reason ESCAPED — the shell executed part of it");
        assert_eq!(got, hostile, "the reason must arrive byte-identical, as one argv element");
    }

    #[test]
    fn templating_substitutes_known_vars_only_and_does_not_re_expand() {
        // an unknown placeholder survives verbatim (a blanked one is a silently empty notification)
        let got = template("x {{nope}} y", &[("reason", "r".into())]);
        assert_eq!(got, "x {{nope}} y");
        // a value containing a placeholder is NOT expanded a second time
        let got = template("{{reason}}", &[("reason", "{{project}}".into()), ("project", "p".into())]);
        assert_eq!(got, "'{{project}}'");
        // every var lands
        let got = template("{{project}}/{{session}}/{{step}}", &[
            ("project", "proj".into()),
            ("session", "7".into()),
            ("step", "worker".into()),
        ]);
        assert_eq!(got, "'proj'/'7'/'worker'");
    }

    #[test]
    fn shq_handles_the_quote_itself() {
        assert_eq!(shq("it's"), r"'it'\''s'");
        assert_eq!(shq(""), "''");
    }

    // ── HANDLER-level tests. `NotifyOnStuck` is where ALL the delivery POLICY lives (the debounce,
    //    the halt exemption, the success suppression, the jail) and none of it is expressible as a
    //    pure function. `tests/plugin_api.rs::probe_state` already stands a `LoopState` up from
    //    OUTSIDE the crate, so building one here is cheap — and every assertion below reads a file
    //    that a REAL `sh -c` wrote through the REAL `hooks::run`, i.e. what was DELIVERED, never a
    //    flag the handler set. ─────────────────────────────────────────────────────────────────────

    use crate::core::config::AggConfig;
    use crate::core::engine::{CycleResult, Engine};
    use crate::core::model::{Judge, JudgeKind, Lifecycle, Verdict};
    use crate::isolation::Isolation;
    use std::path::Path;

    /// A project dir whose `notify:` block runs ONE command appending to a file in the project root.
    /// That file is the delivery sink for every test below.
    fn project(cooldown: u32, cmd: &str) -> (tempfile::TempDir, AggConfig) {
        let tmp = tempfile::tempdir().unwrap();
        // both runtime roots — the step's STATE.md is worker-writable, the run ledger the probe
        // `LoopState` opens is AGG-OWNED under `private/`.
        std::fs::create_dir_all(crate::paths::agg_dir(tmp.path())).unwrap();
        std::fs::create_dir_all(crate::paths::private_dir(tmp.path())).unwrap();
        let yaml = format!(
            "project: probe\nsequence:\n  steps: []\n  notify:\n    cooldown_sessions: {cooldown}\n    cmd:\n      - '{cmd}'\n"
        );
        let path = tmp.path().join("agg.yaml");
        std::fs::write(&path, yaml).unwrap();
        let cfg = AggConfig::load(&path).unwrap();
        (tmp, cfg)
    }

    /// What the delivery command actually received, one line per fire, in order.
    fn pings(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("pings.txt")).unwrap_or_default().lines().map(str::to_string).collect()
    }

    /// An engine that names no judge: `notify_if` is `None`, so the `CycleResult` these tests plant
    /// in scratch is the only source of a reason — the handler's POLICY is isolated from the core's
    /// reason-picking, which `core::engine`'s own tests already cover.
    fn quiet_engine() -> Engine {
        Engine::new(vec![], "iterations > 999999".into(), None, None).unwrap()
    }

    /// A `LoopState` a handler can run against, mirroring `tests/plugin_api.rs::probe_state`.
    /// `tier` is the CURRENT step's isolation — the value `deliver` must resolve the jail from.
    fn state<'a>(cfg: &'a AggConfig, dir: &'a Path, eng: Engine, tier: Isolation) -> LoopState<'a> {
        let loop_start = std::time::Instant::now();
        let dash = crate::state::DashboardState::default();
        LoopState {
            cfg,
            ruler: crate::backend::for_name("claude").unwrap(),
            judge_model: "m".into(),
            judge_timeout: 1,
            dir,
            config_base: dir,
            eng,
            cursor: crate::core::sequence::Cursor::new(vec![]),
            cur_step: Some(crate::core::config::ResolvedStep {
                name: "worker".into(),
                agent: "claude".into(),
                model: None,
                effort: None,
                worker_args: vec![],
                state: "agg/state/STATE.md".into(),
                role_prompt: None,
                prompt: None,
                skip_judges: false,
                isolation: tier,
                image: crate::isolation::DEFAULT_IMAGE.into(),
            }),
            live: crate::state::LiveState::new(dir, loop_start, dash.clone()),
            dash,
            ledger: crate::project::RunLedger::begin(dir, "probe", 0, 0),
            bus: None,
            budget_total: None,
            cost_limit: None,
            max_iter: None,
            max_sessions: 0,
            gate_regressions: false,
            loop_start,
            lifetime_base: 0,
            session: 0,
            tokens_spent: 0,
            cost_spent: 0.0,
            per_agent: std::collections::BTreeMap::new(),
            ext: crate::plugin::Extensions::default(),
            scratch: crate::plugin::Extensions::default(),
        }
    }

    /// Plant the cycle an on_verify handler would have left in scratch, then run ONE session's gate.
    fn cycle(st: &mut LoopState, session: u32, notify: Option<&str>, stop: bool, halt: Option<&str>) {
        st.session = session;
        st.scratch.get::<AGGScratch>().res = Some(CycleResult {
            stop,
            halt: halt.is_some(),
            halt_reason: halt.map(str::to_string),
            notify: notify.map(str::to_string),
            ..CycleResult::default()
        });
        let flow = NotifyOnStuck.run(st).expect("delivery is best-effort — the handler never errors");
        assert!(matches!(flow, Flow::Continue), "notify is PURE SIGNAL: it must never gate the loop");
    }

    /// KILLS `session - last >= cooldown` → `> cooldown` (§9, §12.10). The suppression half is
    /// observable in the shipped e2e fixture; the REFIRE half — "fires again at exactly N+cooldown" —
    /// is observable nowhere, and under `>` every escalation after the first arrives a session late
    /// for the rest of an overnight run.
    #[test]
    fn the_debounce_suppresses_its_window_and_refires_at_exactly_n_plus_cooldown() {
        let (tmp, cfg) = project(3, r#"printf "%s\n" {{session}} >> pings.txt"#);
        let mut st = state(&cfg, tmp.path(), quiet_engine(), Isolation::None);
        for s in 1..=5 {
            cycle(&mut st, s, Some("verdicts are flat"), false, None);
        }
        assert_eq!(pings(tmp.path()), ["1", "4"], "fires at 1, silent through 3, REFIRES at 1+3 — not at 5");

        // `cooldown_sessions: 0` is documented as "every qualifying cycle" — the ladder's first rung.
        let (tmp0, cfg0) = project(0, r#"printf "%s\n" {{session}} >> pings.txt"#);
        let mut st0 = state(&cfg0, tmp0.path(), quiet_engine(), Isolation::None);
        for s in 1..=3 {
            cycle(&mut st0, s, Some("verdicts are flat"), false, None);
        }
        assert_eq!(pings(tmp0.path()), ["1", "2", "3"], "0 ⇒ no debounce at all");
    }

    /// KILLS both halves of §12.10's terminal-ping rule, neither of which any fixture combines with a
    /// live `notify_if` today: (a) gating the halt branch on `cooled_down` drops the "your overnight
    /// run gave up, here is why" page whenever a stuck ping happened recently — precisely when a halt
    /// is likeliest; (b) letting the halt CONSUME the debounce slides the next live ping a session
    /// late. In production a halt ends the run, so (b) is only observable at handler level — which is
    /// the argument for pinning it here rather than in the e2e.
    #[test]
    fn the_halt_ping_ignores_the_debounce_and_does_not_consume_it() {
        let (tmp, cfg) = project(3, r#"printf "%s:%s\n" {{session}} {{reason}} >> pings.txt"#);
        let mut st = state(&cfg, tmp.path(), quiet_engine(), Isolation::None);
        cycle(&mut st, 1, Some("verdicts are flat"), false, None); // opens a 3-session debounce
        cycle(&mut st, 2, None, false, Some("over_iterations")); // …the halt fires straight through it
        cycle(&mut st, 3, Some("verdicts are flat"), false, None); // still inside the ORIGINAL window
        cycle(&mut st, 4, Some("verdicts are flat"), false, None); // 4 = 1+3 ⇒ due, if the halt stayed out
        assert_eq!(pings(tmp.path()), ["1:verdicts are flat", "2:over_iterations", "4:verdicts are flat"]);
    }

    /// KILLS reading `res.notify` without `res.stop`, and KILLS evaluating the cooldown BEFORE that
    /// check (§12.8). Three docs promise `notify.cmd` does not fire when `done_if` is satisfied and
    /// nothing enforced it, so a run that SUCCEEDED paged a human at 3am — and burned the debounce on
    /// the way out, silencing the next real signal.
    #[test]
    fn a_successful_session_never_pings_and_never_burns_the_debounce() {
        let (tmp, cfg) = project(3, r#"printf "%s\n" {{session}} >> pings.txt"#);
        let mut st = state(&cfg, tmp.path(), quiet_engine(), Isolation::None);
        cycle(&mut st, 1, Some("the detector is still shouting"), true, None);
        assert!(pings(tmp.path()).is_empty(), "done_if satisfied — a finished run is not a cry for help");
        assert!(
            st.ext.get::<AGGState>().notify.last_notify_session.is_none(),
            "…and the suppressed cycle must not start a debounce it never used"
        );
        // proof the suppression is about `stop` and not about the fixture being unable to deliver:
        // the very next non-final cycle pings, which it could not do had session 1 opened a window.
        cycle(&mut st, 2, Some("the detector is still shouting"), false, None);
        assert_eq!(pings(tmp.path()), ["2"]);
    }

    /// KILLS comparing the halt expression against `notify_reason`'s echo on anything other than the
    /// SANITIZED form. `notify_reason` collapses whitespace, strips control chars and caps at 400 —
    /// so a raw expression that differs from its own echo in ANY of those three ways gets appended to
    /// itself and the operator is paged the guard twice. A whitespace-only collapse (the obvious
    /// half-fix) still fails the third case, which is why the cap is tested here too: docs/CONFIG.md
    /// promises `{{reason}}` is one line capped at 400 chars, and that has to hold on the halt path.
    #[test]
    fn a_non_canonical_abort_expression_is_never_appended_to_itself() {
        let (tmp, cfg) = project(0, r#"printf "%s\n" {{reason}} >> pings.txt"#);
        let mut st = state(&cfg, tmp.path(), quiet_engine(), Isolation::None);
        // padded, tab-bearing and multi-line spellings a YAML block scalar produces naturally
        cycle(&mut st, 1, None, false, Some("over_iterations  OR \t over_budget"));
        cycle(&mut st, 2, None, false, Some("over_iterations\nOR over_budget\n"));
        // …and one past the 400-char cap, where the echo is truncated and can never equal the raw text
        let long = ["over_iterations"; 30].join(" OR ");
        assert!(long.len() > 400, "the fixture has to exceed the cap to test it");
        cycle(&mut st, 3, None, false, Some(&long));

        let got = pings(tmp.path());
        assert_eq!(got[0], "over_iterations OR over_budget");
        assert_eq!(got[1], "over_iterations OR over_budget");
        assert_eq!(got.len(), 3, "one line per halt — a multi-line reason would split a line-oriented sink");
        assert!(!got[2].contains(" — "), "the capped expression must not be appended to its own echo: {}", got[2]);
        assert_eq!(got[2].chars().count(), 401, "…and it arrives capped at 400 chars plus the ellipsis");
    }

    /// KILLS `let tier = Isolation::None;` in [`deliver`] (§12.5, ISOLATION.md §14). `notify.cmd`
    /// typically execs a project script, and both the command and the script live in the WORKER'S
    /// WRITABLE CWD — so a confined worker escapes through the notification if delivery runs unjailed.
    ///
    /// The probe writes to `$HOME`, and it has to be `$HOME` rather than, say, the crate's `target/`:
    /// the profile grants cwd, `$TMPDIR` AND `/private/tmp` unconditionally, so a probe under a repo
    /// that happens to sit in a temp dir would be writable INSIDE the jail and this would fail for the
    /// wrong reason. Unconfined the file lands; under `sandbox` it cannot land by ANY route — the
    /// profile denies it, and on a host with no wrapper (or a nested jail that refuses, which is why
    /// the twin test in `hooks.rs` is `#[ignore]`d) `hooks::run` SKIPS the command rather than run it
    /// unconfined. Every branch agrees on "absent", so this needs no working Seatbelt to mean
    /// something; only a dropped tier makes the file appear.
    #[test]
    #[cfg(unix)]
    fn delivery_runs_in_the_current_steps_jail() {
        let home = std::env::var("HOME").expect("a unix test host has $HOME");
        let escapes = |tier: Isolation| {
            let probe = Path::new(&home).join(format!(".agg-notify-jail-probe.{}.{tier:?}", std::process::id()));
            let _ = std::fs::remove_file(&probe);
            let (tmp, cfg) = project(0, &format!("printf x > {}", probe.display()));
            let mut st = state(&cfg, tmp.path(), quiet_engine(), tier);
            cycle(&mut st, 1, Some("stalled hard"), false, None);
            let landed = probe.exists();
            let _ = std::fs::remove_file(&probe);
            landed
        };
        assert!(escapes(Isolation::None), "control: an unconfined delivery really does reach outside the project");
        assert!(!escapes(Isolation::Sandbox), "a `sandbox` step's notify.cmd must not reach outside the jail");
    }

    /// KILLS moving `NotifyOnStuck` ahead of `GateKeepRollback` in the on_gate registration. That
    /// order is the entire justification for the placement (§12.6) and nothing observed it: swapping
    /// the two entries compiles and passes the whole suite. Dispatched through the REAL registry
    /// entry, so what is under test is the shipped order, not a hand-assembled list.
    ///
    /// The scenario is a gate that discards the session (nothing staged ⇒ the `_` arm restores base
    /// truth and rebuilds `res`). Notifying before that pages a human about work agg is about to throw
    /// away AND consumes the debounce, so the genuine signal N sessions later is silenced too.
    #[test]
    fn the_gate_pings_about_kept_truth_never_about_work_it_just_discarded() {
        let (tmp, cfg) = project(0, r#"printf "%s\n" {{reason}} >> pings.txt"#);
        // an engine whose `notify_if` is true against RESTORED base truth …
        let mut stuck = Judge {
            name: "stuck".into(),
            kind: JudgeKind::Script { path: "true".into() },
            invariant: false,
            in_dod: false,
            state: Lifecycle::Pending,
            last_verdict: None,
            ever_met: false,
        };
        stuck.apply(Verdict {
            met: true,
            value: Some(90.0),
            max: Some(100.0),
            target: 85.0,
            rationale: "KEPT — base truth is flat".into(),
            evidence: vec![],
            error: None,
        });
        let eng = Engine::new(vec![stuck], "iterations > 999999".into(), None, Some("stuck.value >= 85".into())).unwrap();
        let mut st = state(&cfg, tmp.path(), eng, Isolation::None);
        // … while the pre-gate cycle carries a reason derived from the session about to be discarded.
        st.session = 1;
        st.scratch.get::<AGGScratch>().res = Some(CycleResult {
            notify: Some("DISCARDED — from the rolled-back session".into()),
            ..CycleResult::default()
        });

        let l = crate::registry::Lifecycle::with_hooks(&crate::core::config::Hooks::default(), tmp.path(), Isolation::None);
        for h in &l.on_gate {
            h.run(&mut st).expect("the gate ran");
        }

        // ONE equality pins three things: the discarded reason never left the process (the ordering),
        // a ping DID fire (so this is not vacuously green), and it carries the recomputed truth.
        assert_eq!(pings(tmp.path()), ["KEPT — base truth is flat"]);
    }
}

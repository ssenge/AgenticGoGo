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
    // specifies. Compare COLLAPSED forms — `notify_reason` normalises whitespace, so a raw
    // `blocked  OR  over_iterations` would otherwise never equal its own echo and get duplicated.
    let expr = halt_reason.unwrap_or("abort_if").to_string();
    let detail = ctx.eng.notify_reason(&expr);
    let collapsed = expr.split_whitespace().collect::<Vec<_>>().join(" ");
    let reason = if detail == collapsed { collapsed } else { format!("{collapsed} — {detail}") };
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
}

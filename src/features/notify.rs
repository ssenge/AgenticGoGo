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
        let Some(cfg) = ctx.cfg.sequence.notify.clone() else {
            return Ok(Flow::Continue);
        };
        if cfg.cmd.is_empty() {
            return Ok(Flow::Continue);
        }
        // `res` is still in scratch here — only `CheckRunStop` (on_session_end) takes it.
        let res = ctx.scratch.get::<AGGScratch>().res.as_ref().expect("an on_verify handler set scratch.res");
        let halting = res.halt;
        let notify = res.notify.clone();
        let halt_reason = res.halt_reason.clone();

        // A halt is terminal, so it pings once regardless of the debounce and does not consume it;
        // a live `notify_if` is rate-limited, because the whole point is not to nag a human awake.
        let (reason, kind) = if halting {
            // §8.5 says `{{reason}} = halt_reason`, which is the raw expression. Kept — but a bare
            // `blocked OR over_iterations` tells a human nothing, and "stop + notify" (§4) exists
            // precisely to tell them WHY. So append the winning judge's rationale when the
            // expression names one: `blocked OR over_iterations — BLOCKED: need the prod key`.
            // Ceiling-only expressions name no judge, so `notify_reason` echoes the expression and
            // the guard below leaves the message exactly as §8.5 specifies.
            let expr = halt_reason.unwrap_or_else(|| "abort_if".into());
            let detail = ctx.eng.notify_reason(&expr);
            let text = if detail == expr { expr } else { format!("{expr} — {detail}") };
            (text, "abort")
        } else {
            match notify {
                Some(r) if self.cooled_down(ctx, cfg.cooldown_sessions) => (r, "stuck"),
                _ => return Ok(Flow::Continue),
            }
        };
        if !halting {
            ctx.ext.get::<AGGState>().notify.last_notify_session = Some(ctx.session);
        }

        let step = ctx.cur_step.as_ref().map(|s| s.name.clone()).unwrap_or_default();
        let vars = [
            ("reason", reason.clone()),
            ("project", ctx.cfg.project.clone()),
            ("session", ctx.session.to_string()),
            ("step", step),
        ];
        let cmds: Vec<String> = cfg.cmd.iter().map(|c| template(c, &vars)).collect();

        // Same jail as the step that just ran (ISOLATION.md §14): `notify.cmd` lives in agg.yaml and
        // typically execs a project script — both inside the worker's writable cwd. `hooks::run`
        // SKIPS a command it cannot confine rather than running it unconfined.
        let tier = ctx.cur_step.as_ref().map(|s| s.isolation).unwrap_or(crate::isolation::Isolation::None);
        let dir = ctx.dir.to_path_buf();
        eprintln!("  [notify:{kind}] {reason}");
        crate::hooks::run("notify", &cmds, &dir, tier);

        // ALWAYS Continue — notify is pure signal. A halt still stops the run, but via
        // `CheckRunStop` reading `res.halt`, exactly as it did before this handler existed.
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "NotifyOnStuck"
    }
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

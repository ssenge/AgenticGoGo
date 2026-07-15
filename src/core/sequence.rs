//! The sequence statement grammar (§5.4) — ~a page of parsing over the terse line syntax the
//! owner designed, NOT nested YAML (rejected, §8).
//!
//! ```text
//! statement := repeat | branch | step_ref
//! step_ref  := NAME                          # a key in `steps:`
//! repeat    := NAME "x" INT                  # INT >= 1
//! branch    := "if" expr "then" NAME [ "else" NAME ]
//! expr      := the §5.3 (core::stop) grammar
//! ```
//! No nesting. A missing `else` falls through to the next statement. Keywords are case-insensitive.
//! `serde_yaml` parses the list of lines; this parses each line. Unknown step names / unresolvable
//! judges are a HARD ERROR at startup (checked by the loop against `steps:` and the judge library).

use anyhow::{bail, Result};

/// One parsed sequence statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    /// run a step once.
    Step(String),
    /// run a step INT times.
    Repeat(String, u32),
    /// run `then` when `cond` holds, else `els` when present, else fall through.
    Branch { cond: String, then: String, els: Option<String> },
}

impl Statement {
    /// Every step NAME this statement can dispatch — for the startup "unknown step" check.
    pub fn step_names(&self) -> Vec<&str> {
        match self {
            Statement::Step(n) | Statement::Repeat(n, _) => vec![n.as_str()],
            Statement::Branch { then, els, .. } => {
                let mut v = vec![then.as_str()];
                if let Some(e) = els {
                    v.push(e.as_str());
                }
                v
            }
        }
    }
    /// The branch condition expression, if this is a branch — a member of the run-set (§5.3).
    pub fn condition(&self) -> Option<&str> {
        match self {
            Statement::Branch { cond, .. } => Some(cond.as_str()),
            _ => None,
        }
    }
}

/// Parse the whole `sequence.steps` list. Refuses an EMPTY sequence (nothing could ever run).
pub fn parse(lines: &[String]) -> Result<Vec<Statement>> {
    if lines.is_empty() {
        bail!("`sequence.steps` is empty — a run needs at least one step");
    }
    lines.iter().map(|l| parse_statement(l)).collect()
}

/// Parse a single statement line.
pub fn parse_statement(raw: &str) -> Result<Statement> {
    let line = raw.trim();
    let toks: Vec<&str> = line.split_whitespace().collect();
    if toks.is_empty() {
        bail!("empty sequence statement");
    }

    // ── branch: `if <cond…> then NAME [else NAME]` ──
    if toks[0].eq_ignore_ascii_case("if") {
        let then_i = toks
            .iter()
            .position(|t| t.eq_ignore_ascii_case("then"))
            .ok_or_else(|| anyhow::anyhow!("branch `{line}` is missing `then`"))?;
        if then_i == 1 {
            bail!("branch `{line}` has an empty condition between `if` and `then`");
        }
        let cond = toks[1..then_i].join(" ");
        let else_rel = toks[then_i + 1..].iter().position(|t| t.eq_ignore_ascii_case("else"));
        let (then_toks, else_toks): (&[&str], Option<&[&str]>) = match else_rel {
            Some(rel) => {
                let ei = then_i + 1 + rel;
                (&toks[then_i + 1..ei], Some(&toks[ei + 1..]))
            }
            None => (&toks[then_i + 1..], None),
        };
        let then = single_name(then_toks, line, "then")?;
        let els = match else_toks {
            Some(t) => Some(single_name(t, line, "else")?),
            None => None,
        };
        return Ok(Statement::Branch { cond, then, els });
    }

    // ── repeat: `NAME x INT` (3 tokens) OR `NAME xINT` (2 tokens, the `worker x4` form) ──
    let repeat_count: Option<&str> = match toks.as_slice() {
        [_, kw, n] if kw.eq_ignore_ascii_case("x") => Some(n),
        [_, xn] if xn.len() > 1 && (xn.starts_with('x') || xn.starts_with('X')) => Some(&xn[1..]),
        _ => None,
    };
    if let Some(n) = repeat_count {
        let count: u32 = n
            .parse()
            .map_err(|_| anyhow::anyhow!("repeat count in `{line}` must be an integer, got `{n}`"))?;
        if count < 1 {
            bail!("repeat count in `{line}` must be >= 1");
        }
        return Ok(Statement::Repeat(toks[0].to_string(), count));
    }

    // ── step_ref: a bare NAME ──
    if toks.len() == 1 {
        return Ok(Statement::Step(toks[0].to_string()));
    }

    bail!(
        "unrecognized sequence statement `{line}` — expected `NAME`, `NAME x INT`, \
         or `if <cond> then NAME [else NAME]`"
    )
}

/// A branch target must be a SINGLE step name (no nesting, no list — §5.4).
fn single_name(toks: &[&str], line: &str, kw: &str) -> Result<String> {
    match toks {
        [n] => Ok(n.to_string()),
        [] => bail!("branch `{line}` has no step name after `{kw}`"),
        _ => bail!("branch `{line}`: `{kw}` target must be a single step name, not `{}`", toks.join(" ")),
    }
}

/// A cursor over the parsed statements that yields ONE step name per call, iterating the sequence
/// from the top forever (§5.5). Branch conditions are evaluated by the caller-supplied closure so
/// the cursor stays free of judge state.
pub struct Cursor {
    statements: Vec<Statement>,
    stmt_idx: usize,
    /// remaining reps of a Repeat currently in progress (0 = not inside one).
    repeat_left: u32,
    /// statements examined since the last yielded step — guards an all-false-branch lap.
    since_step: usize,
}

impl Cursor {
    pub fn new(statements: Vec<Statement>) -> Self {
        Cursor { statements, stmt_idx: 0, repeat_left: 0, since_step: 0 }
    }

    /// The next step to run. `eval` decides a branch's condition. Errors only if a full lap yields
    /// no step at all (a pathological all-`if`-false config; the loop already refuses an
    /// all-`skip_judges` sequence at startup).
    pub fn next_step(&mut self, eval: &mut impl FnMut(&str) -> Result<bool>) -> Result<String> {
        loop {
            if self.stmt_idx >= self.statements.len() {
                self.stmt_idx = 0; // wrap
            }
            if self.since_step > self.statements.len() {
                bail!("the sequence completed a full lap without dispatching any step — every `if` was false and there is no unconditional step");
            }
            let stmt = self.statements[self.stmt_idx].clone();
            match stmt {
                Statement::Step(name) => {
                    self.stmt_idx += 1;
                    self.since_step = 0;
                    return Ok(name);
                }
                Statement::Repeat(name, n) => {
                    if self.repeat_left == 0 {
                        self.repeat_left = n;
                    }
                    self.repeat_left -= 1;
                    if self.repeat_left == 0 {
                        self.stmt_idx += 1;
                    }
                    self.since_step = 0;
                    return Ok(name);
                }
                Statement::Branch { cond, then, els } => {
                    self.stmt_idx += 1;
                    self.since_step += 1;
                    if eval(&cond)? {
                        self.since_step = 0;
                        return Ok(then);
                    } else if let Some(e) = els {
                        self.since_step = 0;
                        return Ok(e);
                    }
                    // missing else ⇒ fall through to the next statement.
                }
            }
        }
    }
}

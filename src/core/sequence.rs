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

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------- parse_statement (§5.4) ----------------

    #[test]
    fn a_bare_name_is_a_step_ref() {
        assert_eq!(parse_statement("worker").unwrap(), Statement::Step("worker".into()));
        // surrounding whitespace is trimmed.
        assert_eq!(parse_statement("  reconsider  ").unwrap(), Statement::Step("reconsider".into()));
    }

    #[test]
    fn repeat_accepts_both_the_glued_and_spaced_forms() {
        // `worker x4` (2 tokens) and `worker x 4` (3 tokens) both mean "run worker 4 times".
        assert_eq!(parse_statement("worker x4").unwrap(), Statement::Repeat("worker".into(), 4));
        assert_eq!(parse_statement("worker x 4").unwrap(), Statement::Repeat("worker".into(), 4));
        // the `x` keyword is case-insensitive in both forms.
        assert_eq!(parse_statement("worker X4").unwrap(), Statement::Repeat("worker".into(), 4));
        assert_eq!(parse_statement("worker X 4").unwrap(), Statement::Repeat("worker".into(), 4));
    }

    #[test]
    fn a_repeat_count_must_be_a_positive_integer() {
        assert!(parse_statement("worker x0").unwrap_err().to_string().contains(">= 1"));
        let e = parse_statement("worker x abc").unwrap_err().to_string();
        assert!(e.contains("must be an integer"), "got: {e}");
    }

    #[test]
    fn a_branch_parses_condition_then_and_optional_else() {
        assert_eq!(
            parse_statement("if stalled then reconsider").unwrap(),
            Statement::Branch { cond: "stalled".into(), then: "reconsider".into(), els: None }
        );
        assert_eq!(
            parse_statement("if stalled then reconsider else worker").unwrap(),
            Statement::Branch {
                cond: "stalled".into(),
                then: "reconsider".into(),
                els: Some("worker".into()),
            }
        );
    }

    #[test]
    fn a_branch_condition_may_be_a_multi_token_expression() {
        // the condition is the whole §5.3 grammar — it survives spaces and operators.
        assert_eq!(
            parse_statement("if coverage.value >= 80 then ship").unwrap(),
            Statement::Branch { cond: "coverage.value >= 80".into(), then: "ship".into(), els: None }
        );
    }

    #[test]
    fn branch_keywords_are_case_insensitive() {
        assert_eq!(
            parse_statement("IF stalled THEN reconsider ELSE worker").unwrap(),
            Statement::Branch {
                cond: "stalled".into(),
                then: "reconsider".into(),
                els: Some("worker".into()),
            }
        );
    }

    #[test]
    fn a_branch_target_must_be_a_single_step_name() {
        // no nesting, no list (§5.4): more than one token after `then`/`else` is an error.
        assert!(parse_statement("if x then a b").unwrap_err().to_string().contains("single step name"));
        assert!(parse_statement("if x then a else b c").unwrap_err().to_string().contains("single step name"));
    }

    #[test]
    fn a_branch_without_then_or_condition_is_an_error() {
        assert!(parse_statement("if stalled reconsider").unwrap_err().to_string().contains("missing `then`"));
        assert!(parse_statement("if then reconsider").unwrap_err().to_string().contains("empty condition"));
    }

    #[test]
    fn parse_refuses_an_empty_sequence() {
        assert!(parse(&[]).unwrap_err().to_string().contains("at least one step"));
    }

    #[test]
    fn step_names_and_condition_expose_the_run_set_pieces() {
        let b = parse_statement("if stalled then reconsider else worker").unwrap();
        assert_eq!(b.step_names(), vec!["reconsider", "worker"]);
        assert_eq!(b.condition(), Some("stalled"));
        let s = parse_statement("worker x3").unwrap();
        assert_eq!(s.step_names(), vec!["worker"]);
        assert_eq!(s.condition(), None);
    }

    // ---------------- Cursor (§5.5) ----------------

    /// A cursor whose branches never fire — the common all-unconditional case.
    fn no_branches(c: &mut Cursor, n: usize) -> Vec<String> {
        (0..n).map(|_| c.next_step(&mut |_| Ok(false)).unwrap()).collect()
    }

    #[test]
    fn a_repeat_yields_the_step_n_times_then_moves_on() {
        let mut c = Cursor::new(vec![Statement::Repeat("w".into(), 3), Statement::Step("r".into())]);
        // w w w r, then it wraps: w w w r …
        assert_eq!(no_branches(&mut c, 5), vec!["w", "w", "w", "r", "w"]);
    }

    #[test]
    fn a_branch_takes_then_when_true_and_falls_through_when_false() {
        // `w`, then `if cond then r` (no else). When cond is false the branch falls through and the
        // lap wraps back to `w`; when true it yields `r`.
        let stmts = vec![
            Statement::Step("w".into()),
            Statement::Branch { cond: "cond".into(), then: "r".into(), els: None },
        ];
        let mut c = Cursor::new(stmts.clone());
        // cond=false forever → only `w` ever runs.
        assert_eq!(no_branches(&mut c, 3), vec!["w", "w", "w"]);

        let mut c2 = Cursor::new(stmts);
        // `w`, then the branch (its FIRST encounter) is true → `r`, then the lap wraps to `w`.
        let mut evals = [true].into_iter();
        let got: Vec<String> = (0..3)
            .map(|_| c2.next_step(&mut |_| Ok(evals.next().unwrap_or(false))).unwrap())
            .collect();
        assert_eq!(got, vec!["w", "r", "w"]);
    }

    #[test]
    fn a_missing_else_falls_through_to_the_next_statement() {
        // `if cond then a` (false) must NOT yield `a`; it falls through to the unconditional `b`.
        let mut c = Cursor::new(vec![
            Statement::Branch { cond: "cond".into(), then: "a".into(), els: None },
            Statement::Step("b".into()),
        ]);
        assert_eq!(no_branches(&mut c, 2), vec!["b", "b"]);
    }

    #[test]
    fn an_else_is_taken_when_the_condition_is_false() {
        let mut c = Cursor::new(vec![Statement::Branch {
            cond: "cond".into(),
            then: "a".into(),
            els: Some("b".into()),
        }]);
        assert_eq!(no_branches(&mut c, 2), vec!["b", "b"]);
    }

    #[test]
    fn an_all_false_branch_lap_errors_instead_of_spinning_forever() {
        // a pathological config of ONLY if-branches, all false, would loop with no step to dispatch.
        // The cursor detects the full no-yield lap and errors rather than hang.
        let mut c = Cursor::new(vec![
            Statement::Branch { cond: "x".into(), then: "a".into(), els: None },
            Statement::Branch { cond: "y".into(), then: "b".into(), els: None },
        ]);
        let err = c.next_step(&mut |_| Ok(false)).unwrap_err().to_string();
        assert!(err.contains("without dispatching any step"), "got: {err}");
    }
}

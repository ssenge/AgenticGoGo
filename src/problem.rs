//! One config problem: WHERE (a dotted path into `agg.yaml`) and WHAT (the message startup gives).
//!
//! Startup validation used to stop at the first error, which turns fixing a generated config into
//! a loop of "run, read one error, fix, run again". The validators now COLLECT [`Problem`]s, so
//! `agg check` can list every one at once and an orchestrator can point at the offending key; the
//! startup paths (`agg run` / `plan` / `doctor`) render the same list into their one error.

use serde::Serialize;

/// A single problem with a config, located by a dotted path.
///
/// `path` spells keys with dots and list entries with `[i]` — `steps.worker.isolation`,
/// `sequence.steps[1].until`. The empty string means the file as a whole (it is missing, or it is
/// not YAML at all). `message` is exactly the text startup would print for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub path: String,
    pub message: String,
}

impl Problem {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Problem { path: path.into(), message: message.into() }
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

/// Render problems as ONE startup error.
///
/// A single problem renders as its bare message, byte-for-byte what the old first-error-wins
/// validation printed, so every existing message (and every test that greps for one) survives.
/// Several are numbered and located, so the operator can fix them all in one pass.
pub fn render(problems: &[Problem]) -> String {
    match problems {
        [] => String::new(),
        [one] => one.message.clone(),
        many => {
            let mut s = format!("{} problems in the config:", many.len());
            for p in many {
                s.push_str(&format!("\n\n  ✗ {p}"));
            }
            s
        }
    }
}

/// Append `p` unless an identical problem is already listed — two validators can reach the same
/// fault (an unknown agent is both "not installable" and "has no capabilities").
pub fn push_unique(out: &mut Vec<Problem>, p: Problem) {
    if !out.contains(&p) {
        out.push(p);
    }
}

/// `parent.key`, or just `key` at the root — the one place the dotted spelling is built.
pub fn join(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_problem_renders_as_its_bare_message() {
        let p = Problem::new("sequence.done_if", "no judge named `x`");
        assert_eq!(render(&[p]), "no judge named `x`", "a lone problem must keep the old startup text exactly");
    }

    #[test]
    fn several_problems_render_counted_and_located() {
        let ps = [Problem::new("a.b", "first"), Problem::new("", "second")];
        let s = render(&ps);
        assert!(s.starts_with("2 problems"), "{s}");
        assert!(s.contains("✗ a.b: first"), "{s}");
        assert!(s.contains("✗ second"), "a root problem carries no path prefix: {s}");
        assert_eq!(render(&[]), "");
    }

    #[test]
    fn push_unique_drops_exact_duplicates_only() {
        let mut v = vec![Problem::new("a", "m")];
        push_unique(&mut v, Problem::new("a", "m"));
        push_unique(&mut v, Problem::new("b", "m"));
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn join_builds_dotted_paths() {
        assert_eq!(join("", "steps"), "steps");
        assert_eq!(join("steps", "worker"), "steps.worker");
    }
}

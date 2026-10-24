//! A workflow's variables: `{{ name }}` in a step's prompt file (the role prompt's too) and in an LLM
//! judge's rubric, its `inputs:` frontmatter included, is filled in from the map the driver set
//! ([`Agg::vars`](crate::driver::Agg::vars)). Only files are templates: inline text a driver builds may
//! carry a project's content (a doc with a template language of its own), and code has `format!`. A
//! `{{ name }}` the map lacks is an error: a worker or a judge briefed with a hole does the wrong work.
//! Without variables nothing is touched.

use std::collections::BTreeMap;
use std::sync::RwLock;

// ponytail: process-wide, one workflow per process (the driver's `run`); per-Agg maps if a process ever
// drives two workflows with different variables
static VARS: RwLock<BTreeMap<String, String>> = RwLock::new(BTreeMap::new());

/// Sets the variables every prompt and rubric of this process is rendered with.
pub fn set(vars: BTreeMap<String, String>) {
    *VARS.write().unwrap_or_else(|e| e.into_inner()) = vars;
}

/// `text` rendered with the variables set; unchanged when none are.
pub fn apply(text: &str) -> Result<String, String> {
    let vars = VARS.read().unwrap_or_else(|e| e.into_inner());
    if vars.is_empty() {
        return Ok(text.to_string());
    }
    render(text, &vars)
}

/// `text` with every `{{ name }}` (a name of letters, digits and `_`, spaces inside the braces optional)
/// filled in from `vars`; a name `vars` lacks is an error. Other `{{` (a template language of the
/// project's own) stays as it is.
pub fn render(text: &str, vars: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}").map(|end| (end, after[..end].trim())) {
            Some((end, name)) if is_name(name) => {
                out.push_str(vars.get(name).ok_or_else(|| format!("unknown variable `{{{{ {name} }}}}`"))?);
                rest = &after[end + 2..];
            }
            _ => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Whether `s` is a variable's name: letters, digits and `_`, not starting with a digit.
fn is_name(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> BTreeMap<String, String> {
        BTreeMap::from([("legacy".into(), "old".into()), ("out_dir".into(), "new".into())])
    }

    /// A name is filled in, with or without spaces; an unknown one is refused; other braces stay.
    #[test]
    fn names_are_filled_in_and_unknown_ones_refused() {
        assert_eq!(render("`{{ legacy }}/src` and {{out_dir}}/", &vars()).unwrap(), "`old/src` and new/");
        assert_eq!(render("{{ legasy }}", &vars()).unwrap_err(), "unknown variable `{{ legasy }}`");
        let own = "${{ github.sha }} {{ 1x }} {{ a b }} {{ unclosed";
        assert_eq!(render(own, &vars()).unwrap(), own, "not a variable's name: left as it is");
    }

    /// The variables are process-wide: the tests that set them take turns.
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Without variables nothing is rendered: a project's own `{{ name }}` is no error then.
    #[test]
    fn without_variables_nothing_is_touched() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        set(BTreeMap::new());
        assert_eq!(apply("{{ anything }}").unwrap(), "{{ anything }}");
    }

    /// A prompt file and a rubric (its `inputs:` and its text) are rendered where agg reads them; an
    /// unknown name in a prompt file fails reading it.
    #[test]
    fn prompt_files_and_rubrics_are_rendered_where_they_are_read() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("w.md"), "change {{ out_dir }}/ only\n").unwrap();
        std::fs::write(dir.path().join("bad.md"), "see {{ nowhere }}\n").unwrap();
        std::fs::write(dir.path().join("r.md"), "---\ninputs: [\"{{ legacy }}/a.txt\"]\n---\nJudge {{ legacy }}.\n").unwrap();
        set(vars());
        let step = crate::driver::Step::new("w").prompt_file(dir.path().join("w.md").display().to_string());
        assert_eq!(crate::workflow::task(&step).unwrap(), "change new/ only\n");
        let bad = crate::driver::Step::new("b").prompt_file(dir.path().join("bad.md").display().to_string());
        assert!(crate::workflow::task(&bad).unwrap_err().to_string().contains("unknown variable `{{ nowhere }}`"));
        let crate::core::model::JudgeKind::Llm { inputs, .. } = crate::core::judges::kind_for(&dir.path().join("r.md"))
        else { panic!("an .md is an LLM judge") };
        assert_eq!(inputs, ["old/a.txt"]);
        set(BTreeMap::new());
    }
}

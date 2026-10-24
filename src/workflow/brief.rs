//! A step's brief, put together: its own task with sections after it, or before it for what is new in a
//! session that continues the step's own (a review, a note, the changes since), which a worker reads first
//! where it takes the rest for known and skims it.

use anyhow::anyhow;

use crate::driver::{Fatal, Step};

/// The step's own task with `section` added at its end.
pub fn with_section(step: &Step, section: &str) -> Result<Step, Fatal> {
    if section.trim().is_empty() {
        return Ok(step.clone());
    }
    Ok(step.clone().prompt(format!("{}\n\n{section}\n", task(step)?)))
}

/// The step's own task with `section` before it.
pub fn with_lead(step: &Step, section: &str) -> Result<Step, Fatal> {
    Ok(step.clone().prompt(format!("{section}\n\n{}\n", task(step)?)))
}

/// The step's brief as it stands: its prompt, or its prompt file read now.
pub fn task(step: &Step) -> Result<String, Fatal> {
    match &step.prompt_file {
        Some(file) => {
            let text = std::fs::read_to_string(file).map_err(|e| anyhow!("reading {file}: {e}"))?;
            Ok(crate::core::vars::apply(&text).map_err(|e| anyhow!("{file}: {e}"))?)
        }
        None => Ok(step.prompt.clone().unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A section goes after the task, a lead before it; a task in a file is read; an empty section adds nothing.
    #[test]
    fn sections_go_after_and_leads_before_the_task() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("task.md");
        std::fs::write(&file, "Do it.").unwrap();
        let step = Step::new("1 Do").prompt_file(file.display().to_string());
        let both = with_lead(&with_section(&step, "## After").unwrap(), "## Before").unwrap();
        assert_eq!(both.prompt.as_deref(), Some("## Before\n\nDo it.\n\n## After\n\n"));
        assert_eq!(with_section(&step, " ").unwrap().prompt_file, step.prompt_file);
    }
}

//! The `template` judge (A-38 3.9): a document filled in from a template. Met when the document has the template's
//! headings in the template's order (a heading's level and text; the template's placeholders in a heading match any
//! text), no section is empty, and nothing of the template is left: its placeholders, an HTML comment, an instruction
//! line (a `> Instruction:` line, or a blockquote line the template has). The rationale names the first three problems
//! with their line. A placeholder is `<…>` unless the declaration's `placeholder` (a regex) says otherwise.

use std::collections::BTreeSet;
use std::path::Path;

use regex::Regex;

use crate::core::model::Verdict;

/// The placeholder pattern of a declaration that names none.
pub const PLACEHOLDER: &str = r"<[^<>\n]{1,120}>";

/// One heading of a document: its level, text and 1-based line.
#[derive(Debug, Clone)]
struct Heading {
    level: usize,
    text: String,
    line: usize,
}

/// The lines of `text` outside fenced code and HTML comments, as (line number, line with its inline code spans blanked
/// out); and every line an HTML comment starts on outside fenced code.
fn scan(text: &str) -> (Vec<(usize, String)>, Vec<usize>) {
    let span = Regex::new(r"`[^`\n]*`").expect("a fixed pattern");
    let comment = Regex::new(r"(?s)<!--.*?-->").expect("a fixed pattern");
    // a comment's lines stay (so the line numbers hold) but hold nothing
    let bare = comment.replace_all(text, |c: &regex::Captures| c[0].chars().filter(|ch| *ch == '\n').collect::<String>());
    let (mut fence, mut out, mut comments) = (false, vec![], vec![]);
    for (i, (line, original)) in bare.lines().zip(text.lines()).enumerate() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fence = !fence;
        } else if !fence {
            if original.contains("<!--") {
                comments.push(i + 1);
            }
            out.push((i + 1, span.replace_all(line, "").to_string()));
        }
    }
    (out, comments)
}

fn prose(text: &str) -> Vec<(usize, String)> {
    scan(text).0
}

fn headings(text: &str) -> Vec<Heading> {
    prose(text).into_iter().filter_map(|(line, l)| {
        let level = l.chars().take_while(|c| *c == '#').count();
        (level > 0 && l[level..].starts_with(' ')).then(|| Heading { level, text: l[level..].trim().to_string(), line })
    }).collect()
}

/// A heading of the template as a pattern: its text with each placeholder matching any text.
fn pattern(heading: &str, placeholder: &Regex) -> Regex {
    let mut out = String::from("^");
    let mut last = 0;
    for m in placeholder.find_iter(heading) {
        out.push_str(&regex::escape(&heading[last..m.start()]));
        out.push_str(".+");
        last = m.end();
    }
    out.push_str(&regex::escape(&heading[last..]));
    out.push('$');
    Regex::new(&out).expect("an escaped pattern")
}

/// The problems of `doc` against `template` (each with its line), in the order they come.
pub fn problems(doc: &str, template: &str, placeholder: &str) -> Result<Vec<String>, String> {
    let placeholder = Regex::new(placeholder).map_err(|e| format!("placeholder: {e}"))?;
    let (have, wanted) = (headings(doc), headings(template));
    let mut out = vec![];
    let mut cursor = 0;
    let mut found: Vec<(usize, &Heading)> = vec![];
    for want in &wanted {
        let re = pattern(&want.text, &placeholder);
        let at = |from: usize| have.iter().enumerate().skip(from).find(|(_, h)| h.level == want.level && re.is_match(&h.text)).map(|(i, _)| i);
        match (at(cursor), at(0)) {
            (Some(i), _) => {
                found.push((i, &have[i]));
                cursor = i + 1;
            }
            (None, Some(i)) => out.push(format!("line {}: `{}` is out of the template's order", have[i].line, have[i].text)),
            (None, None) => out.push(format!("missing section `{}`", want.text.trim())),
        }
    }
    let lines: Vec<&str> = doc.lines().collect();
    for (i, h) in found {
        let end = have[i + 1..].iter().find(|n| n.level <= h.level).map_or(lines.len(), |n| n.line - 1);
        let empty = lines[h.line..end.max(h.line)].iter().all(|l| l.trim().is_empty() || l.trim().starts_with("<!--"));
        if empty {
            out.push(format!("line {}: section `{}` is empty", h.line, h.text));
        }
    }
    out.extend(leftovers(doc, template, &placeholder));
    out.sort_by_key(|p| p.strip_prefix("line ").and_then(|r| r.split(':').next()?.parse::<usize>().ok()).unwrap_or(0));
    Ok(out)
}

/// What of the template is still in the document: its placeholders, an HTML comment, its instruction lines.
fn leftovers(doc: &str, template: &str, placeholder: &Regex) -> Vec<String> {
    let own: BTreeSet<String> = prose(template).iter().flat_map(|(_, l)| placeholder.find_iter(l).map(|m| m.as_str().to_string())).collect();
    let instructions: BTreeSet<&str> = template.lines().map(str::trim).filter(|l| l.starts_with('>')).collect();
    let mut out = vec![];
    out.extend(scan(doc).1.into_iter().map(|line| format!("line {line}: the template's comment is left")));
    for (line, text) in prose(doc) {
        out.extend(placeholder.find_iter(&text).filter(|m| own.contains(m.as_str())).map(|m| format!("line {line}: placeholder left: {}", m.as_str())));
        let t = text.trim();
        if t.starts_with("> Instruction:") || instructions.contains(t) {
            out.push(format!("line {line}: instruction left: {}", t.chars().take(60).collect::<String>()));
        }
    }
    out
}

/// The verdict's rationale: the first three problems, and how many more.
pub fn rationale(problems: &[String], doc: &str, sections: usize) -> String {
    if problems.is_empty() {
        return format!("{doc} follows the template ({sections} sections)");
    }
    let more = problems.len().saturating_sub(3);
    format!("{doc}: {}{}", problems.iter().take(3).cloned().collect::<Vec<_>>().join("; "), if more > 0 { format!(" (and {more} more)") } else { String::new() })
}

/// The judge's verdict on `doc` (relative to `project`) against the template file.
pub fn verdict(project: &Path, doc: &str, template: &Path, placeholder: &str) -> Verdict {
    let Ok(text) = std::fs::read_to_string(project.join(doc)) else {
        return Verdict::binary(false).with_rationale(format!("{doc} is missing"));
    };
    let Ok(wanted) = std::fs::read_to_string(template) else {
        return Verdict::binary(false).with_rationale(format!("the template {} can't be read", template.display()));
    };
    match problems(&text, &wanted, placeholder) {
        Ok(found) => Verdict::binary(found.is_empty()).with_rationale(rationale(&found, doc, sections(&wanted))),
        Err(e) => Verdict::binary(false).with_rationale(e),
    }
}

/// The number of headings the template asks for.
pub fn sections(template: &str) -> usize {
    headings(template).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = "<!-- the template: remove this -->\n# Report: <app name>\n\n## 1. Summary\n\n> Instruction: five sentences\n\n## 2. <section>\n\nText with `<code>` and <value>.\n\n### Detail\n";

    /// A document that fills the template in has none.
    #[test]
    fn a_filled_in_document_follows_its_template() {
        let doc = "# Report: Orders\n\n## 1. Summary\n\nIt moved.\n\n## 2. Data\n\nThe `<code>` stays.\n\n### Detail\n\nMore.\n";
        assert_eq!(problems(doc, TEMPLATE, PLACEHOLDER).unwrap(), Vec::<String>::new());
        assert!(rationale(&[], "doc.md", sections(TEMPLATE)).contains("follows the template (4 sections)"));
    }

    /// Missing, out-of-order and empty sections, and what the template leaves behind, each with its line.
    #[test]
    fn problems_name_the_section_and_the_line() {
        let doc = "<!-- the template: remove this -->\n# Report: <app name>\n\n## 2. Data\n\nx <value>\n\n## 1. Summary\n\n> Instruction: five sentences\n\n";
        let found = problems(doc, TEMPLATE, PLACEHOLDER).unwrap();
        let all = found.join("\n");
        assert!(all.contains("line 1: the template's comment is left"), "{all}");
        assert!(all.contains("line 2: placeholder left: <app name>") && all.contains("line 6: placeholder left: <value>"), "{all}");
        assert!(all.contains("line 10: instruction left: > Instruction: five sentences"), "{all}");
        assert!(all.contains("missing section `Detail`"), "{all}");
        assert!(all.contains("line 4: `2. Data` is out of the template's order"), "{all}");
        assert!(!all.contains("is empty"), "an instruction line is content (and a leftover): {all}");
        let said = rationale(&found, "doc.md", 4);
        assert!(said.starts_with("doc.md: ") && said.contains("(and "), "{said}");
    }

    /// The verdict reads the document of the project and the template file: missing either is unmet, a placeholder pattern
    /// that doesn't compile too.
    #[test]
    fn the_verdict_reads_the_document_and_the_template() {
        let dir = tempfile::tempdir().unwrap();
        let (template, doc) = (dir.path().join("form.md"), dir.path().join("report.md"));
        assert!(verdict(dir.path(), "report.md", &template, PLACEHOLDER).rationale().ends_with("is missing"));
        std::fs::write(&doc, "# Report\n\n## One\n\n<fill in>\n").unwrap();
        assert!(verdict(dir.path(), "report.md", &template, PLACEHOLDER).rationale().contains("can't be read"));
        std::fs::write(&template, "# Report\n\n## One\n\n<fill in>\n").unwrap();
        let bad = verdict(dir.path(), "report.md", &template, PLACEHOLDER);
        assert!(!bad.met() && bad.rationale().contains("placeholder left: <fill in>"), "{}", bad.rationale());
        std::fs::write(&doc, "# Report\n\n## One\n\nDone.\n").unwrap();
        assert!(verdict(dir.path(), "report.md", &template, PLACEHOLDER).met());
        assert!(!verdict(dir.path(), "report.md", &template, "(").met());
    }

    /// A blockquote line the template has is an instruction too; a parent section with only subsections isn't empty;
    /// code is no prose; a declaration's own placeholder pattern replaces `<…>`.
    #[test]
    fn instructions_parents_code_and_own_placeholders() {
        let template = "# T\n\n## A\n\n> Say what was looked at.\n\n### B {name}\n\n{value}\n";
        let doc = "# T\n\n## A\n\n> Say what was looked at.\n\n### B Orders\n\n```\n{value}\n```\nok\n";
        let found = problems(doc, template, r"\{[^{}\n]{1,120}\}").unwrap();
        assert_eq!(found, ["line 5: instruction left: > Say what was looked at."], "{found:?}");
        assert!(problems("# T", template, "(").unwrap_err().starts_with("placeholder: "));
    }
}

//! The `{table}` of a quality guide (A-38 3.8): the score definition's parts (tool, metric, good and bad values, weight,
//! hard limits) from `swe-metrics score --describe`, the definition the `score` judge scores by, as Markdown.

use std::process::Command;

use serde_json::Value;

/// The guide with `{table}` filled in from the installed swe-metrics's description of its definition; `{table}` becomes
/// a line saying why when swe-metrics can't describe it.
pub fn fill(guide: &str, definition: &str, version: &str) -> String {
    if !guide.contains("{table}") {
        return guide.to_string();
    }
    let table = describe(definition).map_or_else(|why| format!("(the definition's parts aren't available: {why})"), |d| table(&d, version));
    guide.replace("{table}", &table)
}

/// `swe-metrics score --describe --definition <name>` as JSON.
fn describe(definition: &str) -> Result<Value, String> {
    let out = Command::new("swe-metrics").args(["score", "--describe", "--definition", definition]).output()
        .map_err(|e| format!("swe-metrics: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

/// A number as a short text (`5`, `0.5`).
fn n(v: &Value) -> String {
    v.as_f64().map_or_else(|| "-".into(), |f| if f.fract() == 0.0 { format!("{f:.0}") } else { format!("{f}") })
}

/// The hard limits and the parts of a described definition as Markdown; a note when its version isn't the one asked for.
pub fn table(doc: &Value, version: &str) -> String {
    let mut out = String::new();
    let got = doc["version"].as_str().unwrap_or_default();
    if got != version {
        out.push_str(&format!("(the installed swe-metrics describes version {got}, the process asks for {version})\n\n"));
    }
    out.push_str("**Hard limits.** While one is broken, the score is capped, whatever else is good:\n\n");
    for g in doc["gates"].as_array().into_iter().flatten() {
        let limit = match (g["max"].as_f64(), g["min"].as_f64()) {
            (Some(max), _) => format!("at most {}", n(&g["max"]).replace("-", &max.to_string())),
            (_, Some(_)) => format!("at least {}", n(&g["min"])),
            _ => "met".into(),
        };
        out.push_str(&format!("- `{}`: {limit} (score at most {})\n", g["what"].as_str().unwrap_or_default(), n(&g["cap"])));
    }
    out.push_str("\n**Parts.** Each part scores 100 at its good value or better, 0 at its bad value or worse, linear between; the score is the weighted mean of the parts that apply.\n\n| Part | Tool | Good | Bad | Weight |\n|---|---|---|---|---|\n");
    for c in doc["components"].as_array().into_iter().flatten() {
        let tool = c["tool"].as_str().filter(|t| !t.is_empty()).unwrap_or("any");
        out.push_str(&format!("| `{}` | {tool} | {} | {} | {} |\n", c["what"].as_str().unwrap_or_default(), n(&c["good"]), n(&c["bad"]), n(&c["weight"])));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc() -> Value {
        json!({"name": "swe-default", "version": "0.4",
               "gates": [{"what": "secrets.findings", "tool": "", "max": 0.0, "min": null, "cap": 40.0},
                         {"what": "coverage.line_percent", "tool": "", "max": null, "min": 10.0, "cap": 60.0}],
               "components": [{"what": "complexity.cyclomatic p95", "tool": "lizard", "good": 5.0, "bad": 10.0, "weight": 3.0, "required": false},
                              {"what": "doc.coverage", "tool": "", "good": 80.0, "bad": 30.0, "weight": 0.5, "required": false}]})
    }

    /// The limits and the parts come from the definition; the version is named when it isn't the one asked for.
    #[test]
    fn the_table_lists_the_gates_and_the_parts() {
        let t = table(&doc(), "0.4");
        assert!(t.contains("- `secrets.findings`: at most 0 (score at most 40)") && t.contains("`coverage.line_percent`: at least 10"), "{t}");
        assert!(t.contains("| `complexity.cyclomatic p95` | lizard | 5 | 10 | 3 |") && t.contains("| `doc.coverage` | any | 80 | 30 | 0.5 |"), "{t}");
        assert!(!t.contains("installed swe-metrics describes"));
        assert!(table(&doc(), "0.3").starts_with("(the installed swe-metrics describes version 0.4, the process asks for 0.3)"));
    }

    /// Only a guide that asks for `{table}` runs swe-metrics.
    #[test]
    fn a_guide_without_the_placeholder_is_left_alone() {
        assert_eq!(fill("plain {min}", "swe-default", "0.4"), "plain {min}");
    }
}

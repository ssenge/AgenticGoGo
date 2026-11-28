//! What a run remembers of the lanes it joined: `rlt/private/lanes.jsonl`, one line per joined lane. A resumed run skips
//! them (their changes are in its branch); a fresh run of the same `each` starts over.

use std::path::{Path, PathBuf};

fn record_file(project: &Path) -> PathBuf {
    crate::paths::private_dir(project).join("lanes.jsonl")
}

/// Notes that item `key` of `each` was joined at `tip`.
pub fn note(project: &Path, each: &str, key: &str, tip: &str) {
    use std::io::Write;
    let line = serde_json::json!({"each": each, "key": key, "tip": tip, "at": crate::util::now_epoch()});
    let _ = std::fs::create_dir_all(crate::paths::private_dir(project));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(record_file(project)) {
        let _ = writeln!(f, "{line}");
    }
}

/// The keys of `each` that an earlier process of this run joined.
pub fn recorded(project: &Path, each: &str) -> Vec<String> {
    std::fs::read_to_string(record_file(project)).unwrap_or_default().lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["each"] == each).filter_map(|v| v["key"].as_str().map(String::from)).collect()
}

/// A fresh run of the `each` starts over: its record goes. Nothing is recorded for it afterwards, so none is joined.
pub fn forget(project: &Path, each: &str) -> Vec<String> {
    let kept: Vec<String> = std::fs::read_to_string(record_file(project)).unwrap_or_default().lines()
        .filter(|l| serde_json::from_str::<serde_json::Value>(l).map_or(true, |v| v["each"] != each)).map(String::from).collect();
    let _ = std::fs::write(record_file(project), kept.join("\n") + if kept.is_empty() { "" } else { "\n" });
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A joined lane is recorded by its `each` and key; a fresh run of that `each` forgets its lanes and no other's.
    #[test]
    fn joined_lanes_are_recorded_and_a_fresh_run_forgets_only_its_own_each() {
        let d = tempfile::tempdir().unwrap();
        assert!(recorded(d.path(), "units").is_empty());
        note(d.path(), "units", "a", "t1");
        note(d.path(), "units", "b", "t2");
        note(d.path(), "docs", "x", "t3");
        assert_eq!(recorded(d.path(), "units"), ["a", "b"]);
        assert!(forget(d.path(), "units").is_empty());
        assert!(recorded(d.path(), "units").is_empty());
        assert_eq!(recorded(d.path(), "docs"), ["x"]);
    }
}

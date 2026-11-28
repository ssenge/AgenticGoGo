//! Parallel lanes: the items of an `each` or the children of a `kind: parallel` step run at once, each in a linked worktree
//! of its own, on a branch of its own and a thread of its own, and are joined into the run's branch in item order.
//!
//! * [`worktree`]: the worktrees and the names of a lane's branches;
//! * [`services`]: what lanes share (the stop flag, the rate-limit gate, the run's spend and its ceilings, the views);
//! * [`schedule`]: launching up to `max` lanes, supervising them, forwarding the operator's commands to them;
//! * [`join`]: merging a finished lane's branch into the run's, in item order.

pub mod forward;
pub mod join;
pub mod record;
pub mod schedule;
pub mod services;
pub mod target;
pub mod worktree;

pub use services::{LaneState, LaneView, Shared};

/// Where a run publishes its lanes' views, for `rlt status` and the web view: `rlt/private/lanes.json`.
pub fn views_file(project: &std::path::Path) -> std::path::PathBuf {
    crate::paths::private_dir(project).join("lanes.json")
}

/// Publishes the lanes' views (written whole and moved into place, so a reader never sees half of them).
pub fn publish(project: &std::path::Path, views: &[LaneView]) {
    let file = views_file(project);
    let temp = file.with_extension("json.tmp");
    let _ = std::fs::create_dir_all(crate::paths::private_dir(project));
    if serde_json::to_string_pretty(views).ok().is_some_and(|t| std::fs::write(&temp, t).is_ok()) {
        let _ = std::fs::rename(&temp, &file);
    }
}

/// The lanes' views the run published; none when it has no lanes at work.
pub fn read(project: &std::path::Path) -> Vec<LaneView> {
    std::fs::read_to_string(views_file(project)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// The views as a table: key, state, step, tokens, time.
pub fn table(views: &[LaneView], now: u64) -> String {
    let mut out = format!("lanes  {} ({} running)\n", views.len(), views.iter().filter(|v| v.state == LaneState::Running).count());
    for v in views {
        let state = if v.waiting { "waiting".to_string() } else { format!("{:?}", v.state).to_lowercase() };
        let secs = v.started.map(|s| v.ended.unwrap_or(now).saturating_sub(s)).unwrap_or(0);
        out.push_str(&format!("  {:<14} {:<9} {:<24} {:>8} tok  {}\n", v.key, state, v.step, v.tokens, crate::util::fmt_dur(secs)));
    }
    out
}

/// An item's key as one component of a ref or a folder name: every character that isn't a letter, a digit, `_` or `-`
/// becomes `-`.
pub fn safe(key: &str) -> String {
    key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect()
}

/// The keys whose sanitised names are equal to an earlier key's: two lanes must not share a folder or a branch.
pub fn collisions(keys: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashMap::new();
    let mut out = vec![];
    for k in keys {
        if let Some(first) = seen.insert(safe(k), k) {
            out.push(format!("`{first}` and `{k}` both become `{}`", safe(k)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Views are published whole, read back, and shown as a table.
    #[test]
    fn views_are_published_read_back_and_shown_as_a_table() {
        let d = tempfile::tempdir().unwrap();
        assert!(read(d.path()).is_empty());
        let views = vec![
            LaneView { state: LaneState::Running, step: "build".into(), tokens: 120, started: Some(100), ..LaneView::queued("a") },
            LaneView { state: LaneState::Done, started: Some(100), ended: Some(160), ..LaneView::queued("b") },
        ];
        publish(d.path(), &views);
        assert_eq!(read(d.path()), views);
        let text = table(&views, 130);
        assert!(text.starts_with("lanes  2 (1 running)") && text.contains("a ") && text.contains("running") && text.contains("done"), "{text}");
    }

    #[test]
    fn a_key_becomes_a_safe_name_and_equal_names_are_reported() {
        assert_eq!(safe("billing/api v2"), "billing-api-v2");
        assert_eq!(safe("U1_a-b"), "U1_a-b");
        let keys: Vec<String> = ["a b", "a-b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(collisions(&keys), ["`a b` and `a-b` both become `a-b`"]);
        assert!(collisions(&["x".to_string(), "y".to_string()]).is_empty());
    }
}

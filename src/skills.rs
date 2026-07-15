//! `agg skills install` — put the `/agg:*` skills where the chosen agent will actually LOOK.
//!
//! # Why the binary is the installer
//! Everyone driving a loop already has `agg`. Making it the installer buys ONE code path that e2e
//! can drive, instead of three sets of README instructions that rot silently. The skill files are
//! [`include_str!`]d from `plugin/skills/`, so a released binary installs them with no repo
//! checkout — and `plugin/` stays the single source of truth that also feeds the Claude plugin
//! marketplace.
//!
//! # The discovery matrix — OBSERVED, not documented
//! Each cell below was verified by dropping a probe skill in and asking the agent what it can see
//! (`copilot skill list`; `codex debug prompt-input`; `claude -p`). Two of these contradict what
//! you would guess from the docs, so do not "simplify" this table without re-running those probes.
//!
//! | location | claude | codex | copilot |
//! |---|---|---|---|
//! | `.claude/skills/` (project) | ✅ | ❌ | ✅ |
//! | **`.agents/skills/` (project)** | ❌ | ✅ | ✅ |
//! | `~/.claude/skills/` (user) | ✅ | ❌ | ❌ |
//! | **`~/.agents/skills/` (user)** | ❌ | ✅ | ✅ |
//!
//! `.agents/` is the emerging agent-neutral convention, and Codex and Copilot BOTH honour it — so
//! two directories cover all three agents. No agent reads every location, so one shared directory
//! is not possible.
//!
//! # Naming: the `name:` frontmatter key, and why the directory ALSO says `agg-`
//! Each SKILL.md carries `name: agg-new`. That key is what stops the skill being called plain
//! `new` — Copilot's plugin loader ignores the plugin namespace entirely, so without it the skill
//! surfaces as `new`, which is both useless and a collision. Verified on all three:
//!
//! | | via this installer | via the plugin marketplace |
//! |---|---|---|
//! | claude | `/agg-new` | **`/agg:new`** — the plugin namespace WINS over `name:` |
//! | codex | `agg-new` | `agg-new` |
//! | copilot | `agg-new` | `agg-new` (plain `new` without the key) |
//!
//! So `name:` is safe for Claude — its `/agg:new` is unchanged — and necessary for Copilot.
//! The DIRECTORY is named `agg-new` too, belt-and-braces: Codex and Copilot both fall back to the
//! directory name when a skill has no `name:` key.
//!
//! # How a user INVOKES the skill — the PREFIX is not the same on all three
//! | agent | invoke with | |
//! |---|---|---|
//! | claude | `/agg-new` | `/agg:new` when installed from the plugin marketplace instead — the `agg:` namespace is the PLUGIN's, not the file's |
//! | copilot | `/agg-new` | every skill is a slash command. Its SDK: `userInvocable` = *"whether the skill can be invoked by the user as a slash command"*, and it is `true` for ours (verified on a live `session.skills_loaded`) |
//! | codex | **`$agg-new`** | Codex uses `$`, **not** `/`. A `/agg-new` is "Unrecognized command" — requested and closed as not planned (openai/codex#11817). `/skills` opens a picker. |
//!
//! On all three, the skill is ALSO selected by matching the request against its `description:` —
//! which is the only route that works headlessly. So the description is load-bearing, not decoration.
//!
//! # This installer is not the only route
//! Codex and Copilot BOTH have plugin marketplaces, and both accept this repo's existing
//! `.claude-plugin/marketplace.json` verbatim (`codex plugin marketplace add` / `copilot plugin
//! marketplace add ssenge/AgenticGoGo`). That path needs no code here at all. This command exists
//! for the project-local install, which a marketplace cannot do.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The `/agg:*` skills, embedded at compile time from the same files the Claude plugin ships.
/// The tuple is (destination directory name, file body) — see the module doc for why the directory
/// name carries the `agg-` namespace.
pub const SKILLS: &[(&str, &str)] = &[
    ("agg-new", include_str!("../plugin/skills/new/SKILL.md")),
    ("agg-status", include_str!("../plugin/skills/status/SKILL.md")),
    ("agg-supervise", include_str!("../plugin/skills/supervise/SKILL.md")),
];

/// The skills directory `agent` discovers, relative to a project root.
///
/// Claude reads `.claude/skills/`; Codex and Copilot read `.agents/skills/`. An unknown agent is
/// the caller's problem — [`crate::backend::for_name`] rejects it first.
fn relative_root(agent: &str) -> &'static str {
    match agent {
        "claude" => ".claude/skills",
        // codex + copilot both read the agent-neutral dir. Anything new should too.
        _ => ".agents/skills",
    }
}

/// Which agent is running THIS process's terminal, from the marker each CLI exports into its
/// shell. `None` when we are not inside any of them (a plain terminal).
///
/// # Why this exists
/// `agg skills install` is run at SETUP time — before `agg.yaml` exists. Falling back to
/// `AggConfig::agent_name`, which returns `claude` for a missing file, meant a Codex user running
/// the documented first step got the skills written to `.claude/skills/` — a directory Codex never
/// reads. They then saw no skill, with no error, and nothing to tell them why.
///
/// Markers verified by dumping the environment inside each agent (they are not documented):
/// `COPILOT_CLI=1`, `CODEX_THREAD_ID=<uuid>`, `CLAUDECODE=1`. Checked innermost-first, because
/// nesting one agent inside another leaves BOTH set.
pub fn host_agent() -> Option<&'static str> {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    // order matters: a codex/copilot session launched from inside Claude Code inherits CLAUDECODE.
    if set("COPILOT_CLI") {
        Some("copilot")
    } else if set("CODEX_THREAD_ID") {
        Some("codex")
    } else if set("CLAUDECODE") {
        Some("claude")
    } else {
        None
    }
}

/// Where the `/agg:*` skills belong for `agent`: under `dir` for a project install, or under
/// `$HOME` for a user-wide one.
pub fn skills_root(agent: &str, dir: &Path, user: bool) -> Result<PathBuf> {
    let base = if user {
        std::env::home_dir().context("cannot find your home directory (is $HOME set?)")?
    } else {
        dir.to_path_buf()
    };
    Ok(base.join(relative_root(agent)))
}

/// Copy the three skills into the directory `agent` looks in. Idempotent — an existing install is
/// overwritten, which is what makes this the upgrade path too.
///
/// Returns the root the skills were written to.
pub fn install(agent: &str, dir: &Path, user: bool) -> Result<PathBuf> {
    // reject an unknown agent HERE, before writing anything, with the error that lists the known
    // ones — rather than silently defaulting it into `.agents/skills` via relative_root.
    crate::backend::for_name(agent)?;

    let root = skills_root(agent, dir, user)?;
    for (name, body) in SKILLS {
        let d = root.join(name);
        std::fs::create_dir_all(&d)
            .with_context(|| format!("creating {}", d.display()))?;
        let f = d.join("SKILL.md");
        std::fs::write(&f, body).with_context(|| format!("writing {}", f.display()))?;
    }
    Ok(root)
}

/// Are all three skills present where `agent` would look? Used by `agg doctor` to report the
/// install as a fact, not to fail the run — the skills are optional, the loop runs without them.
pub fn installed(agent: &str, dir: &Path, user: bool) -> bool {
    let Ok(root) = skills_root(agent, dir, user) else {
        return false;
    };
    SKILLS.iter().all(|(name, _)| root.join(name).join("SKILL.md").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The skills are `include_str!`'d into the binary, so stale content compiles cleanly and the
    /// green build never notices — that already shipped a `/agg:new` scaffold generating retired
    /// keys, and prose references (e.g. a `status` skill citing `budget.total`) slipped past several
    /// hand sweeps. The scaffold-parse guard (config.rs) covers the config block; this covers the
    /// PROSE. Every token here is unambiguously retired and has no live homonym (`over_budget`,
    /// `--max-sessions`, and the `budget_total` state.json field do NOT match these patterns).
    #[test]
    fn no_skill_body_mentions_a_retired_config_key() {
        const RETIRED: &[&str] = &[
            "stop_when",       // → done_if
            "halt_when",       // → abort_if
            "budget.total",    // → sequence.limits.tokens
            "cost.total",      // → sequence.limits.cost
            "sequence.budget", // → sequence.limits
            "sequence.cost",   // → sequence.limits
            "max_sessions:",   // → sequence.limits.sessions (the --max-sessions FLAG survives)
            // NB: `goals.yaml` is deliberately NOT here — the skills legitimately say "there is no
            // goals.yaml", which is correct prose, not a stale reference.
        ];
        for (name, body) in SKILLS {
            for tok in RETIRED {
                assert!(
                    !body.contains(tok),
                    "skill `{name}` mentions the retired `{tok}` — the SEQUENCES rewrite renamed it; \
                     update plugin/skills/*/SKILL.md (this skill ships inside the binary)"
                );
            }
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agg-skills-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// THE thing this module gets wrong if anyone "simplifies" it: Claude and the other two do NOT
    /// share a directory. Claude cannot see `.agents/skills/`, and Codex cannot see
    /// `.claude/skills/` — both verified on the wire. Collapsing these to one path silently
    /// installs the skills where the agent will never look.
    #[test]
    fn claude_gets_dot_claude_and_the_neutral_agents_get_dot_agents() {
        let d = Path::new("/proj");
        assert_eq!(skills_root("claude", d, false).unwrap(), Path::new("/proj/.claude/skills"));
        assert_eq!(skills_root("codex", d, false).unwrap(), Path::new("/proj/.agents/skills"));
        assert_eq!(skills_root("copilot", d, false).unwrap(), Path::new("/proj/.agents/skills"));
        // codex and copilot land in the SAME place — that is the point of `.agents/`.
        assert_eq!(
            skills_root("codex", d, false).unwrap(),
            skills_root("copilot", d, false).unwrap()
        );
    }

    /// `--user` resolves against $HOME, not the project — and still splits claude vs the rest.
    #[test]
    fn user_scope_resolves_against_home() {
        let home = std::env::home_dir().expect("a home dir");
        assert_eq!(
            skills_root("claude", Path::new("/proj"), true).unwrap(),
            home.join(".claude/skills")
        );
        assert_eq!(
            skills_root("copilot", Path::new("/proj"), true).unwrap(),
            home.join(".agents/skills")
        );
    }

    /// Installing must land a real SKILL.md, with real frontmatter, in a directory named so that
    /// Codex/Copilot derive the `agg-` namespace from it.
    #[test]
    fn install_writes_all_three_skills_where_the_agent_looks() {
        let d = tmpdir("install");
        let root = install("codex", &d, false).unwrap();
        assert_eq!(root, d.join(".agents/skills"));

        for name in ["agg-new", "agg-status", "agg-supervise"] {
            let f = root.join(name).join("SKILL.md");
            let body = std::fs::read_to_string(&f)
                .unwrap_or_else(|_| panic!("{} must exist", f.display()));
            // the frontmatter is what makes it discoverable at all — an empty copy is worse than
            // no copy, because the agent would list a skill that says nothing.
            assert!(body.starts_with("---\n"), "{name} must open with YAML frontmatter");
            assert!(body.contains("description:"), "{name} needs a description — it is what the agent routes on");
        }
        assert!(installed("codex", &d, false));
        // …and NOT for claude, which looks somewhere else entirely.
        assert!(!installed("claude", &d, false));

        // idempotent: a second install over the top is the upgrade path.
        install("codex", &d, false).unwrap();
        assert!(installed("codex", &d, false));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An unknown agent must fail BEFORE writing anything, rather than defaulting into the neutral
    /// dir and leaving files behind for an agent that does not exist.
    #[test]
    fn an_unknown_agent_is_refused_and_writes_nothing() {
        let d = tmpdir("unknown");
        let e = install("gemini", &d, false).unwrap_err().to_string();
        assert!(e.contains("unknown agent `gemini`"), "got: {e}");
        assert!(!d.join(".agents").exists(), "nothing may be written for an unknown agent");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The embedded copies must be the REAL skills, not placeholders — this is the check that fails
    /// if someone points include_str! at the wrong file.
    #[test]
    fn the_embedded_skills_are_the_real_ones() {
        let (_, new) = SKILLS[0];
        assert!(new.contains("AgenticGoGo"), "agg-new must be the real skill");
        assert!(
            new.contains("agent: <claude|codex|copilot>"),
            "agg-new must carry the capability-aware agg.yaml template — that is the whole point"
        );
        assert_eq!(SKILLS.len(), 3);
    }

    /// REGRESSION: every skill must declare `name: agg-*` in its frontmatter. Without it, Copilot's
    /// PLUGIN loader (which ignores the plugin namespace) surfaces `plugin/skills/new/` as a skill
    /// literally called `new` — a generic name that collides with anything. Claude is unaffected:
    /// its plugin namespace still wins, so `/agg:new` keeps its name. All three verified on the
    /// wire. If this ever drops, Copilot users get `new`/`status`/`supervise` in their skill list.
    #[test]
    fn every_skill_declares_its_namespaced_name() {
        for (dir, body) in SKILLS {
            let want = format!("\nname: {dir}\n");
            assert!(
                body.contains(&want),
                "{dir}/SKILL.md must declare `name: {dir}` — without it Copilot names it `{}`",
                dir.strip_prefix("agg-").unwrap_or(dir)
            );
        }
    }
}

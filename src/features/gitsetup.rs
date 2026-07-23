//! The git-setup feature group: agg's run-start git preconditions as a `pre_start` plugin.

use anyhow::Result;

use crate::loop_::{Bootstrap, PreStart};

/// The `pre_start` feature: agg's run-start git preconditions, in order — recover a stranded merge
/// from a prior crash, require a clean git repo (session isolation is MANDATORY), ensure `agg/state`
/// is gitignored (runtime state survives rollback), and resolve the isolation base branch (→
/// `boot.iso_base` for the constructor). Runs before the loop state exists; any `bail!` is a hard
/// error out of `run()`, exactly as the old inline block. Verbatim, just grouped under one feature.
pub struct GitSetup;
impl PreStart for GitSetup {
    fn run(&self, boot: &mut Bootstrap) -> Result<()> {
        let dir = boot.dir;
        let iso = &boot.cfg.session_isolation;
        // recover a stranded merge left by a prior crash (guarded on being a git repo)
        if crate::git::is_repo(dir) {
            crate::git::recover_stranded_merge(dir, &iso.branch_prefix);
        }
        // require a git repo with a clean tracked tree
        if !crate::git::is_repo(dir) {
            anyhow::bail!(
                "session isolation is mandatory, but this is not a git repository.\n  \
                 fix:  git init && git add -A && git commit -m 'agg baseline'"
            );
        }
        if !crate::git::is_clean(dir) {
            anyhow::bail!(
                "session isolation is mandatory, but the work tree has uncommitted tracked changes.\n  \
                 fix:  commit or stash your changes first  (git status shows them)"
            );
        }
        // keep runtime state untracked (survives rollback)
        crate::git::ensure_agg_gitignored(dir);
        // resolve the isolation base branch (configured, else current; refuse a detached HEAD)
        let iso_base: String = if iso.base_branch.is_empty() {
            match crate::git::current_branch(dir) {
                Some(b) => b,
                None => anyhow::bail!(
                    "session isolation is mandatory, but HEAD is detached.\n  \
                     fix:  git switch -c <branch>"
                ),
            }
        } else {
            iso.base_branch.clone()
        };
        eprintln!("  [iso] per-session branch isolation ON — base branch '{iso_base}'");
        boot.iso_base = Some(iso_base);
        Ok(())
    }
}

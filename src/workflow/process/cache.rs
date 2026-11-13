//! The judge cache (A-37): a check's met verdict from an earlier run of it on exactly the same project
//! content. The worker's self-check and the run's judge run the same check on the same state, and a long check (the
//! gates, a rerun of every measure) needn't run twice. `agg cached [--ignore <path>]… -- <command>…` runs the check,
//! or answers with the cached verdict; the process wraps its script checks in it.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Where the verdicts are, under the project's git folder (shared by its worktrees, never committed).
const DIR: &str = "agg-verdicts";

fn git(cwd: &Path, args: &[&str], index: Option<&Path>) -> Option<String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(cwd).args(args);
    if let Some(i) = index {
        c.env("GIT_INDEX_FILE", i);
    }
    let out = c.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The hash of the working tree as git sees it (untracked files in, ignored ones out, the files `ignore` names
/// out); none when a nested repository's changes would be invisible in it.
fn content(cwd: &Path, ignore: &[String]) -> Option<String> {
    let tmp = std::env::temp_dir().join(format!("agg-cache-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())));
    std::fs::create_dir_all(&tmp).ok()?;
    let index = tmp.join("index");
    let tree = (|| {
        if let Some(current) = git(cwd, &["rev-parse", "--git-path", "index"], None) {
            // its stat cache spares hashing every unchanged file again
            let _ = std::fs::copy(cwd.join(current), &index);
        }
        git(cwd, &["add", "-A", ":/"], Some(&index))?;
        if !ignore.is_empty() {
            let args: Vec<&str> = ["rm", "--cached", "-q", "--ignore-unmatch", "--"].into_iter()
                .chain(ignore.iter().map(String::as_str)).collect();
            git(cwd, &args, Some(&index))?;
        }
        if git(cwd, &["ls-files", "-s"], Some(&index))?.lines().any(|l| l.starts_with("160000")) {
            return None;
        }
        git(cwd, &["write-tree"], Some(&index))
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    tree
}

/// The cache key: the check, its own code and the project's content.
fn key(command: &[String], tree: &str) -> String {
    let code = command.first().and_then(|c| std::fs::read(c).ok()).unwrap_or_default();
    let mut bytes = format!("{}\0{tree}", command.join("\0")).into_bytes();
    bytes.extend(code);
    crate::protect::sha256::hex(&bytes)
}

/// The check's exit code and what it printed: the cached met verdict, or the check run, a met verdict cached (an unmet
/// one could be a flake that a rerun on the same content clears).
pub fn cached(cwd: &Path, command: &[String], ignore: &[String]) -> (i32, String) {
    let tree = content(cwd, ignore);
    let store: Option<PathBuf> = git(cwd, &["rev-parse", "--git-common-dir"], None)
        .map(|d| cwd.join(d)).and_then(|d| d.canonicalize().ok()).map(|d| d.join(DIR));
    let path = tree.as_ref().zip(store.as_ref()).map(|(t, s)| s.join(format!("{}.json", key(command, t))));
    if let Some(text) = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
        return (0, format!("{}\n", text.trim()));
    }
    let Some((program, args)) = command.split_first() else { return (2, String::new()) };
    let out = Command::new(program).args(args).current_dir(cwd).stderr(std::process::Stdio::inherit()).output();
    let Ok(out) = out else { return (127, String::new()) };
    let printed = String::from_utf8_lossy(&out.stdout).to_string();
    let code = out.status.code().unwrap_or(1);
    let verdict: Option<serde_json::Value> = printed.trim().lines().last().and_then(|l| serde_json::from_str(l).ok());
    if let (Some(p), Some(mut v), 0) = (path, verdict, code) {
        if v["met"] == true {
            let why = format!("{} (cached: the same check on the same content)", v["rationale"].as_str().unwrap_or_default());
            v["rationale"] = why.into();
            let _ = p.parent().map(std::fs::create_dir_all);
            let _ = std::fs::write(p, format!("{v}\n"));
        }
    }
    (code, printed)
}

/// The command `agg cached`: its arguments after the subcommand. `--content` prints the project's content hash instead
/// of running a check (exit 1 where there is none).
pub fn command(args: &[String]) -> ExitCode {
    let (mut ignore, mut hash_only) = (vec![], false);
    let mut it = args.iter().peekable();
    while let Some(a) = it.peek() {
        match a.as_str() {
            "--content" => {
                it.next();
                hash_only = true;
            }
            "--ignore" => {
                it.next();
                ignore.extend(it.next().cloned());
            }
            "--" => {
                it.next();
                break;
            }
            _ => break,
        }
    }
    if hash_only {
        // for a caller that keeps its own results by the project's content
        return match content(Path::new("."), &ignore) {
            Some(tree) => {
                println!("{tree}");
                ExitCode::SUCCESS
            }
            None => ExitCode::from(1),
        };
    }
    let command: Vec<String> = it.cloned().collect();
    let (code, printed) = cached(Path::new("."), &command, &ignore);
    print!("{printed}");
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@t"]] {
            git(dir.path(), args, None).unwrap();
        }
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        git(dir.path(), &["add", "-A"], None).unwrap();
        git(dir.path(), &["commit", "-q", "-m", "a"], None).unwrap();
        dir
    }

    fn check(count: &Path) -> Vec<String> {
        let script = format!("echo x >> {}; echo '{{\"met\": true, \"rationale\": \"ok\"}}'", count.display());
        vec!["sh".to_string(), "-c".to_string(), script]
    }

    /// The content hash changes with the files and not with the ones `ignore` names.
    #[test]
    fn the_content_hash_follows_the_files_but_not_the_ignored_ones() {
        let dir = repo();
        let ignore = vec!["notes.md".to_string()];
        let first = content(dir.path(), &ignore).unwrap();
        std::fs::write(dir.path().join("notes.md"), "n").unwrap();
        assert_eq!(content(dir.path(), &ignore).unwrap(), first);
        std::fs::write(dir.path().join("a.txt"), "changed").unwrap();
        assert_ne!(content(dir.path(), &ignore).unwrap(), first);
    }

    /// A met verdict is answered from the cache on the same content, not on changed content; the files `ignore`
    /// names don't count as content; an unmet verdict is never cached.
    #[test]
    fn a_met_verdict_is_reused_on_the_same_content_only() {
        let dir = repo();
        let count = dir.path().join("..").join(format!("count-{}.txt", std::process::id()));
        let runs = || std::fs::read_to_string(&count).unwrap_or_default().lines().count();
        let (code, first) = cached(dir.path(), &check(&count), &["notes.md".into()]);
        assert_eq!((code, runs()), (0, 1));
        assert!(first.contains("\"ok\"") && !first.contains("cached"));
        let (_, second) = cached(dir.path(), &check(&count), &["notes.md".into()]);
        assert_eq!(runs(), 1, "answered from the cache");
        assert!(second.contains("(cached: the same check on the same content)"), "{second}");
        std::fs::write(dir.path().join("notes.md"), "a worker's note").unwrap();
        cached(dir.path(), &check(&count), &["notes.md".into()]);
        assert_eq!(runs(), 1, "an ignored file is no change");
        std::fs::write(dir.path().join("a.txt"), "changed").unwrap();
        cached(dir.path(), &check(&count), &["notes.md".into()]);
        assert_eq!(runs(), 2, "changed content runs the check again");
        let unmet = vec!["sh".to_string(), "-c".to_string(), format!("echo y >> {}; echo '{{\"met\": false, \"rationale\": \"no\"}}'", count.display())];
        cached(dir.path(), &unmet, &[]);
        cached(dir.path(), &unmet, &[]);
        assert_eq!(runs(), 4, "an unmet verdict is run again");
        let _ = std::fs::remove_file(count);
    }
}

//! One `write_deny` pattern: parse it, match a project-relative path against it, and express it
//! for each kernel mechanism (a Seatbelt regex; a concrete path for a read-only bind).
//!
//! # The syntax, and why it is this small
//! Patterns are project-relative, `/`-separated, with three wildcards:
//!
//! | token | matches |
//! |---|---|
//! | `*` | any run of characters inside ONE path component |
//! | `?` | exactly one character inside one component |
//! | `**` | zero or more WHOLE components (it must be a component on its own) |
//!
//! A trailing `/` means "this directory and everything under it" and is stored as `/**`. Because
//! `**` also matches ZERO components, `tests/oracles/**` protects the directory `tests/oracles`
//! itself — so replacing it with a symlink, or deleting and recreating it, is a change too.
//!
//! Everything else that looks like glob syntax (`[…]`, `{…}`, a leading `!`, a `\` escape) is
//! REFUSED rather than read literally: a pattern its author believed was a character class and agg
//! matched as the literal text `[ab]` would protect nothing while reading like it did.

use anyhow::{bail, Result};

/// One component of a parsed pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Comp {
    /// a component that may contain `*` / `?` (or none, i.e. a literal name).
    Name(String),
    /// `**` — zero or more whole components.
    AnyDepth,
}

/// What a kernel mechanism that denies by CONCRETE PATH (a read-only bind) can make of a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// no wildcard at all — one concrete path (a file or a directory).
    Literal(String),
    /// `<literal dir>/**` — one concrete directory and everything that will ever be under it.
    Subtree(String),
    /// anything else (`**/*.golden`, `tests/*/x.json`): it can name files that do not exist yet in
    /// directories nobody listed, which no bind of an existing path can cover.
    Other,
}

/// A parsed, normalised `write_deny` pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    /// the normalised spelling — what agg prints and what two spellings dedupe on.
    pub text: String,
    comps: Vec<Comp>,
}

impl Glob {
    /// Parse and normalise one pattern, or say exactly why it is refused.
    ///
    /// Normalisation drops `.`/empty components and turns a trailing `/` into `/**`, so `./tests/`,
    /// `tests//` and `tests/**` are one pattern. Refused: an absolute path, any `..`, a pattern that
    /// names the project root, and glob syntax this matcher does not implement.
    pub fn parse(raw: &str) -> Result<Glob> {
        let t = raw.trim();
        if t.starts_with('/') {
            bail!("`{raw}` is absolute — write_deny patterns are relative to the project root");
        }
        if let Some(c) = t.chars().find(|c| "[]{}\\".contains(*c)) {
            bail!("`{raw}` uses `{c}`, which write_deny does not support (only `*`, `?` and `**`)");
        }
        if t.starts_with('!') {
            bail!("`{raw}` is a negation — write_deny only ever adds protection, it cannot carve one out");
        }
        let mut comps = Vec::new();
        for c in t.split('/') {
            match c {
                "" | "." => {}
                ".." => bail!("`{raw}` contains `..` — a protected path must stay inside the project"),
                "**" => {
                    // `a/**/**/b` is `a/**/b`: two adjacent any-depths match nothing more.
                    if comps.last() != Some(&Comp::AnyDepth) {
                        comps.push(Comp::AnyDepth);
                    }
                }
                c if c.contains("**") => {
                    bail!("`{raw}`: `**` must be a whole path component (`a/**/b`), not part of `{c}`")
                }
                c => comps.push(Comp::Name(c.to_string())),
            }
        }
        if comps.is_empty() {
            bail!("`{raw}` names the project root itself — protect specific paths instead");
        }
        if t.ends_with('/') && comps.last() != Some(&Comp::AnyDepth) {
            comps.push(Comp::AnyDepth);
        }
        let text = comps
            .iter()
            .map(|c| match c {
                Comp::Name(n) => n.as_str(),
                Comp::AnyDepth => "**",
            })
            .collect::<Vec<_>>()
            .join("/");
        Ok(Glob { text, comps })
    }

    /// Does the project-relative path `rel` (components joined by `/`, no leading `/`) match?
    /// `ci` = compare case-insensitively (the project lives on a case-insensitive filesystem).
    pub fn matches(&self, rel: &str, ci: bool) -> bool {
        let path: Vec<&str> = rel.split('/').filter(|c| !c.is_empty()).collect();
        match_comps(&self.comps, &path, ci)
    }

    /// Could a path STRICTLY BELOW the directory `dir_rel` match? The walk's pruning test: a
    /// directory for which no pattern answers yes is never descended into, so a `tests/oracles/**`
    /// project does not hash its `node_modules/`.
    pub fn could_match_below(&self, dir_rel: &str, ci: bool) -> bool {
        let path: Vec<&str> = dir_rel.split('/').filter(|c| !c.is_empty()).collect();
        prefix_match(&self.comps, &path, ci)
    }

    /// How a bind-based mechanism can express this pattern (see [`Shape`]).
    pub fn shape(&self) -> Shape {
        let is_lit = |c: &Comp| matches!(c, Comp::Name(n) if !n.contains(['*', '?']));
        let names = |cs: &[Comp]| {
            cs.iter()
                .map(|c| match c {
                    Comp::Name(n) => n.clone(),
                    Comp::AnyDepth => String::new(),
                })
                .collect::<Vec<_>>()
                .join("/")
        };
        match self.comps.split_last() {
            Some((Comp::AnyDepth, head)) if !head.is_empty() && head.iter().all(is_lit) => {
                Shape::Subtree(names(head))
            }
            _ if self.comps.iter().all(is_lit) => Shape::Literal(names(&self.comps)),
            _ => Shape::Other,
        }
    }

    /// The leading run of wildcard-free components — the deepest directory every match lies under
    /// (empty for a pattern that starts with a wildcard).
    pub fn literal_prefix(&self) -> Vec<String> {
        self.comps
            .iter()
            .take_while(|c| matches!(c, Comp::Name(n) if !n.contains(['*', '?'])))
            .map(|c| match c {
                Comp::Name(n) => n.clone(),
                Comp::AnyDepth => unreachable!("take_while stops at `**`"),
            })
            .collect()
    }

    /// The pattern as an anchored POSIX regex over ABSOLUTE paths under `root` — the form Seatbelt's
    /// `(regex #"…")` filter takes. `root` must already be canonical: Seatbelt matches the resolved
    /// path, never the spelling the process used.
    ///
    /// With `ci`, every ASCII letter of the PATTERN becomes a two-case class (`[tT]`): Seatbelt has
    /// no case-insensitive flag, and on a case-insensitive volume `TESTS/x` and `tests/x` are one
    /// file. The root is left as resolved — `canonicalize` already returned its on-disk spelling.
    pub fn to_regex(&self, root: &str, ci: bool) -> String {
        let mut out = format!("^{}", regex_literal(root.trim_end_matches('/'), false));
        let n = self.comps.len();
        for (i, c) in self.comps.iter().enumerate() {
            match c {
                // trailing `/**` also matches the directory itself ⇒ the whole tail is optional.
                Comp::AnyDepth if i + 1 == n => out.push_str("(/.*)?"),
                // a middle (or leading) `**` is zero or more `name/` groups.
                Comp::AnyDepth => out.push_str("/(.*/)?"),
                Comp::Name(name) => {
                    if i == 0 || self.comps[i - 1] != Comp::AnyDepth {
                        out.push('/');
                    }
                    for ch in name.chars() {
                        match ch {
                            '*' => out.push_str("[^/]*"),
                            '?' => out.push_str("[^/]"),
                            c => out.push_str(&regex_literal(&c.to_string(), ci)),
                        }
                    }
                }
            }
        }
        out.push('$');
        out
    }
}

/// Escape `s` for a POSIX regex inside a Seatbelt `#"…"` string; with `ci`, letters become
/// two-case classes.
pub fn regex_literal(s: &str, ci: bool) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if ci && c.is_ascii_alphabetic() {
            out.push('[');
            out.push(c.to_ascii_lowercase());
            out.push(c.to_ascii_uppercase());
            out.push(']');
            continue;
        }
        if r#"\^$.|?*+()[]{}""#.contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Full match of pattern components against path components; `**` consumes zero or more.
fn match_comps(pat: &[Comp], path: &[&str], ci: bool) -> bool {
    match pat.split_first() {
        None => path.is_empty(),
        Some((Comp::AnyDepth, rest)) => (0..=path.len()).any(|k| match_comps(rest, &path[k..], ci)),
        Some((Comp::Name(n), rest)) => {
            !path.is_empty() && match_name(n, path[0], ci) && match_comps(rest, &path[1..], ci)
        }
    }
}

/// Can `path` be extended (by at least one more component) into a match?
fn prefix_match(pat: &[Comp], path: &[&str], ci: bool) -> bool {
    match pat.split_first() {
        None => false,
        Some((Comp::AnyDepth, _)) => true,
        Some((Comp::Name(n), rest)) => match path.split_first() {
            None => true,
            Some((head, tail)) => match_name(n, head, ci) && prefix_match(rest, tail, ci),
        },
    }
}

/// One component against a name that may hold `*` and `?`.
fn match_name(pat: &str, name: &str, ci: bool) -> bool {
    let fold = |s: &str| if ci { s.to_lowercase() } else { s.to_string() };
    let (p, n): (Vec<char>, Vec<char>) = (fold(pat).chars().collect(), fold(name).chars().collect());
    wild(&p, &n)
}

/// Classic `*`/`?` wildcard match over chars.
fn wild(p: &[char], n: &[char]) -> bool {
    match p.split_first() {
        None => n.is_empty(),
        Some(('*', rest)) => (0..=n.len()).any(|k| wild(rest, &n[k..])),
        Some(('?', rest)) => !n.is_empty() && wild(rest, &n[1..]),
        Some((c, rest)) => n.first() == Some(c) && wild(rest, &n[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(s: &str) -> Glob {
        Glob::parse(s).unwrap_or_else(|e| panic!("`{s}` should parse: {e}"))
    }

    #[test]
    fn spellings_of_one_pattern_normalise_to_one_text() {
        for s in ["tests/oracles/**", "./tests/oracles/", "tests//oracles/**/", " tests/oracles/**/** "] {
            assert_eq!(g(s).text, "tests/oracles/**", "spelled `{s}`");
        }
        assert_eq!(g(".gitlab-ci.yml").text, ".gitlab-ci.yml");
    }

    #[test]
    fn refuses_what_it_cannot_honestly_match() {
        for bad in ["/etc/passwd", "../x", "a/../b", "", ".", "./", "x/[ab]", "{a,b}", "!keep", "a\\b", "a**/b", "x/**y"] {
            assert!(Glob::parse(bad).is_err(), "`{bad}` must be refused");
        }
    }

    #[test]
    fn star_stays_inside_one_component_and_double_star_crosses_them() {
        let p = g("tests/*/x.json");
        assert!(p.matches("tests/a/x.json", false));
        assert!(!p.matches("tests/a/b/x.json", false));
        let p = g("**/*.golden");
        assert!(p.matches("a.golden", false), "a leading `**` matches zero components");
        assert!(p.matches("deep/er/a.golden", false));
        assert!(!p.matches("a.golden.bak", false));
        assert!(g("a/?.txt").matches("a/b.txt", false));
        assert!(!g("a/?.txt").matches("a/bb.txt", false));
    }

    /// `dir/**` protects the directory itself, so swapping it for a symlink or recreating it counts.
    #[test]
    fn a_trailing_double_star_also_matches_the_directory() {
        let p = g("tests/oracles/**");
        assert!(p.matches("tests/oracles", false));
        assert!(p.matches("tests/oracles/x.json", false));
        assert!(p.matches("tests/oracles/sub/new.json", false));
        assert!(!p.matches("tests/oracles2/x.json", false));
        assert!(!p.matches("tests", false));
    }

    #[test]
    fn case_folding_is_opt_in() {
        let p = g("tests/oracles/**");
        assert!(!p.matches("Tests/ORACLES/x.json", false));
        assert!(p.matches("Tests/ORACLES/x.json", true));
    }

    #[test]
    fn pruning_descends_only_where_a_match_can_live() {
        let p = g("tests/oracles/**");
        assert!(p.could_match_below("", false));
        assert!(p.could_match_below("tests", false));
        assert!(p.could_match_below("tests/oracles", false));
        assert!(p.could_match_below("tests/oracles/deep", false));
        assert!(!p.could_match_below("node_modules", false));
        assert!(!p.could_match_below("tests/unit", false));
        assert!(g("**/*.golden").could_match_below("node_modules/x", false));
        assert!(!g(".gitlab-ci.yml").could_match_below(".gitlab-ci.yml", false), "a file has no inside");
    }

    #[test]
    fn shapes_classify_what_a_bind_can_express() {
        assert_eq!(g("tests/oracles/**").shape(), Shape::Subtree("tests/oracles".into()));
        assert_eq!(g(".gitlab-ci.yml").shape(), Shape::Literal(".gitlab-ci.yml".into()));
        assert_eq!(g("cfg/rules.yml").shape(), Shape::Literal("cfg/rules.yml".into()));
        assert_eq!(g("**/*.golden").shape(), Shape::Other);
        assert_eq!(g("tests/*/x.json").shape(), Shape::Other);
        assert_eq!(g("**").shape(), Shape::Other, "the whole project is not one bind");
        assert_eq!(g("tests/*/x").literal_prefix(), vec!["tests".to_string()]);
    }

    #[test]
    fn regexes_anchor_under_the_root_and_mirror_the_matcher() {
        assert_eq!(g("tests/oracles/**").to_regex("/r", false), "^/r/tests/oracles(/.*)?$");
        assert_eq!(g("**/*.golden").to_regex("/r", false), "^/r/(.*/)?[^/]*\\.golden$");
        assert_eq!(g("a/**/b").to_regex("/r", false), "^/r/a/(.*/)?b$");
        assert_eq!(g(".ci.yml").to_regex("/r.x", false), "^/r\\.x/\\.ci\\.yml$");
        assert_eq!(g("ab/**").to_regex("/R", true), "^/R/[aA][bB](/.*)?$", "the root is not folded");
    }
}

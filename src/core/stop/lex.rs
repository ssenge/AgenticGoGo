//! The stop-condition lexer: `Tok` and `tokenize`.

use anyhow::{anyhow, bail, Result};

// ---------------- tokens ----------------

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Tok {
    Num(f64),
    Ident(String),
    LParen,
    RParen,
    Or,
    And,
    Not,
    Op(String), // == != >= <= > <
}

pub(super) fn tokenize(s: &str) -> Result<Vec<Tok>> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        // This loop walks RAW BYTES and slices the &str on those byte offsets. A multi-byte char
        // (one umlaut in a user's YAML) would make `b[i] as char` a lone continuation byte, and the
        // slice below would land mid-character and PANIC. The grammar is ASCII-only — say so, and
        // error instead. `s[i..]` is safe here: every byte consumed so far was ASCII, so `i` is on
        // a char boundary.
        if !b[i].is_ascii() {
            let ch = s[i..].chars().next().unwrap_or('?');
            bail!("unexpected character `{ch}` in stop condition (the grammar is ASCII-only)");
        }
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
        } else if c == '(' {
            out.push(Tok::LParen);
            i += 1;
        } else if c == ')' {
            out.push(Tok::RParen);
            i += 1;
        } else if c == '|' && i + 1 < b.len() && b[i + 1] == b'|' {
            out.push(Tok::Or);
            i += 2;
        } else if c == '&' && i + 1 < b.len() && b[i + 1] == b'&' {
            out.push(Tok::And);
            i += 2;
        } else if matches!(c, '=' | '!' | '>' | '<') {
            // two-char ops first. The `is_ascii` guard is not cosmetic: `>ö` would slice s[i..i+2]
            // straight through the middle of `ö` and panic. A non-ASCII second byte falls through
            // to the single-char op (or the error), and the loop head above then rejects it.
            let two = if i + 1 < b.len() && b[i + 1].is_ascii() { &s[i..i + 2] } else { "" };
            if matches!(two, "==" | "!=" | ">=" | "<=") {
                out.push(Tok::Op(two.to_string()));
                i += 2;
            } else if matches!(c, '>' | '<') {
                out.push(Tok::Op(c.to_string()));
                i += 1;
            } else {
                bail!("unexpected `{c}` in stop condition");
            }
        } else if c.is_ascii_digit() || c == '.' {
            // A LEADING `.` still starts a number, so the leading-dot float `.5` keeps working.
            // That is what makes the accessor below safe to fold into the identifier charset:
            // `.` after an identifier char continues the identifier, `.` anywhere else is a number.
            let start = i;
            while i < b.len() && ((b[i] as char).is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            let n: f64 = s[start..i].parse().map_err(|_| anyhow!("bad number `{}`", &s[start..i]))?;
            out.push(Tok::Num(n));
        } else if c.is_ascii_alphabetic() || c == '_' {
            // `.` is an identifier CONTINUATION char (never a start char), so `coverage.value`
            // lexes as one Ident and `met_fraction >= 0.75` still lexes as Ident, Op, Num. This is
            // why there is no `Dot` token: a Dot token would have to steal `.` from the number
            // branch and would silently break `.5`.
            //
            // `is_ascii_alphanumeric`, NOT `is_alphanumeric`: the latter is true for the lead byte
            // of `ö` (0xC3 as char == 'Ã'), so it would swallow half a char and then slice on the
            // boundary — the panic this guards. Stopping here hands the byte back to the loop head,
            // which errors.
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.') {
                i += 1;
            }
            let word = &s[start..i];
            match word {
                "OR" | "or" => out.push(Tok::Or),
                "AND" | "and" => out.push(Tok::And),
                "NOT" | "not" => out.push(Tok::Not),
                _ => out.push(Tok::Ident(word.to_string())),
            }
        } else {
            bail!("unexpected character `{c}` in stop condition");
        }
    }
    Ok(out)
}

//! Render the "How it works" architecture diagram to an SVG for the README.
//!
//! Hand-drawing this as ASCII in the README drifts and never lines up. This draws it
//! programmatically: real boxes, arrows, and a loop-back, laid out on a grid so it's always
//! crisp. The stages mirror the actual per-cycle sequence in `src/loop_.rs`.
//!
//! Usage:  cargo run --example how_it_works_svg > assets/how-it-works.svg
//!         rsvg-convert -z 2 -o assets/how-it-works.png assets/how-it-works.svg

// A one-off diagram generator (an example binary), not library code — the drawing helpers take
// many positional params by design; splitting each into a builder struct would be pure ceremony.
#![allow(clippy::too_many_arguments)]

use std::fmt::Write as _;

// palette (matches the dashboard screenshot's GitHub-dark theme)
const BG: &str = "#0d1117";
const CARD: &str = "#161b22";
const BORDER: &str = "#30363d";
const FG: &str = "#c9d1d9";
const MUTED: &str = "#8b949e";
const CYAN: &str = "#39c5cf";
const GREEN: &str = "#3fb950";
const AMBER: &str = "#d29922";
const PURPLE: &str = "#bc8cff";
const RED: &str = "#ff7b72";

const W: f32 = 1520.0;
const H: f32 = 1010.0;

struct Svg(String);
impl Svg {
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, fill: &str, stroke: &str) {
        let _ = writeln!(
            self.0,
            "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" rx=\"9\" fill=\"{fill}\" \
             stroke=\"{stroke}\" stroke-width=\"1.5\"/>"
        );
    }
    fn text(&mut self, x: f32, y: f32, s: &str, fill: &str, size: f32, weight: &str, anchor: &str, mono: bool) {
        let font = if mono { "SFMono-Regular,Menlo,Consolas,monospace" } else { "-apple-system,Segoe UI,Roboto,sans-serif" };
        let _ = writeln!(
            self.0,
            "<text x=\"{x}\" y=\"{y}\" fill=\"{fill}\" font-size=\"{size}\" font-weight=\"{weight}\" \
             font-family=\"{font}\" text-anchor=\"{anchor}\">{}</text>",
            xml_escape(s)
        );
    }
    fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, stroke: &str, arrow: bool, dash: bool) {
        let d = if dash { " stroke-dasharray=\"5 4\"" } else { "" };
        let m = if arrow { " marker-end=\"url(#arrow)\"" } else { "" };
        let _ = writeln!(
            self.0,
            "<line x1=\"{x1}\" y1=\"{y1}\" x2=\"{x2}\" y2=\"{y2}\" stroke=\"{stroke}\" \
             stroke-width=\"2\"{d}{m}/>"
        );
    }
    fn path(&mut self, d: &str, stroke: &str, arrow: bool) {
        let m = if arrow { " marker-end=\"url(#arrow)\"" } else { "" };
        let _ = writeln!(self.0, "<path d=\"{d}\" fill=\"none\" stroke=\"{stroke}\" stroke-width=\"2\"{m}/>");
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// One stage box in the per-cycle pipeline: a title + a muted sub-line, colored accent.
fn stage(svg: &mut Svg, x: f32, y: f32, w: f32, h: f32, accent: &str, title: &str, sub: &str) {
    svg.rect(x, y, w, h, CARD, BORDER);
    // accent bar on the left
    let _ = writeln!(svg.0, "<rect x=\"{x}\" y=\"{y}\" width=\"5\" height=\"{h}\" rx=\"2\" fill=\"{accent}\"/>");
    svg.text(x + 18.0, y + 26.0, title, FG, 17.0, "600", "start", true);
    svg.text(x + 18.0, y + 47.0, sub, MUTED, 13.0, "400", "start", false);
}

fn main() {
    let mut s = Svg(String::new());
    let _ = writeln!(
        s.0,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{W}\" height=\"{H}\" viewBox=\"0 0 {W} {H}\">"
    );
    // arrowhead marker
    s.0.push_str(
        "<defs><marker id=\"arrow\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" markerWidth=\"7\" \
         markerHeight=\"7\" orient=\"auto-start-reverse\"><path d=\"M0,0 L10,5 L0,10 z\" fill=\"#8b949e\"/></marker></defs>\n",
    );
    s.rect(0.0, 0.0, W, H, BG, BG);

    // ── top: setup ────────────────────────────────────────────────────────────────────────────
    s.text(40.0, 46.0, "you", CYAN, 18.0, "700", "start", true);
    s.text(40.0, 68.0, "/agg:new", MUTED, 14.0, "400", "start", true);
    s.line(150.0, 40.0, 250.0, 40.0, MUTED, true, false);
    stage(&mut s, 260.0, 16.0, 470.0, 52.0, GREEN, "goals.yaml · agg.yaml · AGG_RESUME.md",
        "typed goals + judges, ceilings, the resume prompt");
    // baseline judge
    s.line(495.0, 68.0, 495.0, 96.0, MUTED, true, false);
    stage(&mut s, 300.0, 96.0, 390.0, 52.0, AMBER, "baseline judge (once)",
        "already done? invariant already broken? bail early");

    // ── the loop card ───────────────────────────────────────────────────────────────────────
    let lx = 40.0;
    let ly = 176.0;

    // pipeline stages: (stage, accent, title, sub). `stage` groups them under the 4 deterministic
    // outer-loop phases (INJECT/RUN/VERIFY/GATE); RUN is the single stochastic step. Height is
    // derived from these so nothing is ever clipped.
    let steps: &[(&str, &str, &str, &str)] = &[
        ("INJECT", PURPLE, "drain steering bus", "apply inject / pause / resume / budget / stop from you (or /agg:supervise on your phone)"),
        ("INJECT", GREEN, "build the prompt", "inject AGG_MEMORY.md + last-session block + your instruction, prepend prompt_includes"),
        ("RUN", CYAN, "a FRESH claude -p worker  ·  STOCHASTIC", "the black box: it plans/acts/observes however it likes (ReAct, COAR, …) · heartbeat · two-signal watchdog"),
        ("VERIFY", AMBER, "worker exits → rate-limit check", "429 / limit → back off and retry the session (no judging on an incomplete run)"),
        ("VERIFY", FG, "stage the merge (git isolation)", "merge the session branch --no-commit onto base, uncommitted, so judges test the MERGED tree"),
        ("VERIFY", GREEN, "run JUDGES → update GOALS", "scripts + LLM judges agg runs on the filesystem → met / in-progress / REGRESSED, invariants checked"),
        ("GATE", RED, "rollback gate", "a regression on the staged merge → abort it, base stays pristine, branch kept; else commit it"),
        ("GATE", PURPLE, "fold memory + summarize", "enforced entry into AGG_MEMORY.md (even if the worker wrote nothing) + a 1-line progress summary"),
    ];

    // ── geometry (all derived, so the card always contains its contents) ──
    let sh = 58.0;          // stage box height
    let gap = 18.0;         // vertical gap between stages
    let gutter = 96.0;      // right gutter inside the card for the loop-back arrow + its label
    let sx = lx + 84.0;     // stage x (left margin holds the INJECT/RUN/VERIFY/GATE band labels)
    let sw = 940.0;         // stage width
    let hdr = 56.0;         // card header height
    let n_boxes = steps.len() as f32 + 1.0; // stages + the check box
    let content_h = n_boxes * sh + (n_boxes - 1.0) * gap;
    let lw = (sx - lx) + sw + gutter;                 // card width contains stages + gutter
    let lh = hdr + content_h + 34.0;                  // + bottom padding

    s.rect(lx, ly, lw, lh, "#0f141b", CYAN);
    s.text(lx + 20.0, ly + 34.0, "agg run", CYAN, 18.0, "700", "start", true);
    s.text(lx + 118.0, ly + 34.0, "— the DETERMINISTIC outer loop (plain code, no model in the control path)", MUTED, 13.5, "400", "start", false);

    // stages top→bottom, with a left-margin band label whenever the outer stage changes.
    let mut y = ly + hdr;
    let mut prev_stage = "";
    for (st, accent, title, sub) in steps {
        if *st != prev_stage {
            // stage band label to the LEFT of the boxes (inside the card's left padding)
            let c = if *st == "RUN" { AMBER } else { CYAN };
            s.text(lx + 16.0, y + 20.0, st, c, 12.0, "700", "start", true);
            if *st == "RUN" {
                s.text(lx + 16.0, y + 36.0, "stochastic", AMBER, 10.0, "600", "start", false);
            }
            prev_stage = st;
        }
        stage(&mut s, sx, y, sw, sh, accent, title, sub);
        let ny = y + sh;
        s.line(sx + sw / 2.0, ny, sx + sw / 2.0, ny + gap, MUTED, true, false);
        y = ny + gap;
    }
    // check stop/halt (last box in the loop)
    stage(&mut s, sx, y, sw, sh, CYAN, "check STOP / HALT",
        "all_goals?  OR  over_budget / over_cost / over_iterations / wall_hours?  OR  an invariant regressed?");
    let check_mid_y = y + sh / 2.0;
    let check_bottom = y + sh;

    // loop-back arrow: right edge of the check box → up the FAR side of the gutter → into the
    // right edge of the first stage. Routed near the card edge so the label has clear space to
    // its left, inside the gutter.
    let back_x = lx + lw - 22.0;
    let top_stage_mid = ly + hdr + sh / 2.0;
    s.path(
        &format!("M {} {} H {} V {} H {}", sx + sw, check_mid_y, back_x, top_stage_mid, sx + sw),
        AMBER, true,
    );
    // label the loop-back, centered vertically in the gutter, LEFT of the arrow line (no overlap).
    let mid_y = (top_stage_mid + check_mid_y) / 2.0;
    s.text(back_x - 20.0, mid_y - 6.0, "not yet", AMBER, 13.0, "600", "end", false);
    s.text(back_x - 20.0, mid_y + 12.0, "↻ repeat", AMBER, 13.0, "600", "end", false);

    // exit arrow: bottom of the check box → the "loop stops" label below the card.
    s.line(sx + sw / 2.0, check_bottom, sx + sw / 2.0, ly + lh - 6.0, GREEN, true, false);
    s.text(lx + lw / 2.0, ly + lh + 30.0, "goals met (or a guard HALTs) → the loop stops, base is clean",
        GREEN, 16.0, "600", "middle", false);

    // ── right column: what you watch/steer it with ──
    let rw = 300.0;
    let rx = W - rw - 24.0;
    let ry = ly + 70.0;
    let rgap = 96.0;
    stage(&mut s, rx, ry, rw, 64.0, CYAN, "agg dashboard", "live TUI — goals, usage, activity, memory");
    stage(&mut s, rx, ry + rgap, rw, 64.0, PURPLE, "agg send / stop", "steer or stop it mid-run, from anywhere");
    stage(&mut s, rx, ry + 2.0 * rgap, rw, 64.0, GREEN, "agg spawn", "track long side-tasks across sessions");
    // dashed connectors from the card's right edge to the column
    s.line(lx + lw, ry + 32.0, rx, ry + 32.0, MUTED, false, true);
    s.line(lx + lw, ry + rgap + 32.0, rx, ry + rgap + 32.0, MUTED, false, true);

    s.0.push_str("</svg>\n");
    print!("{}", s.0);
}

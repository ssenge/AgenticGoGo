//! Render the four-stage loop to an SVG for the README.
//!
//! Deliberately simple: four nodes on a circle, arrows clockwise, closing back to INJECT.
//! RUN is filled — it is the one stochastic step (the model). The other three are outlined:
//! plain, deterministic code. No sub-boxes, no branch arrows; the detail belongs in the docs.
//!
//! Drawn programmatically rather than hand-written so it can't drift from the real
//! `src/loop_.rs` stage names.
//!
//! Usage:  cargo run --example loop_svg > assets/loop.svg
//!         rsvg-convert -z 2 -o assets/loop.png assets/loop.svg

const W: f64 = 720.0;
const H: f64 = 470.0;
const CX: f64 = 360.0;
const CY: f64 = 212.0;
const R: f64 = 150.0; // radius of the node centres

const BG: &str = "#0d1117";
const INK: &str = "#c9d1d9";
const DIM: &str = "#8b949e";
const ACCENT: &str = "#39c5cf";
const RUN_FILL: &str = "#1f6feb";

/// (label, sub-label, angle in degrees; 0° = top, clockwise)
const STAGES: [(&str, &str, f64); 4] = [
    ("INJECT", "state + steering → prompt", 0.0),
    ("RUN", "Claude Code — one fresh `claude -p` session", 90.0),
    ("VERIFY", "judges run against the filesystem", 180.0),
    ("GATE", "keep or roll back · repeat until stop_when", 270.0),
];

fn pos(angle_deg: f64, radius: f64) -> (f64, f64) {
    let a = (angle_deg - 90.0).to_radians(); // 0° = top
    (CX + radius * a.cos(), CY + radius * a.sin())
}

fn main() {
    let mut s = String::new();
    s.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{W:.0}\" height=\"{H:.0}\" \
         viewBox=\"0 0 {W:.0} {H:.0}\" font-family=\"SFMono-Regular,Menlo,Consolas,monospace\">\n"
    ));
    s.push_str(&format!("<rect width=\"{W:.0}\" height=\"{H:.0}\" rx=\"10\" fill=\"{BG}\"/>\n"));

    // arrowhead
    s.push_str(&format!(
        "<defs><marker id=\"a\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" markerWidth=\"5\" \
         markerHeight=\"5\" orient=\"auto-start-reverse\">\
         <path d=\"M0,0 L10,5 L0,10 z\" fill=\"{DIM}\"/></marker></defs>\n"
    ));

    // the four connecting arcs, drawn between node edges so the arrowhead lands cleanly
    let gap = 26.0_f64; // degrees of clearance either side of a node
    for i in 0..4 {
        let from = STAGES[i].2 + gap;
        let to = STAGES[(i + 1) % 4].2 - gap;
        let (x1, y1) = pos(from, R);
        let (x2, y2) = pos(to, R);
        s.push_str(&format!(
            "<path d=\"M{x1:.1},{y1:.1} A{R:.0},{R:.0} 0 0 1 {x2:.1},{y2:.1}\" fill=\"none\" \
             stroke=\"{DIM}\" stroke-width=\"1.6\" marker-end=\"url(#a)\"/>\n"
        ));
    }

    // nodes
    for (label, sub, angle) in STAGES {
        let (x, y) = pos(angle, R);
        let stochastic = label == "RUN";
        let (fill, stroke, text) = if stochastic {
            (RUN_FILL, RUN_FILL, "#ffffff")
        } else {
            (BG, ACCENT, INK)
        };
        let (bw, bh) = (150.0, 44.0);
        s.push_str(&format!(
            "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{bw}\" height=\"{bh}\" rx=\"8\" \
             fill=\"{fill}\" stroke=\"{stroke}\" stroke-width=\"1.6\"/>\n",
            x - bw / 2.0,
            y - bh / 2.0
        ));
        s.push_str(&format!(
            "<text x=\"{x:.1}\" y=\"{:.1}\" fill=\"{text}\" font-size=\"15\" font-weight=\"bold\" \
             text-anchor=\"middle\">{label}</text>\n",
            y + 5.0
        ));
        // sub-label directly beneath its box — placing it radially put the side labels
        // inside the boxes.
        s.push_str(&format!(
            "<text x=\"{x:.1}\" y=\"{:.1}\" fill=\"{DIM}\" font-size=\"11\" \
             text-anchor=\"middle\">{sub}</text>\n",
            y + bh / 2.0 + 16.0
        ));
    }

    // legend
    let ly = H - 20.0;
    s.push_str(&format!(
        "<rect x=\"186\" y=\"{:.0}\" width=\"12\" height=\"12\" rx=\"3\" fill=\"{RUN_FILL}\"/>\n",
        ly - 10.0
    ));
    s.push_str(&format!(
        "<text x=\"204\" y=\"{ly:.0}\" fill=\"{DIM}\" font-size=\"11\">stochastic — the model</text>\n"
    ));
    s.push_str(&format!(
        "<rect x=\"382\" y=\"{:.0}\" width=\"12\" height=\"12\" rx=\"3\" fill=\"{BG}\" stroke=\"{ACCENT}\" stroke-width=\"1.4\"/>\n",
        ly - 10.0
    ));
    s.push_str(&format!(
        "<text x=\"400\" y=\"{ly:.0}\" fill=\"{DIM}\" font-size=\"11\">deterministic — plain code</text>\n"
    ));

    s.push_str("</svg>\n");
    print!("{s}");
}

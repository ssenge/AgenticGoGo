//! Render the REAL `agg dashboard` TUI to an SVG, for the README screenshot.
//!
//! It drives the actual `dashboard::draw()` path via a headless ratatui `TestBackend` (the same
//! render the live TUI uses), then walks the resulting cell buffer — symbol + fg/bg color per
//! cell — into a self-contained SVG with a monospace grid. Because it uses the production render,
//! the image cannot drift from what `agg dashboard` actually shows.
//!
//! Usage:  cargo run --example dashboard_svg > assets/dashboard.svg
//! Then rasterize to PNG (crisper in GitHub) with e.g. `magick -density 200 dashboard.svg dashboard.png`.

use agg::dashboard;
use agg::state::DashboardState;
use ratatui::style::Color;

const COLS: u16 = 92;
const ROWS: u16 = 34;
// cell metrics (px) — tuned so box-drawing lines join cleanly.
const CW: f32 = 8.4;
const CH: f32 = 17.0;
const PAD: f32 = 16.0;
const FONT: f32 = 13.5;

// A calm dark "terminal" theme. ratatui's named colors map onto these so the SVG matches a
// typical dark terminal rendering of the dashboard.
const BG: &str = "#0d1117"; // terminal background (GitHub-dark-ish)
const FG: &str = "#c9d1d9"; // default foreground

fn color_hex(c: Color, default: &str) -> String {
    match c {
        Color::Reset => default.to_string(),
        Color::Black => "#484f58".into(),
        Color::Red => "#ff7b72".into(),
        Color::Green => "#3fb950".into(),
        Color::Yellow => "#d29922".into(),
        Color::Blue => "#58a6ff".into(),
        Color::Magenta => "#bc8cff".into(),
        Color::Cyan => "#39c5cf".into(),
        Color::Gray => "#8b949e".into(),
        Color::DarkGray => "#6e7681".into(),
        Color::LightRed => "#ffa198".into(),
        Color::LightGreen => "#56d364".into(),
        Color::LightYellow => "#e3b341".into(),
        Color::LightBlue => "#79c0ff".into(),
        Color::LightMagenta => "#d2a8ff".into(),
        Color::LightCyan => "#56d4dd".into(),
        Color::White => "#f0f6fc".into(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(_) => default.to_string(),
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn main() {
    let state: DashboardState = dashboard::sample_state();
    let buf = dashboard::render_buffer(&state, COLS, ROWS);
    let area = *buf.area();

    let w = PAD * 2.0 + CW * COLS as f32;
    let h = PAD * 2.0 + CH * ROWS as f32;

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w:.0}\" height=\"{h:.0}\" \
         viewBox=\"0 0 {w:.0} {h:.0}\" font-family=\"SFMono-Regular,Menlo,Consolas,monospace\" \
         font-size=\"{FONT}\">\n"
    ));
    // rounded terminal chrome
    svg.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{w:.0}\" height=\"{h:.0}\" rx=\"10\" fill=\"{BG}\"/>\n"
    ));

    // First pass: background rects for any cell whose bg isn't the default (e.g. reversed rows).
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            let bg = color_hex(cell.bg, BG);
            if bg != BG {
                let px = PAD + x as f32 * CW;
                let py = PAD + y as f32 * CH;
                svg.push_str(&format!(
                    "<rect x=\"{px:.2}\" y=\"{py:.2}\" width=\"{CW:.2}\" height=\"{CH:.2}\" fill=\"{bg}\"/>\n"
                ));
            }
        }
    }

    // Second pass: glyphs, one <text> run per row grouping same-color cells to keep the file small.
    for y in 0..area.height {
        let baseline = PAD + y as f32 * CH + CH * 0.72;
        let mut run = String::new();
        let mut run_fg = String::new();
        let mut run_x0 = 0.0f32;
        let flush = |svg: &mut String, run: &str, fg: &str, x0: f32| {
            if run.trim().is_empty() {
                return;
            }
            svg.push_str(&format!(
                "<text x=\"{x0:.2}\" y=\"{baseline:.2}\" fill=\"{fg}\" xml:space=\"preserve\">{}</text>\n",
                xml_escape(run)
            ));
        };
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            let sym = cell.symbol();
            let fg = color_hex(cell.fg, FG);
            let px = PAD + x as f32 * CW;
            if fg != run_fg && !run.is_empty() {
                flush(&mut svg, &run, &run_fg, run_x0);
                run.clear();
            }
            if run.is_empty() {
                run_x0 = px;
                run_fg = fg;
            }
            run.push_str(sym);
        }
        flush(&mut svg, &run, &run_fg, run_x0);
    }

    svg.push_str("</svg>\n");
    print!("{svg}");
}

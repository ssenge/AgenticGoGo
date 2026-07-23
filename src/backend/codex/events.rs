//! Stream-event constructors — turn a scrap of text into a [`stream::Event`] for the TUI.
//!
//! One helper per glyph (💬 think, 🔧 tool, ↳ tool-result, ✅ result), plus `clean` to collapse
//! whitespace before display. All are `pub(super)` so the parent's `parse_event` can call them.

use super::stream;
use crate::util::truncate;

pub(super) fn think(text: String) -> stream::Event {
    stream::Event {
        display: format!("💬 {}", truncate(&text, 200)),
        kind: stream::EventKind::Think,
        text: truncate(&text, 200),
        is_result: false,
        thought: Some(text),
    }
}
pub(super) fn tool(text: String) -> stream::Event {
    stream::Event {
        display: format!("🔧 {}", truncate(&text, 200)),
        kind: stream::EventKind::Tool,
        text: truncate(&text, 200),
        is_result: false,
        thought: None,
    }
}
pub(super) fn tool_result(text: String) -> stream::Event {
    stream::Event {
        display: format!("↳ {}", truncate(&text, 200)),
        kind: stream::EventKind::ToolResult,
        text: truncate(&text, 200),
        is_result: false,
        thought: None,
    }
}
pub(super) fn result_event(text: String) -> stream::Event {
    stream::Event {
        display: format!("✅ {text}"),
        kind: stream::EventKind::Result,
        text,
        is_result: true,
        thought: None,
    }
}

pub(super) fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

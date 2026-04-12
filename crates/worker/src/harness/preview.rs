//! Shared preview helpers for worker-facing stream, trace, and task outputs.

/// Default truncation length for user-visible previews.
pub const PREVIEW_MAX_CHARS: usize = 1000;

/// Shorter truncation used for debug log summaries.
pub const DEBUG_PREVIEW_MAX_CHARS: usize = 80;

pub fn truncate_preview(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => format!("{}...", &s[..idx]),
        None => s.to_string(),
    }
}

/// Return the first substantive preview line from a block of text.
///
/// This intentionally skips blank lines and bracket-only pretty-printed JSON
/// lines such as `[` and `{`, which are low-signal when rendered as previews.
pub fn summarize_preview(text: &str) -> Option<String> {
    for line in text.lines().map(str::trim) {
        if line.is_empty() || matches!(line, "[" | "]" | "{" | "}" | "," | "[," | "],") {
            continue;
        }

        return Some(truncate_preview(line, PREVIEW_MAX_CHARS));
    }

    None
}

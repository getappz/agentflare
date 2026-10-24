//! Side-by-side anchored diff rendering (port of baz-cli `DiffDisplay`).
//!
//! Shows the ±[`MAX_ADDED_VIEW_LINES`] window around an anchor line, clipped to
//! each hunk's bounds, with selection highlighted on the correct diff side.

use super::diff::{Chunk, DiffLine, FileDiff, LineKind, parse_unified_diff};
use crate::github::models::PullRequestFile;

/// Context lines above/below the anchor (baz `MAX_ADDED_VIEW_LINES`).
pub const MAX_ADDED_VIEW_LINES: u32 = 3;

/// Which side of the diff the anchor sits on (GitHub `diffSide` / baz `side`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorSide {
    Left,
    Right,
}

/// A line or range on one side of the diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub line_start: u32,
    pub line_end: u32,
    pub side: AnchorSide,
}

impl Anchor {
    pub fn single_line(line: u32, side: AnchorSide) -> Self {
        Self {
            line_start: line,
            line_end: line,
            side,
        }
    }
}

/// Visible line range around an anchor (not yet clipped to a hunk).
pub fn view_window(anchor: &Anchor) -> (u32, u32) {
    let start = anchor.line_start.saturating_sub(MAX_ADDED_VIEW_LINES);
    let end = anchor.line_end + MAX_ADDED_VIEW_LINES;
    (start, end)
}

/// Parses PR file patches into structured [`FileDiff`]s.
pub fn file_diffs_from_pull_files(files: &[PullRequestFile]) -> Vec<FileDiff> {
    let mut out = Vec::new();
    for f in files {
        if let Some(patch) = &f.patch {
            let wrapped = wrap_single_file_patch(&f.filename, patch);
            out.extend(parse_unified_diff(&wrapped));
        }
    }
    out
}

fn wrap_single_file_patch(path: &str, patch: &str) -> String {
    format!(
        "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n{patch}",
        path = path
    )
}

fn find_file<'a>(files: &'a [FileDiff], path: &str) -> Option<&'a FileDiff> {
    files.iter().find(|f| f.path().is_some_and(|p| p == path))
}

/// Renders the anchored hunk as plain text (markdown / HTML `<pre>`).
pub fn render_anchored_plain(files: &[FileDiff], path: &str, anchor: &Anchor) -> String {
    let Some(file) = find_file(files, path) else {
        return no_context_message(path);
    };
    render_file_plain(file, anchor)
}

/// Renders the anchored hunk with ANSI colours for terminal output.
pub fn render_anchored_ansi(files: &[FileDiff], path: &str, anchor: &Anchor) -> String {
    let Some(file) = find_file(files, path) else {
        return no_context_message(path);
    };
    render_file_ansi(file, anchor)
}

pub fn no_context_message(path: &str) -> String {
    format!("no context for {path}")
}

/// First changed line in `files` (PR file list), for summary cards.
pub fn first_change_summary(files: &[PullRequestFile]) -> Option<String> {
    let parsed = file_diffs_from_pull_files(files);
    for file in &parsed {
        if let (Some(path), Some(anchor)) = (file.path(), first_anchor_in_file(file)) {
            return Some(render_anchored_plain(&parsed, path, &anchor));
        }
    }
    None
}

fn first_anchor_in_file(file: &FileDiff) -> Option<Anchor> {
    for chunk in &file.chunks {
        for line in &chunk.lines {
            match line.kind {
                LineKind::Added if line.new_line.is_some() => {
                    return Some(Anchor::single_line(
                        line.new_line.unwrap(),
                        AnchorSide::Right,
                    ));
                }
                LineKind::Deleted if line.old_line.is_some() => {
                    return Some(Anchor::single_line(
                        line.old_line.unwrap(),
                        AnchorSide::Left,
                    ));
                }
                LineKind::Changed if line.new_line.is_some() => {
                    return Some(Anchor::single_line(
                        line.new_line.unwrap(),
                        AnchorSide::Right,
                    ));
                }
                _ => {}
            }
        }
    }
    None
}

// --- plain --------------------------------------------------------------------

fn render_file_plain(file: &FileDiff, anchor: &Anchor) -> String {
    let (view_start, view_end) = view_window(anchor);
    let mut out = String::new();
    out.push_str(&format!("☰ {}\n", file.path().unwrap_or("?")));
    for chunk in &file.chunks {
        let (clip_start, clip_end) = clip_window_to_chunk(chunk, anchor, view_start, view_end);
        if clip_start > clip_end {
            continue;
        }
        for row in rows_for_chunk(chunk, anchor, clip_start, clip_end) {
            out.push_str(&format_plain_row(&row));
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

fn format_plain_row(row: &DisplayRow) -> String {
    let left = format_side_plain(
        row.left_num,
        row.left_prefix,
        &row.left_text,
        row.left_selected,
    );
    let right = format_side_plain(
        row.right_num,
        row.right_prefix,
        &row.right_text,
        row.right_selected,
    );
    format!("{left} │ {right}")
}

fn format_side_plain(num: Option<u32>, prefix: char, text: &str, selected: bool) -> String {
    let gutter = match num {
        Some(n) => format!("{:>5}", n),
        None => "     ".to_string(),
    };
    let p = if prefix == ' ' {
        "   ".to_string()
    } else {
        format!(" {prefix} ")
    };
    let mark = if selected { "▸" } else { " " };
    format!("{mark}{gutter}{p}{text}")
}

// --- ANSI ---------------------------------------------------------------------

#[derive(Clone, Copy)]
struct AnsiStyle {
    fg: u8,
    bg: u8,
    bold: bool,
}

impl AnsiStyle {
    fn pair(fg: u8, bg: u8) -> Self {
        Self {
            fg,
            bg,
            bold: false,
        }
    }
}

const STYLE_CONTEXT: AnsiStyle = AnsiStyle {
    fg: 252,
    bg: 0,
    bold: false,
};
const STYLE_GUTTER: AnsiStyle = AnsiStyle {
    fg: 244,
    bg: 0,
    bold: false,
};
const STYLE_ADDED: AnsiStyle = AnsiStyle {
    fg: 255,
    bg: 22,
    bold: false,
};
const STYLE_DELETED: AnsiStyle = AnsiStyle {
    fg: 255,
    bg: 52,
    bold: false,
};
const STYLE_SELECTED: AnsiStyle = AnsiStyle {
    fg: 255,
    bg: 24,
    bold: false,
};

fn render_file_ansi(file: &FileDiff, anchor: &Anchor) -> String {
    let (view_start, view_end) = view_window(anchor);
    let mut out = String::new();
    let header = format!("☰ {}", file.path().unwrap_or("?"));
    push_ansi_span(&mut out, &AnsiStyle::pair(255, 236), &header);
    out.push('\n');
    for chunk in &file.chunks {
        let (clip_start, clip_end) = clip_window_to_chunk(chunk, anchor, view_start, view_end);
        if clip_start > clip_end {
            continue;
        }
        for row in rows_for_chunk(chunk, anchor, clip_start, clip_end) {
            format_ansi_row(&mut out, &row);
            out.push('\n');
        }
    }
    let trimmed = out.trim_end().to_string();
    debug_assert!(
        ansi_backgrounds_have_foreground(&trimmed),
        "anchored diff ansi missing foreground on a background span"
    );
    trimmed
}

fn format_ansi_row(out: &mut String, row: &DisplayRow) {
    format_ansi_side(
        out,
        row.left_num,
        row.left_prefix,
        &row.left_text,
        row.left_style,
        row.left_selected,
    );
    push_ansi_span(out, &STYLE_CONTEXT, " │ ");
    format_ansi_side(
        out,
        row.right_num,
        row.right_prefix,
        &row.right_text,
        row.right_style,
        row.right_selected,
    );
}

fn format_ansi_side(
    out: &mut String,
    num: Option<u32>,
    prefix: char,
    text: &str,
    line_style: LineStyle,
    selected: bool,
) {
    let style = if selected {
        STYLE_SELECTED
    } else {
        match line_style {
            LineStyle::Context => STYLE_CONTEXT,
            LineStyle::Added => STYLE_ADDED,
            LineStyle::Deleted => STYLE_DELETED,
        }
    };
    let gutter_style = if style.bg != 0 { style } else { STYLE_GUTTER };
    let gutter = match num {
        Some(n) => format!("{:>5}", n),
        None => "     ".to_string(),
    };
    let p = if prefix == ' ' {
        "   ".to_string()
    } else {
        format!(" {prefix} ")
    };
    push_ansi_span(out, &gutter_style, &gutter);
    push_ansi_span(out, &gutter_style, &p);
    push_ansi_span(out, &style, text);
}

/// Foreground and background are always emitted together when a background is set.
fn push_ansi_span(out: &mut String, style: &AnsiStyle, text: &str) {
    if style.bg != 0 {
        out.push_str(&format!("\x1b[38;5;{}m\x1b[48;5;{}m", style.fg, style.bg));
    } else if style.fg != 0 {
        out.push_str(&format!("\x1b[38;5;{}m", style.fg));
    }
    if style.bold {
        out.push_str("\x1b[1m");
    }
    out.push_str(text);
    out.push_str("\x1b[0m");
}

// --- row building -------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineStyle {
    Context,
    Added,
    Deleted,
}

struct DisplayRow {
    left_num: Option<u32>,
    left_prefix: char,
    left_text: String,
    left_style: LineStyle,
    left_selected: bool,
    right_num: Option<u32>,
    right_prefix: char,
    right_text: String,
    right_style: LineStyle,
    right_selected: bool,
}

fn clip_window_to_chunk(
    chunk: &Chunk,
    anchor: &Anchor,
    view_start: u32,
    view_end: u32,
) -> (u32, u32) {
    let (mut lo, mut hi) = (view_start, view_end);
    let mut chunk_lo = u32::MAX;
    let mut chunk_hi = 0u32;
    for line in &chunk.lines {
        let n = anchor_line_number(line, anchor.side, false);
        if let Some(n) = n {
            chunk_lo = chunk_lo.min(n);
            chunk_hi = chunk_hi.max(n);
        }
        if let Some(n) = line.new_line {
            chunk_lo = chunk_lo.min(n);
            chunk_hi = chunk_hi.max(n);
        }
    }
    if chunk_lo == u32::MAX {
        return (view_start, view_end);
    }
    lo = lo.max(chunk_lo);
    hi = hi.min(chunk_hi);
    (lo, hi)
}

fn rows_for_chunk(
    chunk: &Chunk,
    anchor: &Anchor,
    clip_start: u32,
    clip_end: u32,
) -> Vec<DisplayRow> {
    let before_len = chunk.before_context().len();
    let after_start = chunk.lines.len() - chunk.after_context().len();
    let mut rows = Vec::new();
    for (i, line) in chunk.lines.iter().enumerate() {
        let in_context = i < before_len || i >= after_start;
        if !line_in_window(line, anchor, clip_start, clip_end, in_context) {
            continue;
        }
        rows.push(display_row_for_line(line, anchor));
    }
    rows
}

fn line_in_window(
    line: &DiffLine,
    anchor: &Anchor,
    view_start: u32,
    view_end: u32,
    in_context_run: bool,
) -> bool {
    let n = if in_context_run {
        line.new_line.unwrap_or(0)
    } else {
        anchor_line_number(line, anchor.side, false).unwrap_or(0)
    };
    if n == 0 {
        return false;
    }
    n >= view_start && n <= view_end
}

fn anchor_line_number(line: &DiffLine, side: AnchorSide, _in_context: bool) -> Option<u32> {
    match side {
        AnchorSide::Left => line.old_line,
        AnchorSide::Right => line.new_line,
    }
}

fn display_row_for_line(line: &DiffLine, anchor: &Anchor) -> DisplayRow {
    let (mut left_prefix, mut right_prefix) = (' ', ' ');
    let (mut left_style, mut right_style) = (LineStyle::Context, LineStyle::Context);
    let (left_text, right_text) = match line.kind {
        LineKind::Unchanged => {
            let t = line
                .old_text
                .as_deref()
                .or(line.new_text.as_deref())
                .unwrap_or("");
            (t.to_string(), t.to_string())
        }
        LineKind::Added => {
            right_prefix = '+';
            right_style = LineStyle::Added;
            (String::new(), line.new_text.clone().unwrap_or_default())
        }
        LineKind::Deleted => {
            left_prefix = '-';
            left_style = LineStyle::Deleted;
            (line.old_text.clone().unwrap_or_default(), String::new())
        }
        LineKind::Changed => {
            left_prefix = '-';
            right_prefix = '+';
            left_style = LineStyle::Deleted;
            right_style = LineStyle::Added;
            (
                line.old_text.clone().unwrap_or_default(),
                line.new_text.clone().unwrap_or_default(),
            )
        }
    };
    let left_selected = anchor.side == AnchorSide::Left
        && line
            .old_line
            .is_some_and(|n| n >= anchor.line_start && n <= anchor.line_end);
    let right_selected = anchor.side == AnchorSide::Right
        && line
            .new_line
            .is_some_and(|n| n >= anchor.line_start && n <= anchor.line_end);
    DisplayRow {
        left_num: line.old_line,
        left_prefix,
        left_text,
        left_style,
        left_selected,
        right_num: line.new_line,
        right_prefix,
        right_text,
        right_style,
        right_selected,
    }
}

/// Every background SGR in `s` is preceded by a foreground SGR in the same span.
pub fn ansi_backgrounds_have_foreground(s: &str) -> bool {
    for chunk in s.split("\x1b[0m") {
        if chunk.contains("\x1b[48;5;") && !chunk.contains("\x1b[38;5;") {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::diff::parse_unified_diff;

    fn sample_diff() -> String {
        "\
diff --git a/src/f.rs b/src/f.rs
--- a/src/f.rs
+++ b/src/f.rs
@@ -1,10 +1,10 @@
 line1
 line2
 line3
-old4
+new4
 line5
 line6
 line7
 line8
 line9
 line10
"
        .to_string()
    }

    fn files(diff: &str) -> Vec<FileDiff> {
        parse_unified_diff(diff)
    }

    #[test]
    fn view_window_is_plus_minus_three_and_does_not_underflow_at_line_one() {
        let anchor = Anchor::single_line(1, AnchorSide::Right);
        assert_eq!(view_window(&anchor), (0, 4));
        let anchor = Anchor {
            line_start: 1,
            line_end: 2,
            side: AnchorSide::Right,
        };
        assert_eq!(view_window(&anchor), (0, 5));
    }

    #[test]
    fn window_is_clipped_to_the_hunk_not_beyond_it() {
        let diff = files(&sample_diff());
        let anchor = Anchor::single_line(4, AnchorSide::Right);
        let plain = render_anchored_plain(&diff, "src/f.rs", &anchor);
        assert!(plain.contains("line3"));
        assert!(plain.contains("new4"));
        assert!(plain.contains("line5"));
        assert!(
            !plain.contains("line9"),
            "±3 from line 4 should not reach line 9"
        );
    }

    #[test]
    fn left_side_anchor_highlights_left_column_only() {
        let diff = files(&sample_diff());
        let anchor = Anchor::single_line(4, AnchorSide::Left);
        let plain = render_anchored_plain(&diff, "src/f.rs", &anchor);
        assert!(plain.contains("old4"));
        let lines: Vec<&str> = plain.lines().collect();
        let change_row = lines
            .iter()
            .find(|l| l.contains("old4"))
            .expect("deleted line visible");
        assert!(
            change_row.starts_with("▸"),
            "left anchor should mark the left side: {change_row}"
        );
        assert!(
            !change_row.contains("│ ▸"),
            "right side should not be marked selected: {change_row}"
        );
    }

    #[test]
    fn missing_file_renders_no_context() {
        let diff = files(&sample_diff());
        let anchor = Anchor::single_line(1, AnchorSide::Right);
        let plain = render_anchored_plain(&diff, "src/missing.rs", &anchor);
        assert_eq!(plain, "no context for src/missing.rs");
    }

    #[test]
    fn ansi_output_pairs_foreground_with_background() {
        let diff = files(&sample_diff());
        let anchor = Anchor::single_line(4, AnchorSide::Right);
        let ansi = render_anchored_ansi(&diff, "src/f.rs", &anchor);
        assert!(
            ansi.contains("\x1b[48;5;"),
            "expected highlighted rows: {ansi}"
        );
        assert!(
            ansi_backgrounds_have_foreground(&ansi),
            "background without foreground regressed: {ansi}"
        );
        for chunk in ansi.split("\x1b[0m") {
            if chunk.contains("\x1b[48;5;") && !chunk.contains("\x1b[38;5;") {
                panic!("span missing foreground: {chunk:?}");
            }
        }
    }

    #[test]
    fn file_diffs_from_pull_files_wraps_patches() {
        let files = vec![PullRequestFile {
            filename: "x.rs".into(),
            previous_filename: None,
            status: Some("modified".into()),
            patch: Some("@@ -1,1 +1,2 @@\n a\n+b\n".into()),
        }];
        let parsed = file_diffs_from_pull_files(&files);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].path(), Some("x.rs"));
    }
}

//! Structured unified-diff parsing: files → hunks → typed lines.
//!
//! [`super::changed_lines`] collapses a diff to the new-side line numbers it
//! touches, which is enough to *verify* a finding against a diff but not to
//! *show* one: it loses the old side, the text, and the position of every
//! line within its hunk. This module keeps that whole shape, so a comment can
//! be anchored on either side of a change and rendered next to the code it
//! talks about.
//!
//! The old and the new line number of a line are kept **separately**, never
//! collapsed into one: a deleted line has an old number and no new one, an
//! added line the reverse, and a changed line both.

/// What a diff line did to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Present on both sides, unchanged.
    Unchanged,
    /// Present only on the new side.
    Added,
    /// Present only on the old side.
    Deleted,
    /// A deletion and an addition paired up — the two halves of one edit.
    Changed,
}

/// One line of a hunk, with its position on each side of the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// 1-based line number on the old side, when the line exists there.
    pub old_line: Option<u32>,
    /// 1-based line number on the new side, when the line exists there.
    pub new_line: Option<u32>,
    /// The line's text on the old side, when it exists there.
    pub old_text: Option<String>,
    /// The line's text on the new side, when it exists there.
    pub new_text: Option<String>,
}

impl DiffLine {
    fn unchanged(old_line: u32, new_line: u32, text: &str) -> Self {
        Self {
            kind: LineKind::Unchanged,
            old_line: Some(old_line),
            new_line: Some(new_line),
            old_text: Some(text.to_string()),
            new_text: Some(text.to_string()),
        }
    }

    fn added(new_line: u32, text: &str) -> Self {
        Self {
            kind: LineKind::Added,
            old_line: None,
            new_line: Some(new_line),
            old_text: None,
            new_text: Some(text.to_string()),
        }
    }

    fn deleted(old_line: u32, text: &str) -> Self {
        Self {
            kind: LineKind::Deleted,
            old_line: Some(old_line),
            new_line: None,
            old_text: Some(text.to_string()),
            new_text: None,
        }
    }
}

/// One hunk (`@@ … @@`) of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// First old-side line of the hunk.
    pub old_start: u32,
    /// Number of old-side lines the hunk claims.
    pub old_len: u32,
    /// First new-side line of the hunk.
    pub new_start: u32,
    /// Number of new-side lines the hunk claims.
    pub new_len: u32,
    /// Every line of the hunk, in order.
    pub lines: Vec<DiffLine>,
}

impl Chunk {
    /// The lines that changed — everything that is not context.
    #[allow(dead_code)] // read by the anchored rendering (item #388)
    pub fn changed_lines(&self) -> impl Iterator<Item = &DiffLine> {
        self.lines.iter().filter(|l| l.kind != LineKind::Unchanged)
    }

    /// The unchanged run before the first change in this hunk.
    #[allow(dead_code)] // read by the anchored rendering (item #388)
    pub fn before_context(&self) -> &[DiffLine] {
        let end = self
            .lines
            .iter()
            .position(|l| l.kind != LineKind::Unchanged)
            .unwrap_or(self.lines.len());
        &self.lines[..end]
    }

    /// The unchanged run after the last change in this hunk.
    #[allow(dead_code)] // read by the anchored rendering (item #388)
    pub fn after_context(&self) -> &[DiffLine] {
        let start = self
            .lines
            .iter()
            .rposition(|l| l.kind != LineKind::Unchanged)
            .map(|i| i + 1)
            .unwrap_or(0);
        &self.lines[start..]
    }
}

/// One file's section of a diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// The path on the old side — `None` for a file that was added.
    pub old_path: Option<String>,
    /// The path on the new side — `None` for a file that was deleted.
    pub new_path: Option<String>,
    /// The file's hunks, in order.
    pub chunks: Vec<Chunk>,
}

impl FileDiff {
    /// The path this file is known by after the change — what a finding on it
    /// would cite.
    #[allow(dead_code)] // read by the anchored rendering (item #388)
    pub fn path(&self) -> Option<&str> {
        self.new_path.as_deref().or(self.old_path.as_deref())
    }
}

/// The path a file is known by in a diff: the new side when there is one.
pub fn file_key(file: &FileDiff) -> Option<&str> {
    file.new_path.as_deref()
}

/// Parses a unified diff into its files, hunks and typed lines.
///
/// The parser is deliberately forgiving: a malformed hunk header loses its
/// numbers, not the lines that follow it.
pub fn parse_unified_diff(diff: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut file: Option<FileState> = None;
    let mut chunk: Option<ChunkState> = None;

    for raw in diff.lines() {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            // A new file section. Everything after this until the next
            // `diff --git` belongs to it.
            if let Some(f) = file.take() {
                files.push(f.finish(chunk.take()));
            }
            file = Some(FileState::from_header(rest));
            continue;
        }

        // The `---`/`+++` pair names the file. Inside a hunk they are just
        // diff lines (a deleted line whose text starts with `-- ` looks
        // exactly like a header), so they are only read as headers while no
        // hunk is open.
        if chunk.is_none() {
            if let Some(rest) = raw.strip_prefix("--- ") {
                if let Some(f) = file.as_mut() {
                    f.old_path = header_path(rest, "a/");
                }
                continue;
            }
            if let Some(rest) = raw.strip_prefix("+++ ") {
                if let Some(f) = file.as_mut() {
                    f.new_path = header_path(rest, "b/");
                }
                continue;
            }
            if let Some(rest) = raw.strip_prefix("rename from ") {
                if let Some(f) = file.as_mut() {
                    f.old_path = Some(rest.trim().to_string());
                }
                continue;
            }
            if let Some(rest) = raw.strip_prefix("rename to ") {
                if let Some(f) = file.as_mut() {
                    f.new_path = Some(rest.trim().to_string());
                }
                continue;
            }
        }

        if let Some(rest) = raw.strip_prefix("@@") {
            // A new hunk closes the previous one.
            if let Some(f) = file.as_mut()
                && let Some(c) = chunk.take()
            {
                f.chunks.push(c.finish());
            }
            chunk = Some(ChunkState::new(parse_hunk_header(rest)));
            continue;
        }

        let Some(open) = chunk.as_mut() else {
            // Metadata before the first hunk (`index …`, `new file mode …`)
            // carries no lines.
            continue;
        };

        if raw.starts_with('\\') {
            // "\ No newline at end of file" — a marker, not a line.
            continue;
        }

        open.push(raw);
    }

    if let Some(f) = file.take() {
        files.push(f.finish(chunk.take()));
    }
    files
}

// --- parser internals ---------------------------------------------------------

/// The file section being collected.
struct FileState {
    old_path: Option<String>,
    new_path: Option<String>,
    chunks: Vec<Chunk>,
}

impl FileState {
    /// A new file section, seeded from the `diff --git a/x b/y` header. The
    /// `---`/`+++` pair is authoritative when it is present; this is the
    /// fallback for a pure rename, which carries no `---`/`+++` lines.
    fn from_header(rest: &str) -> Self {
        let (old, new) = split_header_paths(rest);
        Self {
            old_path: old,
            new_path: new,
            chunks: Vec::new(),
        }
    }

    fn finish(self, chunk: Option<ChunkState>) -> FileDiff {
        let mut chunks = self.chunks;
        if let Some(c) = chunk {
            chunks.push(c.finish());
        }
        FileDiff {
            old_path: self.old_path,
            new_path: self.new_path,
            chunks,
        }
    }
}

/// The hunk being collected.
struct ChunkState {
    old_start: u32,
    old_len: u32,
    new_start: u32,
    new_len: u32,
    /// The next old-side line number.
    old_next: u32,
    /// The next new-side line number.
    new_next: u32,
    /// Lines seen so far, before deletions and additions are paired up.
    entries: Vec<DiffLine>,
}

impl ChunkState {
    fn new(header: Option<(u32, u32, u32, u32)>) -> Self {
        let (old_start, old_len, new_start, new_len) = header.unwrap_or((0, 0, 0, 0));
        Self {
            old_start,
            old_len,
            new_start,
            new_len,
            old_next: old_start,
            new_next: new_start,
            entries: Vec::new(),
        }
    }

    /// Records one raw diff line, advancing the side counters.
    fn push(&mut self, raw: &str) {
        let mut chars = raw.chars();
        match chars.next() {
            Some('+') => {
                let text = &raw[1..];
                self.entries.push(DiffLine::added(self.new_next, text));
                self.new_next += 1;
            }
            Some('-') => {
                let text = &raw[1..];
                self.entries.push(DiffLine::deleted(self.old_next, text));
                self.old_next += 1;
            }
            Some('\\') => {}
            _ => {
                // A context line: a leading space, or a bare blank line.
                let text = raw.strip_prefix(' ').unwrap_or(raw);
                self.entries
                    .push(DiffLine::unchanged(self.old_next, self.new_next, text));
                self.old_next += 1;
                self.new_next += 1;
            }
        }
    }

    fn finish(mut self) -> Chunk {
        let lines = pair_up(std::mem::take(&mut self.entries));
        Chunk {
            old_start: self.old_start,
            old_len: self.old_len,
            new_start: self.new_start,
            new_len: self.new_len,
            lines,
        }
    }
}

/// Pairs each run of deletions with the run of additions that follows it, so
/// one edit shows as a single line carrying both the old and the new text.
/// Leftovers stay one-sided.
fn pair_up(entries: Vec<DiffLine>) -> Vec<DiffLine> {
    let mut out: Vec<DiffLine> = Vec::with_capacity(entries.len());
    let mut i = 0;
    while i < entries.len() {
        if entries[i].kind != LineKind::Deleted {
            out.push(entries[i].clone());
            i += 1;
            continue;
        }
        let del_start = i;
        while i < entries.len() && entries[i].kind == LineKind::Deleted {
            i += 1;
        }
        let del_end = i;
        let add_start = i;
        while i < entries.len() && entries[i].kind == LineKind::Added {
            i += 1;
        }
        let add_end = i;

        let dels = &entries[del_start..del_end];
        let adds = &entries[add_start..add_end];
        let pairs = dels.len().min(adds.len());

        for k in 0..pairs {
            out.push(DiffLine {
                kind: LineKind::Changed,
                old_line: dels[k].old_line,
                new_line: adds[k].new_line,
                old_text: dels[k].old_text.clone(),
                new_text: adds[k].new_text.clone(),
            });
        }
        out.extend(dels[pairs..].iter().cloned());
        out.extend(adds[pairs..].iter().cloned());
    }
    out
}

/// Parses ` -a,b +c,d @@` (the leading `@@` already stripped) into
/// `(old_start, old_len, new_start, new_len)`. A missing count means one.
fn parse_hunk_header(rest: &str) -> Option<(u32, u32, u32, u32)> {
    let rest = rest.trim_start();
    let old = rest.strip_prefix('-')?;
    let (old_start, old_len, used) = read_range(old)?;
    // The old range ends where the new one begins.
    let new = old[used..].trim_start().strip_prefix('+')?;
    let (new_start, new_len, _) = read_range(new)?;
    Some((old_start, old_len, new_start, new_len))
}

/// Reads `start` or `start,count` from the front of `s`, returning the pair
/// and how many bytes it consumed. A missing count means one.
fn read_range(s: &str) -> Option<(u32, u32, usize)> {
    let lead = s.len() - s.trim_start().len();
    let s = &s[lead..];
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let start = s[..end].parse::<u32>().ok()?;
    let mut count = 1;
    let mut consumed = end;
    if s[end..].starts_with(',') {
        let rest = &s[end + 1..];
        let count_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if count_end > 0 {
            count = rest[..count_end].parse::<u32>().ok()?;
            consumed = end + 1 + count_end;
        }
    }
    Some((start, count, lead + consumed))
}

/// The path a `---`/`+++` header names, with the `a/`/`b/` prefix removed.
/// `/dev/null` — a file that does not exist on that side — yields `None`.
fn header_path(rest: &str, prefix: &str) -> Option<String> {
    // Some tools append a timestamp after a tab; the path ends there.
    let path = rest.split('\t').next().unwrap_or(rest).trim();
    if path == "/dev/null" {
        return None;
    }
    Some(strip_prefix_once(path, prefix).to_string())
}

/// Splits `a/x b/y` into the two paths.
fn split_header_paths(rest: &str) -> (Option<String>, Option<String>) {
    let rest = rest.trim();
    // The two sides name the same path in the common case, so prefer the
    // split that makes them agree.
    for (idx, _) in rest.match_indices(" b/") {
        let left = &rest[..idx];
        let right = &rest[idx + 3..];
        if let Some(left) = left.strip_prefix("a/")
            && strip_prefix_once(left, "b/") == right
        {
            return (Some(left.to_string()), Some(right.to_string()));
        }
    }
    // Fall back to the last " b/", which is what a path containing " b/" needs.
    if let Some((left, right)) = rest.rsplit_once(" b/") {
        let left = left.strip_prefix("a/").unwrap_or(left);
        return (Some(left.to_string()), Some(right.to_string()));
    }
    (None, None)
}

/// Removes one occurrence of `prefix` from the start of `s`.
fn strip_prefix_once<'a>(s: &'a str, prefix: &str) -> &'a str {
    s.strip_prefix(prefix).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_changed_pair_becomes_one_line_with_both_sides() {
        let diff = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -10,2 +10,2 @@
-old line
+new line
";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        let chunk = &files[0].chunks[0];
        assert_eq!(chunk.lines.len(), 1);
        let line = &chunk.lines[0];
        assert_eq!(line.kind, LineKind::Changed);
        assert_eq!(line.old_line, Some(10));
        assert_eq!(line.new_line, Some(10));
        assert_eq!(line.old_text.as_deref(), Some("old line"));
        assert_eq!(line.new_text.as_deref(), Some("new line"));
    }

    #[test]
    fn each_kind_keeps_its_own_side_of_the_line_numbers() {
        let diff = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -10,3 +10,3 @@
 ctx
-old
+new
";
        let files = parse_unified_diff(diff);
        let chunk = &files[0].chunks[0];
        assert_eq!(chunk.lines.len(), 2);

        let ctx = &chunk.lines[0];
        assert_eq!(ctx.kind, LineKind::Unchanged);
        assert_eq!(ctx.old_line, Some(10));
        assert_eq!(ctx.new_line, Some(10));

        let changed = &chunk.lines[1];
        assert_eq!(changed.kind, LineKind::Changed);
        assert_eq!(changed.old_line, Some(11));
        assert_eq!(changed.new_line, Some(11));
    }

    #[test]
    fn an_added_line_has_a_new_number_and_no_old_one() {
        let diff = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -1,1 +1,2 @@
 keep
+added
";
        let files = parse_unified_diff(diff);
        let chunk = &files[0].chunks[0];
        let added = chunk
            .lines
            .iter()
            .find(|l| l.kind == LineKind::Added)
            .expect("an added line");
        assert_eq!(added.new_line, Some(2));
        assert_eq!(added.old_line, None);
        assert_eq!(added.new_text.as_deref(), Some("added"));
    }

    #[test]
    fn a_deleted_line_has_an_old_number_and_no_new_one() {
        let diff = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -1,2 +1,1 @@
 keep
-gone
";
        let files = parse_unified_diff(diff);
        let chunk = &files[0].chunks[0];
        let deleted = chunk
            .lines
            .iter()
            .find(|l| l.kind == LineKind::Deleted)
            .expect("a deleted line");
        assert_eq!(deleted.old_line, Some(2));
        assert_eq!(deleted.new_line, None);
        assert_eq!(deleted.old_text.as_deref(), Some("gone"));
    }

    #[test]
    fn uneven_runs_leave_the_extras_one_sided() {
        let diff = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -1,1 +1,3 @@
-one
+two
+three
";
        let files = parse_unified_diff(diff);
        let chunk = &files[0].chunks[0];
        let kinds: Vec<LineKind> = chunk.lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![LineKind::Changed, LineKind::Added],
            "one deletion pairs with the first addition; the second stays added"
        );
    }

    #[test]
    fn the_context_runs_around_a_change_are_kept_apart() {
        let diff = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -1,5 +1,5 @@
 before 1
 before 2
-old
+new
 after 1
 after 2
";
        let files = parse_unified_diff(diff);
        let chunk = &files[0].chunks[0];
        assert_eq!(chunk.before_context().len(), 2);
        assert_eq!(chunk.after_context().len(), 2);
        assert_eq!(chunk.changed_lines().count(), 1);
    }

    #[test]
    fn the_no_newline_marker_is_not_a_line() {
        let diff = "\
diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,1 +1,1 @@
-old
+new
\\ No newline at end of file
";
        let files = parse_unified_diff(diff);
        let chunk = &files[0].chunks[0];
        assert_eq!(chunk.lines.len(), 1);
        assert_eq!(chunk.lines[0].kind, LineKind::Changed);
    }

    #[test]
    fn a_rename_keeps_both_paths() {
        let diff = "\
diff --git a/old.rs b/new.rs
similarity index 95%
rename from old.rs
rename to new.rs
--- a/old.rs
+++ b/new.rs
@@ -1,1 +1,1 @@
-a
+b
";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].old_path.as_deref(), Some("old.rs"));
        assert_eq!(files[0].new_path.as_deref(), Some("new.rs"));
    }

    #[test]
    fn a_deleted_file_has_no_new_path() {
        let diff = "\
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1,1 +0,0 @@
-a
";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].new_path, None);
        assert_eq!(files[0].old_path.as_deref(), Some("gone.rs"));
    }

    #[test]
    fn a_malformed_hunk_header_keeps_the_lines_that_follow_it() {
        let diff = "\
diff --git a/f b/f
--- a/f
+++ b/f
@@ this is not a header @@
+added
";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].chunks.len(), 1);
        assert_eq!(files[0].chunks[0].lines.len(), 1);
        assert_eq!(files[0].chunks[0].lines[0].kind, LineKind::Added);
    }

    #[test]
    fn metadata_between_the_header_and_the_first_hunk_is_not_a_line() {
        let diff = "\
diff --git a/f b/f
index 1234567..89abcde 100644
--- a/f
+++ b/f
@@ -1,1 +1,1 @@
-a
+b
";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].chunks[0].lines.len(), 1);
    }

    #[test]
    fn a_line_inside_a_hunk_that_looks_like_a_header_is_still_a_line() {
        // A deleted line whose text is "-- x" renders as "--- x", and an
        // added line whose text is "++ x" renders as "+++ x" — both look
        // exactly like file headers.
        let diff = "\
diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,2 +1,2 @@
--- x
+++ x
";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        let chunk = &files[0].chunks[0];
        assert_eq!(chunk.lines.len(), 1);
        assert_eq!(chunk.lines[0].kind, LineKind::Changed);
        assert_eq!(chunk.lines[0].old_text.as_deref(), Some("-- x"));
        assert_eq!(chunk.lines[0].new_text.as_deref(), Some("++ x"));
    }

    #[test]
    fn a_multi_file_diff_keeps_each_file_apart() {
        let diff = "\
diff --git a/one.rs b/one.rs
--- a/one.rs
+++ b/one.rs
@@ -1,3 +1,3 @@
 ctx
-old
+new
diff --git a/two.rs b/two.rs
--- a/two.rs
+++ b/two.rs
@@ -1,2 +1,3 @@
 keep
+added
";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].new_path.as_deref(), Some("one.rs"));
        assert_eq!(files[1].new_path.as_deref(), Some("two.rs"));

        let one = &files[0].chunks[0].lines;
        assert_eq!(one.len(), 2);
        assert_eq!(one[0].kind, LineKind::Unchanged);
        assert_eq!(one[0].old_line, Some(1));
        assert_eq!(one[0].new_line, Some(1));
        assert_eq!(one[1].kind, LineKind::Changed);
        assert_eq!(one[1].old_line, Some(2));
        assert_eq!(one[1].new_line, Some(2));

        let two = &files[1].chunks[0].lines;
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].kind, LineKind::Unchanged);
        assert_eq!(two[1].kind, LineKind::Added);
        assert_eq!(two[1].new_line, Some(2));
        assert_eq!(two[1].old_line, None);
    }

    #[test]
    fn every_hunk_of_a_file_is_kept() {
        let diff = "\
diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,2 +1,2 @@
-a
+b
@@ -10,2 +10,2 @@
-c
+d
";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].chunks.len(), 2, "both hunks of the file are kept");
        assert_eq!(files[0].chunks[0].new_start, 1);
        assert_eq!(files[0].chunks[1].new_start, 10);
        assert_eq!(files[0].chunks[1].lines[0].new_line, Some(10));
    }

    #[test]
    fn a_file_key_is_the_new_path_when_there_is_one() {
        let added = FileDiff {
            old_path: None,
            new_path: Some("new.rs".into()),
            chunks: vec![],
        };
        assert_eq!(file_key(&added), Some("new.rs"));
        let deleted = FileDiff {
            old_path: Some("old.rs".into()),
            new_path: None,
            chunks: vec![],
        };
        assert_eq!(file_key(&deleted), None);
    }
}

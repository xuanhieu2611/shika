use super::budget::Limit;
use super::patch::{header_name, parse_hunks, patch_name, read_line, unquote};
use super::*;

fn kinds(hunk: &Hunk) -> Vec<LineKind> {
    hunk.lines.iter().map(|line| line.kind).collect()
}

#[test]
fn quoted_names_unquote_every_escape() {
    let (name, rest) = unquote(br#""a/q\"t\\x\t\n\303\274.txt" tail"#).unwrap();
    assert_eq!(name, "a/q\"t\\x\t\nü.txt".as_bytes());
    assert_eq!(rest, b" tail");
    assert_eq!(unquote(b"plain"), None);
    assert_eq!(unquote(br#""open"#), None);
}

#[test]
fn header_names_split_by_length_and_quotes() {
    assert_eq!(
        header_name(b"a/x b/y b/x b/y").as_deref(),
        Some(&b"x b/y"[..])
    );
    assert_eq!(
        header_name(b"a/sp ace b/sp ace").as_deref(),
        Some(&b"sp ace"[..])
    );
    assert_eq!(
        header_name(br#""a/\303\274" "b/\303\274""#).as_deref(),
        Some("ü".as_bytes())
    );
    // A rename's header has two different names; the parser uses its
    // `rename from`/`rename to` lines instead.
    assert_eq!(header_name(b"a/old b/new"), None);
    assert_eq!(header_name(b"a/x"), None);
}

#[test]
fn patch_names_drop_the_tab_and_the_prefix() {
    assert_eq!(
        patch_name(b"a/sp ace.txt\t", b"a/").as_deref(),
        Some(&b"sp ace.txt"[..])
    );
    assert_eq!(patch_name(b"/dev/null", b"a/"), None);
    assert_eq!(
        patch_name(br#""b/q\"x""#, b"b/").as_deref(),
        Some(&b"q\"x"[..])
    );
}

#[test]
fn hunks_number_lines_and_keep_markers() {
    let patch = b"diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n\
@@ -1,3 +1,3 @@ fn x\n a\n-b\n+B\n\n@@ -10 +10 @@\n-y\n\\ No newline at end of file\n+y\n";
    let diff = session_diff(&patch[..], &[], Path::new("/nonexistent"), "base").unwrap();
    let file = &diff.files[0];
    assert_eq!(file.path, "f");
    assert_eq!((file.insertions, file.deletions), (2, 2));
    assert_eq!(file.hunks.len(), 2);
    assert_eq!(file.hunks[0].header, "@@ -1,3 +1,3 @@ fn x");
    assert_eq!(
        kinds(&file.hunks[0]),
        [
            LineKind::Context,
            LineKind::Removed,
            LineKind::Added,
            LineKind::Context
        ]
    );
    // A blank line is an empty context line (`diff.suppressBlankEmpty`).
    let blank = &file.hunks[0].lines[3];
    assert_eq!(
        (blank.old, blank.new, blank.text.as_str()),
        (Some(3), Some(3), "")
    );
    let second = &file.hunks[1].lines;
    assert_eq!((second[0].old, second[0].new), (Some(10), None));
    assert_eq!(second[1].kind, LineKind::NoNewline);
    assert_eq!(second[1].text, "\\ No newline at end of file");
    assert_eq!((second[2].old, second[2].new), (None, Some(10)));
    assert_eq!(
        diff.stat,
        DiffStat {
            files: 1,
            insertions: 2,
            deletions: 2
        }
    );
}

#[test]
fn a_limit_keeps_a_prefix_and_counts_the_rest() {
    let body = b"@@ -1,2 +1,2 @@\n-a\n+b\n@@ -9 +9 @@\n-c\n+d\n";
    let (hunks, parsed) = parse_hunks(
        body,
        Limit::Upto {
            lines: 3,
            bytes: usize::MAX,
        },
    );
    assert_eq!(parsed, 3);
    assert_eq!(hunks.len(), 2);
    assert_eq!(kinds(&hunks[1]), [LineKind::Removed]);
    // A hunk header whose first line does not fit is dropped.
    let (hunks, parsed) = parse_hunks(
        body,
        Limit::Upto {
            lines: 2,
            bytes: usize::MAX,
        },
    );
    assert_eq!((hunks.len(), parsed), (1, 2));
}

#[test]
fn the_total_cap_collapses_every_later_file() {
    let mut patch = Vec::new();
    for name in ["a", "b", "c"] {
        patch.extend_from_slice(
            format!("diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n@@ -1,2 +1,2 @@\n-x\n+y\n")
                .as_bytes(),
        );
    }
    let mut budget = Budget::session();
    budget.lines = 3;
    let diff =
        session_diff_with(&patch[..], &[], Path::new("/nonexistent"), "base", budget).unwrap();
    let collapsed: Vec<_> = diff.files.iter().map(|f| f.collapsed).collect();
    assert_eq!(
        collapsed,
        [None, Some(Collapse::Budget), Some(Collapse::Budget)]
    );
    assert_eq!(diff.files[1].hidden_lines, 2);
    assert_eq!(
        diff.stat,
        DiffStat {
            files: 3,
            insertions: 3,
            deletions: 3
        }
    );
}

#[test]
fn a_file_over_the_read_limit_is_counted_but_not_held() {
    // One 3 MB line, as a minified bundle would have.
    let long = "x".repeat(3 * 1024 * 1024);
    let patch = format!(
        "diff --git a/min.js b/min.js\n--- a/min.js\n+++ b/min.js\n@@ -1 +1 @@\n-{long}\n+{long}\n\
diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-x\n+y\n"
    );
    let mut held = 0;
    read_patch(patch.as_bytes(), &mut Budget::session(), |section, _| {
        held = held.max(section.body.len());
    })
    .unwrap();
    assert!(held < 64, "{held}");
    let diff = session_diff(patch.as_bytes(), &[], Path::new("/nonexistent"), "base").unwrap();
    let min = &diff.files[0];
    // The size is the patch body's: the hunk header and both lines.
    let body = "@@ -1 +1 @@\n".len() + 2 * (long.len() + 2);
    assert_eq!(min.collapsed, Some(Collapse::Size(body as u64)));
    assert_eq!((min.insertions, min.deletions, min.hidden_lines), (1, 1, 2));
    // A file over its own cap does not use up the task's budget.
    assert_eq!(diff.files[1].collapsed, None);
    assert_eq!(diff.files[1].hunks[0].lines.len(), 2);
}

#[test]
fn long_lines_are_read_in_part() {
    let mut line = Vec::new();
    let mut input = &b"abcdef\nxy"[..];
    assert_eq!(read_line(&mut input, &mut line, 3).unwrap(), Some(6));
    assert_eq!(line, b"abc");
    assert_eq!(read_line(&mut input, &mut line, 3).unwrap(), Some(2));
    assert_eq!(line, b"xy");
    assert_eq!(read_line(&mut input, &mut line, 3).unwrap(), None);
}

fn text_file(path: &str, status: FileStatus, added: usize, removed: usize) -> FileDiff {
    let mut lines = Vec::new();
    for i in 0..removed {
        lines.push(DiffLine {
            kind: LineKind::Removed,
            old: Some(i as u32 + 1),
            new: None,
            text: format!("old {i}"),
        });
    }
    for i in 0..added {
        lines.push(DiffLine {
            kind: LineKind::Added,
            old: None,
            new: Some(i as u32 + 1),
            text: format!("new {i}\r"),
        });
    }
    FileDiff {
        path: path.into(),
        status,
        insertions: added,
        deletions: removed,
        hunks: vec![Hunk {
            header: "@@ -1 +1 @@".into(),
            lines,
        }],
        ..FileDiff::default()
    }
}

fn session(files: Vec<FileDiff>) -> SessionDiff {
    let stat = DiffStat {
        files: files.len(),
        insertions: files.iter().map(|f| f.insertions).sum(),
        deletions: files.iter().map(|f| f.deletions).sum(),
    };
    SessionDiff { files, stat }
}

#[test]
fn rendering_shows_headers_renames_binary_untracked_and_totals() {
    let mut renamed = text_file("src/new.rs", FileStatus::Renamed, 1, 1);
    renamed.old_path = Some("src/old.rs".into());
    let mut binary = text_file("logo.png", FileStatus::Modified, 0, 0);
    binary.binary = true;
    binary.hunks.clear();
    let mut mode = text_file("run.sh", FileStatus::Modified, 0, 0);
    mode.hunks.clear();
    mode.mode_change = Some(ModeChange {
        old: "100644".into(),
        new: "100755".into(),
    });
    let diff = session(vec![
        text_file("a.rs", FileStatus::Modified, 1, 1),
        renamed,
        binary,
        mode,
        text_file("notes.md", FileStatus::Untracked, 2, 0),
        text_file("gone.rs", FileStatus::Deleted, 0, 1),
    ]);
    let text = render_unified(&diff, false, Path::new("/w"), RENDER_CAP_BYTES);
    assert!(
        text.starts_with(
            "diff M a.rs (+1 -1)\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old 0\n+new 0\n"
        )
    );
    assert!(
        text.contains(
            "diff R src/old.rs -> src/new.rs (+1 -1)\n--- a/src/old.rs\n+++ b/src/new.rs\n"
        )
    );
    assert!(text.contains("diff M logo.png (binary)\nBinary file, not shown.\n"));
    assert!(text.contains("diff M run.sh (+0 -0)\nmode 100644 -> 100755\n"));
    assert!(text.contains("diff U notes.md (+2 -0) untracked\n--- /dev/null\n+++ b/notes.md\n"));
    assert!(text.contains("--- a/gone.rs\n+++ /dev/null\n"));
    // A CRLF line ends without the stray carriage return.
    assert!(!text.contains('\r'));
    assert!(text.ends_with("6 files +4 -3"), "{text}");

    let stat = render_unified(&diff, true, Path::new("/w"), RENDER_CAP_BYTES);
    assert_eq!(
        stat,
        "M a.rs +1 -1\nR src/old.rs -> src/new.rs +1 -1\nM logo.png binary\nM run.sh +0 -0\nU notes.md +2 -0 untracked\nD gone.rs +0 -1\n6 files +4 -3"
    );
    assert_eq!(
        render_unified(&SessionDiff::default(), false, Path::new("/w"), 100),
        "No changes."
    );
}

#[test]
fn rendering_notes_collapsed_files() {
    let mut big = text_file("big.rs", FileStatus::Modified, 3000, 0);
    big.hunks.clear();
    big.collapsed = Some(Collapse::Lines);
    let text = render_unified(
        &session(vec![big]),
        false,
        Path::new("/w"),
        RENDER_CAP_BYTES,
    );
    assert!(text.contains("[3000 changed lines not shown: it changes more than 2000 lines. Read big.rs in the worktree.]"), "{text}");
}

#[test]
fn rendering_stops_at_the_cap_with_a_note_naming_the_worktree() {
    let files: Vec<FileDiff> = (0..20)
        .map(|i| text_file(&format!("f{i}.rs"), FileStatus::Modified, 50, 0))
        .collect();
    let diff = session(files);
    let cap = 2048;
    let text = render_unified(&diff, false, Path::new("/repo/.worktrees/t"), cap);
    let (body, note) = text.split_once("[truncated").unwrap();
    assert!(body.len() <= cap, "{}", body.len());
    assert!(note.contains("of 20 files"), "{note}");
    assert!(note.contains("/repo/.worktrees/t"), "{note}");
    assert!(text.ends_with("20 files +1000 -0"));
    // Cut at a line, never inside one.
    assert!(body.ends_with('\n'));
    // Under the cap, nothing is cut.
    let whole = render_unified(&diff, false, Path::new("/w"), RENDER_CAP_BYTES);
    assert!(!whole.contains("[truncated"));
}

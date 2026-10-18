//! The editing model the native inputs share: text, selection, caret
//! affinity, and what each key means. The key table follows AppKit's
//! `StandardKeyBinding.dict`, so the composer and the single-line fields
//! edit the way NSTextView does. Layout stays with each input behind
//! [`Lines`].
use gpui::Keystroke;
use std::ops::Range;
use std::sync::Mutex;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Default)]
pub struct Text {
    pub value: String,
    pub anchor: usize,
    pub cursor: usize,
    pub marked: Option<Range<usize>>,
    /// At a soft wrap, the caret sits at the start of the lower row. False
    /// keeps it at the end of the upper row, after a move to the line end.
    pub upstream: bool,
    /// The x that consecutive Up and Down moves keep to.
    goal: Option<f32>,
    /// The last edit was a kill, so the next one joins the kill buffer.
    killed: bool,
}

/// The visual rows an input laid its text out in. Rows are in order; a soft
/// wrap ends one row where the next starts, and a newline sits between rows.
pub trait Lines {
    fn rows(&self) -> &[Range<usize>];
    /// The x of `index` on `row`, from the row's left edge.
    fn x(&self, index: usize, row: usize) -> f32;
    /// The index on `row` closest to `x`.
    fn index_at(&self, row: usize, x: f32) -> usize;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    /// The visual row's ends.
    LineStart,
    LineEnd,
    Up,
    Down,
    /// Option+Up and Option+Down: one step, then that paragraph's end.
    ParagraphBackward,
    ParagraphForward,
    /// Ctrl+A and Ctrl+E.
    ParagraphStart,
    ParagraphEnd,
    DocumentStart,
    DocumentEnd,
}
impl Motion {
    fn backward(self) -> bool {
        matches!(
            self,
            Motion::Left
                | Motion::WordLeft
                | Motion::LineStart
                | Motion::Up
                | Motion::ParagraphBackward
                | Motion::ParagraphStart
                | Motion::DocumentStart
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    Move(Motion, bool),
    /// Delete the selection, or from the caret to where the motion lands.
    Delete(Motion),
    /// Ctrl+K and Cmd+Backspace: a delete that fills the kill buffer.
    Kill(Motion),
    Yank,
    Transpose,
    /// Ctrl+O: a newline after the caret.
    OpenLine,
    /// Home and End scroll without moving the caret.
    Scroll(bool),
}

/// What `stroke` does in a text input, or None when it is not an editing key.
pub fn edit_for(stroke: &Keystroke, multiline: bool) -> Option<Edit> {
    use Motion::*;
    let m = stroke.modifiers;
    let s = m.shift;
    let key = stroke.key.as_str();
    let plain = !m.control && !m.alt && !m.platform;
    let alt = m.alt && !m.control && !m.platform;
    let cmd = m.platform && !m.control && !m.alt;
    let ctrl = m.control && !m.alt && !m.platform;
    let ctrl_alt = m.control && m.alt && !m.platform;
    Some(match key {
        "left" if plain => Edit::Move(Left, s),
        "right" if plain => Edit::Move(Right, s),
        "left" if alt => Edit::Move(WordLeft, s),
        "right" if alt => Edit::Move(WordRight, s),
        "left" if cmd || ctrl => Edit::Move(LineStart, s),
        "right" if cmd || ctrl => Edit::Move(LineEnd, s),
        "up" if plain => Edit::Move(Up, s),
        "down" if plain => Edit::Move(Down, s),
        "up" if alt => Edit::Move(ParagraphBackward, s),
        "down" if alt => Edit::Move(ParagraphForward, s),
        "up" if cmd => Edit::Move(DocumentStart, s),
        "down" if cmd => Edit::Move(DocumentEnd, s),
        "home" | "end" if plain && (s || !multiline) => Edit::Move(
            if key == "home" {
                DocumentStart
            } else {
                DocumentEnd
            },
            s,
        ),
        "home" | "end" if plain => Edit::Scroll(key == "home"),
        "backspace" if plain => Edit::Delete(Left),
        "backspace" if alt || ctrl_alt => Edit::Delete(WordLeft),
        "backspace" if cmd => Edit::Kill(LineStart),
        "delete" if plain => Edit::Delete(Right),
        "delete" if alt => Edit::Delete(WordRight),
        "b" if ctrl_alt => Edit::Move(WordLeft, s),
        "f" if ctrl_alt => Edit::Move(WordRight, s),
        "a" if ctrl => Edit::Move(ParagraphStart, s),
        "e" if ctrl => Edit::Move(ParagraphEnd, s),
        "b" if ctrl => Edit::Move(Left, s),
        "f" if ctrl => Edit::Move(Right, s),
        "p" if ctrl => Edit::Move(Up, s),
        "n" if ctrl => Edit::Move(Down, s),
        "h" if ctrl && !s => Edit::Delete(Left),
        "d" if ctrl && !s => Edit::Delete(Right),
        "k" if ctrl && !s => Edit::Kill(ParagraphEnd),
        "y" if ctrl && !s => Edit::Yank,
        "t" if ctrl && !s => Edit::Transpose,
        "o" if ctrl && !s && multiline => Edit::OpenLine,
        _ => return None,
    })
}

/// AppKit keeps one kill buffer for every text view in the app.
pub static KILL_BUFFER: Mutex<String> = Mutex::new(String::new());

impl Text {
    pub fn new(value: String) -> Self {
        let cursor = value.len();
        Self {
            value,
            anchor: cursor,
            cursor,
            ..Default::default()
        }
    }
    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor)
    }
    pub fn move_to(&mut self, offset: usize, select: bool) {
        self.cursor = offset;
        if !select {
            self.anchor = offset;
        }
        self.marked = None;
        self.upstream = false;
        self.goal = None;
        self.killed = false;
    }
    pub fn previous(&self) -> usize {
        self.previous_from(self.cursor)
    }
    pub fn next(&self) -> usize {
        self.next_from(self.cursor)
    }
    fn previous_from(&self, at: usize) -> usize {
        self.value
            .grapheme_indices(true)
            .rev()
            .find_map(|(i, _)| (i < at).then_some(i))
            .unwrap_or(0)
    }
    fn next_from(&self, at: usize) -> usize {
        self.value
            .grapheme_indices(true)
            .find_map(|(i, _)| (i > at).then_some(i))
            .unwrap_or(self.value.len())
    }
    // macOS word rules: skip whitespace and punctuation, then the word.
    fn word_start(&self, at: usize) -> usize {
        self.value[..at]
            .split_word_bound_indices()
            .rev()
            .find_map(|(i, word)| word.chars().any(char::is_alphanumeric).then_some(i))
            .unwrap_or(0)
    }
    fn word_end(&self, at: usize) -> usize {
        self.value[at..]
            .split_word_bound_indices()
            .find_map(|(i, word)| {
                word.chars()
                    .any(char::is_alphanumeric)
                    .then_some(at + i + word.len())
            })
            .unwrap_or(self.value.len())
    }
    fn paragraph_start(&self, at: usize) -> usize {
        self.value[..at].rfind('\n').map_or(0, |i| i + 1)
    }
    fn paragraph_end(&self, at: usize) -> usize {
        self.value[at..]
            .find('\n')
            .map_or(self.value.len(), |i| at + i)
    }
    /// The row the caret is on: a soft wrap boundary belongs to the lower
    /// row unless the caret is upstream.
    pub fn row(&self, lines: &impl Lines) -> usize {
        row_of(lines.rows(), self.cursor, self.upstream)
    }
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        self.value.replace_range(range.clone(), text);
        self.cursor = range.start + text.len();
        self.anchor = self.cursor;
        self.marked = None;
        self.upstream = false;
        self.goal = None;
        self.killed = false;
    }
    pub fn utf8_range(&self, range: Range<usize>) -> Range<usize> {
        utf8_offset(&self.value, range.start)..utf8_offset(&self.value, range.end)
    }
    pub fn to_utf16(&self, range: Range<usize>) -> Range<usize> {
        self.value[..range.start].encode_utf16().count()
            ..self.value[..range.end].encode_utf16().count()
    }
    /// Where `motion` lands from `at`, and whether the caret stays upstream.
    fn target(&self, motion: Motion, at: usize, lines: &impl Lines) -> (usize, bool) {
        let rows = lines.rows();
        let row = row_of(rows, at, at == self.cursor && self.upstream);
        let soft_end = |row: usize, index: usize| {
            index == rows[row].end && rows.get(row + 1).is_some_and(|next| next.start == index)
        };
        let to = match motion {
            Motion::Left => self.previous_from(at),
            Motion::Right => self.next_from(at),
            Motion::WordLeft => self.word_start(at),
            Motion::WordRight => self.word_end(at),
            Motion::LineStart => rows[row].start,
            Motion::LineEnd => return (rows[row].end, soft_end(row, rows[row].end)),
            Motion::Up | Motion::Down => {
                let up = motion == Motion::Up;
                let next = if up {
                    row.checked_sub(1)
                } else {
                    Some(row + 1).filter(|r| *r < rows.len())
                };
                let Some(next) = next else {
                    return (if up { 0 } else { self.value.len() }, false);
                };
                let x = self.goal.unwrap_or_else(|| lines.x(at, row));
                let index = lines
                    .index_at(next, x)
                    .clamp(rows[next].start, rows[next].end);
                return (index, soft_end(next, index));
            }
            Motion::ParagraphBackward => self.paragraph_start(self.previous_from(at)),
            Motion::ParagraphForward => self.paragraph_end(self.next_from(at)),
            Motion::ParagraphStart => self.paragraph_start(at),
            Motion::ParagraphEnd => self.paragraph_end(at),
            Motion::DocumentStart => 0,
            Motion::DocumentEnd => self.value.len(),
        };
        (to, false)
    }
    /// Apply one editing key. Returns false for Scroll, which the input does.
    pub fn apply(&mut self, edit: Edit, lines: &impl Lines, kill: &mut String) -> bool {
        let killed = std::mem::take(&mut self.killed);
        let selected = self.selection();
        match edit {
            Edit::Move(motion, select) => {
                // Without Shift, a selection moves from the edge it is leaving by.
                let at = if select || selected.is_empty() {
                    self.cursor
                } else if motion.backward() {
                    selected.start
                } else {
                    selected.end
                };
                let collapse = !select && !selected.is_empty();
                let (to, upstream) = match motion {
                    Motion::Left | Motion::Right if collapse => (at, false),
                    Motion::ParagraphBackward if collapse => (self.paragraph_start(at), false),
                    Motion::ParagraphForward if collapse => (self.paragraph_end(at), false),
                    _ => self.target(motion, at, lines),
                };
                let goal = matches!(motion, Motion::Up | Motion::Down).then(|| {
                    let upstream = at == self.cursor && self.upstream;
                    self.goal
                        .unwrap_or_else(|| lines.x(at, row_of(lines.rows(), at, upstream)))
                });
                self.move_to(to, select);
                self.upstream = upstream;
                self.goal = goal;
            }
            Edit::Delete(motion) | Edit::Kill(motion) => {
                let range = if !selected.is_empty() {
                    selected
                } else {
                    let (to, _) = self.target(motion, self.cursor, lines);
                    let range = to.min(self.cursor)..to.max(self.cursor);
                    if !range.is_empty() {
                        range
                    } else if motion.backward() {
                        // Cmd+Backspace at a row start joins it to the row above.
                        self.previous()..self.cursor
                    } else {
                        // Ctrl+K at a paragraph end deletes the newline.
                        self.cursor..self.next()
                    }
                };
                if let Edit::Kill(_) = edit {
                    let text = &self.value[range.clone()];
                    if !killed {
                        kill.clear();
                    }
                    if motion.backward() {
                        kill.insert_str(0, text);
                    } else {
                        kill.push_str(text);
                    }
                }
                self.replace(range, "");
                self.killed = matches!(edit, Edit::Kill(_));
            }
            Edit::Yank => {
                let text = kill.clone();
                self.replace(selected, &text);
            }
            Edit::Transpose => self.transpose(),
            Edit::OpenLine => {
                self.replace(selected, "\n");
                self.move_to(self.cursor - 1, false);
            }
            Edit::Scroll(_) => return false,
        }
        true
    }
    /// Ctrl+T: swap the graphemes around the caret and step past them; at a
    /// paragraph end, swap the two before it.
    fn transpose(&mut self) {
        if !self.selection().is_empty() || self.cursor == 0 {
            return;
        }
        let at_end = self.cursor == self.paragraph_end(self.cursor);
        let middle = if at_end { self.previous() } else { self.cursor };
        if middle == 0 {
            return;
        }
        let start = self.previous_from(middle);
        let end = self.next_from(middle);
        let swapped = format!("{}{}", &self.value[middle..end], &self.value[start..middle]);
        self.replace(start..end, &swapped);
    }
}

fn row_of(rows: &[Range<usize>], index: usize, upstream: bool) -> usize {
    let row = rows
        .iter()
        .position(|row| row.start <= index && index <= row.end)
        .unwrap_or(rows.len().saturating_sub(1));
    let soft = rows
        .get(row + 1)
        .is_some_and(|next| next.start == index && rows[row].end == index);
    if soft && !upstream { row + 1 } else { row }
}

pub fn utf8_offset(text: &str, offset: usize) -> usize {
    let mut count = 0;
    for (i, ch) in text.char_indices() {
        if count >= offset {
            return i;
        }
        count += ch.len_utf16();
    }
    text.len()
}

/// Rows from newlines alone, one unit of x per grapheme. Single-line fields
/// use it, and so does the composer while its layout is behind its text.
pub struct Paragraphs<'a> {
    text: &'a str,
    rows: Vec<Range<usize>>,
}
impl<'a> Paragraphs<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut rows = Vec::new();
        let mut start = 0;
        for (i, _) in text.match_indices('\n') {
            rows.push(start..i);
            start = i + 1;
        }
        rows.push(start..text.len());
        Self { text, rows }
    }
}
impl Lines for Paragraphs<'_> {
    fn rows(&self) -> &[Range<usize>] {
        &self.rows
    }
    fn x(&self, index: usize, row: usize) -> f32 {
        self.text[self.rows[row].start..index]
            .graphemes(true)
            .count() as f32
    }
    fn index_at(&self, row: usize, x: f32) -> usize {
        let range = self.rows[row].clone();
        self.text[range.clone()]
            .grapheme_indices(true)
            .map(|(i, _)| range.start + i)
            .chain([range.end])
            .nth(x.round().max(0.) as usize)
            .unwrap_or(range.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    /// Monospace rows that wrap after `width` graphemes, the way a text view
    /// wraps a long paragraph: the break follows the last space that fits.
    struct Wrapped<'a> {
        text: &'a str,
        rows: Vec<Range<usize>>,
    }
    impl<'a> Wrapped<'a> {
        fn new(text: &'a str, width: usize) -> Self {
            let mut rows = Vec::new();
            for hard in Paragraphs::new(text).rows {
                let mut start = hard.start;
                loop {
                    let rest = &text[start..hard.end];
                    if rest.graphemes(true).count() <= width {
                        rows.push(start..hard.end);
                        break;
                    }
                    let limit = rest
                        .grapheme_indices(true)
                        .nth(width)
                        .map_or(rest.len(), |(i, _)| i);
                    let end = rest[..limit].rfind(' ').map_or(limit, |i| i + 1);
                    rows.push(start..start + end);
                    start += end;
                }
            }
            Self { text, rows }
        }
    }
    impl Lines for Wrapped<'_> {
        fn rows(&self) -> &[Range<usize>] {
            &self.rows
        }
        fn x(&self, index: usize, row: usize) -> f32 {
            self.text[self.rows[row].start..index]
                .graphemes(true)
                .count() as f32
        }
        fn index_at(&self, row: usize, x: f32) -> usize {
            let range = self.rows[row].clone();
            self.text[range.clone()]
                .grapheme_indices(true)
                .map(|(i, _)| range.start + i)
                .chain([range.end])
                .nth(x.round() as usize)
                .unwrap_or(range.end)
        }
    }

    fn key(raw: &str) -> Keystroke {
        Keystroke::parse(raw).unwrap()
    }
    fn at(value: &str, cursor: usize) -> Text {
        Text {
            value: value.into(),
            anchor: cursor,
            cursor,
            ..Default::default()
        }
    }
    fn selected(value: &str, anchor: usize, cursor: usize) -> Text {
        Text {
            anchor,
            ..at(value, cursor)
        }
    }
    /// Press `keys` in a field `width` graphemes wide.
    fn press(text: &mut Text, width: usize, keys: &[&str]) -> String {
        let mut kill = String::new();
        press_with(text, width, keys, &mut kill);
        kill
    }
    fn press_with(text: &mut Text, width: usize, keys: &[&str], kill: &mut String) {
        for raw in keys {
            let edit = edit_for(&key(raw), true).unwrap_or_else(|| panic!("{raw} is not bound"));
            let value = text.value.clone();
            let lines = Wrapped::new(&value, width);
            text.apply(edit, &lines, kill);
        }
    }
    fn state(text: &Text) -> (&str, usize, usize) {
        (text.value.as_str(), text.anchor, text.cursor)
    }

    #[test]
    fn keys_follow_the_standard_key_bindings() {
        use Motion::*;
        let cases = [
            ("alt-backspace", Some(Edit::Delete(WordLeft))),
            ("ctrl-alt-backspace", Some(Edit::Delete(WordLeft))),
            ("alt-delete", Some(Edit::Delete(WordRight))),
            ("cmd-backspace", Some(Edit::Kill(LineStart))),
            ("cmd-delete", None),
            ("ctrl-backspace", None),
            ("cmd-left", Some(Edit::Move(LineStart, false))),
            ("cmd-shift-right", Some(Edit::Move(LineEnd, true))),
            ("ctrl-left", Some(Edit::Move(LineStart, false))),
            ("ctrl-up", None),
            ("cmd-up", Some(Edit::Move(DocumentStart, false))),
            ("cmd-shift-down", Some(Edit::Move(DocumentEnd, true))),
            ("alt-up", Some(Edit::Move(ParagraphBackward, false))),
            ("alt-shift-down", Some(Edit::Move(ParagraphForward, true))),
            ("home", Some(Edit::Scroll(true))),
            ("shift-end", Some(Edit::Move(DocumentEnd, true))),
            ("alt-home", None),
            ("ctrl-a", Some(Edit::Move(ParagraphStart, false))),
            ("ctrl-shift-e", Some(Edit::Move(ParagraphEnd, true))),
            ("ctrl-b", Some(Edit::Move(Left, false))),
            ("ctrl-shift-f", Some(Edit::Move(Right, true))),
            ("ctrl-n", Some(Edit::Move(Down, false))),
            ("ctrl-shift-p", Some(Edit::Move(Up, true))),
            ("ctrl-alt-b", Some(Edit::Move(WordLeft, false))),
            ("ctrl-alt-shift-f", Some(Edit::Move(WordRight, true))),
            ("ctrl-h", Some(Edit::Delete(Left))),
            ("ctrl-d", Some(Edit::Delete(Right))),
            ("ctrl-k", Some(Edit::Kill(ParagraphEnd))),
            ("ctrl-y", Some(Edit::Yank)),
            ("ctrl-t", Some(Edit::Transpose)),
            ("ctrl-o", Some(Edit::OpenLine)),
            ("ctrl-q", None),
            ("ctrl-tab", None),
            ("cmd-a", None),
            ("cmd-z", None),
            ("enter", None),
        ];
        for (raw, edit) in cases {
            assert_eq!(edit_for(&key(raw), true), edit, "{raw}");
        }
        assert_eq!(
            edit_for(&key("home"), false),
            Some(Edit::Move(DocumentStart, false))
        );
        assert_eq!(edit_for(&key("ctrl-o"), false), None);
        let fn_arrow = Keystroke {
            modifiers: Modifiers {
                function: true,
                ..Modifiers::default()
            },
            ..key("left")
        };
        assert_eq!(edit_for(&fn_arrow, true), Some(Edit::Move(Left, false)));
    }

    #[test]
    fn option_backspace_deletes_words_by_macos_rules() {
        for (value, cursor, rest) in [
            ("hello world", 11, "hello "),
            ("hello world  ", 13, "hello "),
            ("run the tests.", 14, "run the "),
            ("see (foo).", 10, "see ("),
            ("don't", 5, ""),
            ("snake_case", 10, ""),
            ("hello world", 8, "hello rld"),
            ("  ...", 5, ""),
            ("", 0, ""),
            ("hello", 0, "hello"),
            ("one\ntwo", 7, "one\n"),
            ("one two\n\n", 9, "one "),
            ("x café", 7, "x "),
            ("да привет", "да привет".len(), "да "),
            // UAX #29 has no dictionary, so ideographs go one at a time.
            ("go 日本語", "go 日本語".len(), "go 日本"),
            (
                "ok e\u{301}te\u{301} 👩‍💻",
                "ok e\u{301}te\u{301} 👩‍💻".len(),
                "ok ",
            ),
        ] {
            let mut text = at(value, cursor);
            press(&mut text, 80, &["alt-backspace"]);
            assert_eq!(text.value, rest, "{value:?}");
            assert!(text.value.is_char_boundary(text.cursor));
        }
        let mut text = at("hello, world", 5);
        press(&mut text, 80, &["alt-delete"]);
        assert_eq!(state(&text), ("hello", 5, 5));
    }

    #[test]
    fn every_delete_takes_the_selection_first() {
        for raw in [
            "backspace",
            "delete",
            "alt-backspace",
            "alt-delete",
            "cmd-backspace",
            "ctrl-h",
            "ctrl-d",
            "ctrl-k",
        ] {
            let mut text = selected("hello brave world", 12, 6);
            let kill = press(&mut text, 80, &[raw]);
            assert_eq!(state(&text), ("hello world", 6, 6), "{raw}");
            let killed = matches!(raw, "cmd-backspace" | "ctrl-k");
            assert_eq!(kill, if killed { "brave " } else { "" }, "{raw}");
        }
    }

    #[test]
    fn cmd_backspace_deletes_to_the_visual_row_start() {
        // Rows: "the quick " | "brown fox".
        let mut text = at("the quick brown fox", 19);
        let kill = press(&mut text, 12, &["cmd-backspace"]);
        assert_eq!(state(&text), ("the quick ", 10, 10));
        assert_eq!(kill, "brown fox");
        // At a row start it joins the row above, as NSTextView does.
        let mut text = at("one\ntwo", 4);
        press(&mut text, 80, &["cmd-backspace"]);
        assert_eq!(text.value, "onetwo");
        let mut text = at("one\ntwo three", 13);
        press(&mut text, 80, &["cmd-backspace"]);
        assert_eq!(text.value, "one\n");
    }

    #[test]
    fn cmd_arrows_use_visual_rows_and_keep_the_caret_on_its_row() {
        // Rows: "the quick " | "brown fox " | "jumps".
        let value = "the quick brown fox jumps";
        let mut text = at(value, 13);
        press(&mut text, 12, &["cmd-left"]);
        assert_eq!((text.cursor, text.upstream), (10, false));
        press(&mut text, 12, &["cmd-right"]);
        // The end of "brown fox " is also the start of "jumps"; the caret
        // stays upstream so it paints on the row it was asked for.
        assert_eq!((text.cursor, text.upstream), (20, true));
        press(&mut text, 12, &["cmd-left"]);
        assert_eq!(text.cursor, 10);
        press(&mut text, 12, &["cmd-right", "cmd-right"]);
        assert_eq!(text.cursor, 20);
        press(&mut text, 12, &["right", "cmd-right"]);
        assert_eq!((text.cursor, text.upstream), (25, false));
        // Hard lines end before the newline.
        let mut text = at("one\ntwo", 1);
        press(&mut text, 80, &["cmd-right"]);
        assert_eq!(text.cursor, 3);
        press(&mut text, 80, &["cmd-down"]);
        assert_eq!(text.cursor, 7);
        press(&mut text, 80, &["cmd-up"]);
        assert_eq!(text.cursor, 0);
    }

    #[test]
    fn shift_extends_from_a_fixed_anchor() {
        let value = "the quick brown fox jumps";
        let mut text = at(value, 13);
        press(&mut text, 12, &["cmd-shift-left"]);
        assert_eq!(state(&text), (value, 13, 10));
        press(&mut text, 12, &["cmd-shift-right"]);
        assert_eq!(state(&text), (value, 13, 20));
        press(&mut text, 12, &["alt-shift-left", "shift-left"]);
        assert_eq!(state(&text), (value, 13, 15));
        press(&mut text, 12, &["cmd-shift-up"]);
        assert_eq!(state(&text), (value, 13, 0));
        press(&mut text, 12, &["shift-end"]);
        assert_eq!(state(&text), (value, 13, 25));
        press(&mut text, 12, &["ctrl-shift-a"]);
        assert_eq!(state(&text), (value, 13, 0));
        press(&mut text, 12, &["ctrl-shift-f", "ctrl-shift-n"]);
        assert_eq!(text.anchor, 13);
        assert_eq!(text.cursor, 11);
    }

    #[test]
    fn plain_moves_leave_a_selection_from_its_edge() {
        let value = "one two three";
        let cases = [
            ("left", 4),
            ("right", 7),
            ("alt-left", 0),
            ("alt-right", 13),
            ("cmd-left", 0),
            ("cmd-right", 13),
            ("ctrl-b", 4),
            ("ctrl-f", 7),
        ];
        for (raw, to) in cases {
            let mut text = selected(value, 7, 4);
            press(&mut text, 80, &[raw]);
            assert_eq!(state(&text), (value, to, to), "{raw}");
        }
    }

    #[test]
    fn up_and_down_keep_their_column_and_stop_at_the_ends() {
        let value = "abcdef\nab\nabcdef";
        let mut text = at(value, 5);
        press(&mut text, 80, &["down"]);
        assert_eq!(text.cursor, 9);
        press(&mut text, 80, &["down"]);
        assert_eq!(text.cursor, 15);
        press(&mut text, 80, &["ctrl-p", "ctrl-p"]);
        assert_eq!(text.cursor, 5);
        press(&mut text, 80, &["up"]);
        assert_eq!(text.cursor, 0);
        press(&mut text, 80, &["ctrl-n", "ctrl-n", "ctrl-n", "down"]);
        assert_eq!(text.cursor, value.len());
        // A selection moves from its top going up and its bottom going down.
        let mut text = selected(value, 2, 12);
        press(&mut text, 80, &["up"]);
        assert_eq!(text.cursor, 0);
        let mut text = selected(value, 14, 11);
        press(&mut text, 80, &["up"]);
        assert_eq!(text.cursor, 8);
        let mut text = selected(value, 2, 12);
        press(&mut text, 80, &["down"]);
        assert_eq!(text.cursor, value.len());
        // Wrapped rows count as lines.
        let mut text = at("the quick brown fox", 2);
        press(&mut text, 12, &["down"]);
        assert_eq!(text.cursor, 12);
        // Past a short soft row the caret stays on that row.
        let mut text = at("aaaaaaaaaa bb cccccccccc", 9);
        press(&mut text, 12, &["down"]);
        assert_eq!((text.cursor, text.upstream), (14, true));
        press(&mut text, 12, &["down"]);
        assert_eq!(text.cursor, 23);
    }

    #[test]
    fn option_up_and_down_step_through_paragraphs() {
        let value = "one\ntwo two\nthree";
        let mut text = at(value, 9);
        press(&mut text, 80, &["alt-up"]);
        assert_eq!(text.cursor, 4);
        press(&mut text, 80, &["alt-up"]);
        assert_eq!(text.cursor, 0);
        press(&mut text, 80, &["alt-down"]);
        assert_eq!(text.cursor, 3);
        press(&mut text, 80, &["alt-down"]);
        assert_eq!(text.cursor, 11);
        press(&mut text, 80, &["alt-down", "alt-down"]);
        assert_eq!(text.cursor, value.len());
        let mut text = at(value, 9);
        press(&mut text, 80, &["alt-shift-up", "alt-shift-up"]);
        assert_eq!(state(&text), (value, 9, 0));
        // Ctrl+A and Ctrl+E stay in the paragraph, even when it wraps.
        let mut text = at("aaaa bbbb cccc\nz", 12);
        press(&mut text, 6, &["ctrl-a"]);
        assert_eq!(text.cursor, 0);
        press(&mut text, 6, &["ctrl-e", "ctrl-e"]);
        assert_eq!(text.cursor, 14);
    }

    #[test]
    fn ctrl_k_kills_to_the_paragraph_end_and_ctrl_y_yanks() {
        let mut kill = String::new();
        let mut text = at("one two\nthree", 3);
        press_with(&mut text, 80, &["ctrl-k"], &mut kill);
        assert_eq!((text.value.as_str(), kill.as_str()), ("one\nthree", " two"));
        // At the end it takes the newline, and consecutive kills add up.
        press_with(&mut text, 80, &["ctrl-k", "ctrl-k"], &mut kill);
        assert_eq!((text.value.as_str(), kill.as_str()), ("one", " two\nthree"));
        press_with(&mut text, 80, &["ctrl-a", "ctrl-y"], &mut kill);
        assert_eq!(state(&text), (" two\nthreeone", 10, 10));
        // Another edit starts a new kill; a backward kill goes in front.
        press_with(&mut text, 80, &["left", "ctrl-k"], &mut kill);
        assert_eq!(kill, "eone");
        let mut text = at("ab cd", 5);
        press_with(&mut text, 2, &["cmd-backspace", "cmd-backspace"], &mut kill);
        assert_eq!((text.value.as_str(), kill.as_str()), ("ab", " cd"));
        // A kill of nothing at the very end leaves the buffer empty.
        let mut text = at("x", 1);
        press_with(&mut text, 80, &["ctrl-k"], &mut kill);
        assert_eq!((text.value.as_str(), kill.as_str()), ("x", ""));
    }

    #[test]
    fn ctrl_h_d_t_o_edit_by_grapheme() {
        let value = "ae\u{301}👩‍💻";
        let mut text = at(value, value.len());
        press(&mut text, 80, &["ctrl-h"]);
        assert_eq!(text.value, "ae\u{301}");
        press(&mut text, 80, &["ctrl-a", "ctrl-d"]);
        assert_eq!(text.value, "e\u{301}");
        let mut text = at("abc", 1);
        press(&mut text, 80, &["ctrl-t"]);
        assert_eq!(state(&text), ("bac", 2, 2));
        press(&mut text, 80, &["ctrl-e", "ctrl-t"]);
        assert_eq!(state(&text), ("bca", 3, 3));
        let mut text = at("é👍", "é".len());
        press(&mut text, 80, &["ctrl-t"]);
        assert_eq!(text.value, "👍é");
        let mut text = at("ab", 1);
        press(&mut text, 80, &["ctrl-o"]);
        assert_eq!(state(&text), ("a\nb", 1, 1));
        let mut text = at("ab", 0);
        press(&mut text, 80, &["ctrl-t"]);
        assert_eq!(text.value, "ab");
    }

    #[test]
    fn single_line_fields_reach_both_ends_by_line_and_vertical_moves() {
        let value = "Rename this task";
        let lines = Paragraphs::new(value);
        let mut kill = String::new();
        for (raw, to) in [
            ("up", 0),
            ("down", 16),
            ("ctrl-p", 0),
            ("ctrl-n", 16),
            ("cmd-left", 0),
            ("cmd-right", 16),
            ("alt-up", 0),
            ("alt-down", 16),
            ("home", 0),
            ("end", 16),
        ] {
            let mut text = at(value, 7);
            text.apply(edit_for(&key(raw), false).unwrap(), &lines, &mut kill);
            assert_eq!(text.cursor, to, "{raw}");
        }
    }

    #[test]
    fn paragraphs_split_on_newlines() {
        let lines = Paragraphs::new("a\n\nbc");
        assert_eq!(lines.rows(), &[0..1, 2..2, 3..5]);
        assert_eq!(lines.index_at(2, 1.), 4);
        assert_eq!(lines.index_at(2, 9.), 5);
        assert_eq!(
            Wrapped::new("the quick brown", 12).rows,
            vec![0..10, 10..15]
        );
    }

    #[test]
    fn utf16_offsets_round_trip() {
        let text = Text::new("A😀é".into());
        assert_eq!(text.utf8_range(1..3), 1..5);
        assert_eq!(text.to_utf16(1..5), 1..3);
        assert_eq!(text.utf8_range(99..100), 7..7);
    }
}

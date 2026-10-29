use std::path::{Path, PathBuf};

use ropey::Rope;

use crate::encoding::{self, LineEnding};
use crate::selection::{normalize, Selection};

/// What happened to the file on disk since it was loaded or saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskState {
    Unchanged,
    Modified,
    Deleted,
}

fn disk_stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// Replace chars `start..end` (in pre-edit coordinates) with `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[derive(Clone, Debug)]
struct Txn {
    forward: Vec<Change>,
    inverse: Vec<Change>,
}

#[derive(Clone, Debug)]
struct UndoEntry {
    txns: Vec<Txn>,
    sels_before: Vec<Selection>,
    sels_after: Vec<Selection>,
    kind: Option<&'static str>,
}

fn is_pair(a: char, b: char) -> bool {
    matches!((a, b), ('(', ')') | ('[', ']') | ('{', '}') | ('"', '"') | ('\'', '\'') | ('`', '`'))
}

pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[derive(Clone, Debug)]
pub struct Buffer {
    pub rope: Rope,
    pub sels: Vec<Selection>,
    pub path: Option<PathBuf>,
    pub encoding: String,
    pub line_ending: LineEnding,
    /// mtime and size when last loaded/saved, to notice external changes.
    disk_stamp: Option<(std::time::SystemTime, u64)>,
    /// Bumped on every text change; lets views invalidate caches.
    pub version: u64,
    pub tab_size: usize,
    pub translate_tabs: bool,
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    saved_version: u64,
    /// Lowest line touched since the last call to `take_dirty_line`.
    dirty_from_line: Option<usize>,
    /// Folded regions as char ranges: (end of the header line, end of the last hidden line).
    /// Kept in sync with edits, including undo/redo.
    pub folds: Vec<(usize, usize)>,
    /// Bookmarked positions (line starts), kept in sync with edits.
    pub bookmarks: Vec<usize>,
    /// Whether typing brackets/quotes inserts the closing pair.
    pub auto_pair: bool,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::from_str("")
    }
}

impl Buffer {
    pub fn from_str(s: &str) -> Self {
        Self {
            rope: Rope::from_str(s),
            sels: vec![Selection::caret(0)],
            path: None,
            encoding: "UTF-8".into(),
            line_ending: LineEnding::Unix,
            disk_stamp: None,
            version: 0,
            tab_size: 4,
            translate_tabs: true,
            undo: Vec::new(),
            redo: Vec::new(),
            saved_version: 0,
            dirty_from_line: Some(0),
            folds: Vec::new(),
            bookmarks: Vec::new(),
            auto_pair: true,
        }
    }

    pub fn open(path: &Path) -> std::io::Result<Self> {
        Self::open_with(path, None)
    }

    /// Open with a specific encoding (None = detect).
    pub fn open_with(path: &Path, encoding: Option<&str>) -> std::io::Result<Self> {
        let (text, enc, le) = Self::read_file(path, encoding)?;
        let mut b = Self::from_str(&text);
        b.path = Some(path.to_path_buf());
        b.encoding = enc;
        b.line_ending = le;
        b.disk_stamp = disk_stamp(path);
        Ok(b)
    }

    fn read_file(path: &Path, encoding: Option<&str>) -> std::io::Result<(String, String, LineEnding)> {
        let bytes = std::fs::read(path)?;
        let (text, enc) = encoding::decode(&bytes, encoding);
        let le = LineEnding::detect(&text);
        let text = if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text };
        Ok((text, enc, le))
    }

    pub fn disk_state(&self) -> DiskState {
        let Some(path) = &self.path else { return DiskState::Unchanged };
        match (disk_stamp(path), self.disk_stamp) {
            (None, Some(_)) => DiskState::Deleted,
            (Some(now), Some(then)) if now != then => DiskState::Modified,
            _ => DiskState::Unchanged,
        }
    }

    /// Accept the current disk state without reloading (user chose to keep their version).
    pub fn ignore_disk_change(&mut self) {
        self.disk_stamp = self.path.as_deref().and_then(disk_stamp);
    }

    /// Replace the contents with the file on disk as one undoable step (optionally with another encoding).
    pub fn reload(&mut self, encoding: Option<&str>) -> std::io::Result<()> {
        let path = self.path.clone().ok_or_else(|| std::io::Error::other("no path"))?;
        let (text, enc, le) = Self::read_file(&path, encoding)?;
        self.encoding = enc;
        self.line_ending = le;
        self.disk_stamp = disk_stamp(&path);
        if text != self.text() {
            let len = self.len();
            let n = text.chars().count();
            let sels: Vec<Selection> = self.sels.iter().map(|s| Selection::new(s.anchor.min(n), s.head.min(n))).collect();
            self.transact(vec![Change { start: 0, end: len, text }], sels, None);
        }
        self.saved_version = self.version;
        Ok(())
    }

    pub fn set_line_ending(&mut self, le: LineEnding) {
        if le != self.line_ending {
            self.line_ending = le;
            self.mark_dirty();
        }
    }

    pub fn set_encoding(&mut self, enc: &str) {
        if enc != self.encoding {
            self.encoding = enc.to_string();
            self.mark_dirty();
        }
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        let path = self.path.clone().ok_or_else(|| std::io::Error::other("no path"))?;
        self.save_as(&path)
    }

    pub fn save_as(&mut self, path: &Path) -> std::io::Result<()> {
        let mut text = self.text();
        if self.line_ending != LineEnding::Unix {
            text = text.replace('\n', self.line_ending.as_str());
        }
        std::fs::write(path, encoding::encode(&text, &self.encoding))?;
        self.path = Some(path.to_path_buf());
        self.saved_version = self.version;
        self.disk_stamp = disk_stamp(path);
        Ok(())
    }

    pub fn is_dirty(&self) -> bool {
        self.version != self.saved_version
    }

    pub fn name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                // Like Sublime: an unsaved buffer is named after its first non-empty line.
                let first = (0..self.rope.len_lines().min(50)).map(|l| self.line_text(l)).find(|t| !t.trim().is_empty());
                match first {
                    Some(t) => {
                        let t = t.trim();
                        let mut n: String = t.chars().take(50).collect();
                        if t.chars().count() > 50 {
                            n.push('…');
                        }
                        n
                    }
                    None => "untitled".into(),
                }
            })
    }

    /// Treat the buffer as modified (e.g. restored unsaved content from a session).
    pub fn mark_dirty(&mut self) {
        self.saved_version = u64::MAX;
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn len(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns (and clears) the first line whose content changed.
    pub fn take_dirty_line(&mut self) -> Option<usize> {
        self.dirty_from_line.take()
    }

    // ---------- coordinates ----------

    pub fn line_of(&self, pos: usize) -> usize {
        self.rope.char_to_line(pos.min(self.len()))
    }

    pub fn line_start(&self, line: usize) -> usize {
        self.rope.line_to_char(line.min(self.rope.len_lines()))
    }

    /// End of line content, excluding the newline.
    pub fn line_end(&self, line: usize) -> usize {
        if line + 1 >= self.rope.len_lines() {
            self.len()
        } else {
            self.line_start(line + 1) - 1
        }
    }

    pub fn line_text(&self, line: usize) -> String {
        self.rope.slice(self.line_start(line)..self.line_end(line)).to_string()
    }

    pub fn col_of(&self, pos: usize) -> usize {
        pos - self.line_start(self.line_of(pos))
    }

    pub fn pos_at(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        let s = self.line_start(line);
        (s + col).min(self.line_end(line))
    }

    fn char_at(&self, pos: usize) -> Option<char> {
        (pos < self.len()).then(|| self.rope.char(pos))
    }

    // ---------- core editing ----------

    fn apply_raw(&mut self, changes: &[Change]) -> Vec<Change> {
        let mut inverse = Vec::with_capacity(changes.len());
        let mut delta: isize = 0;
        let mut removed = Vec::with_capacity(changes.len());
        for c in changes {
            removed.push(self.rope.slice(c.start..c.end).to_string());
        }
        if let Some(first) = changes.first() {
            let l = self.line_of(first.start);
            self.dirty_from_line = Some(self.dirty_from_line.map_or(l, |d| d.min(l)));
        }
        for c in changes.iter().rev() {
            self.rope.remove(c.start..c.end);
            self.rope.insert(c.start, &c.text);
        }
        for (c, rem) in changes.iter().zip(removed) {
            let ns = (c.start as isize + delta) as usize;
            let ins = c.text.chars().count();
            inverse.push(Change { start: ns, end: ns + ins, text: rem });
            delta += ins as isize - (c.end - c.start) as isize;
        }
        if !self.folds.is_empty() || !self.bookmarks.is_empty() {
            let map = |m: usize| -> usize {
                let mut shift: isize = 0;
                for c in changes {
                    if c.end <= m && !(c.start == c.end && c.start == m) {
                        shift += c.text.chars().count() as isize - (c.end - c.start) as isize;
                    } else if c.start < m {
                        // Mark inside a replaced range collapses to the change start.
                        return (c.start as isize + shift) as usize;
                    }
                }
                (m as isize + shift) as usize
            };
            let folds: Vec<_> = self.folds.iter().map(|&(a, b)| (map(a), map(b))).collect();
            self.folds = folds;
            let marks: Vec<_> = self.bookmarks.iter().map(|&m| map(m)).collect();
            self.bookmarks = marks;
            // Drop folds that became empty or no longer span a line break.
            let len = self.len();
            let rope = &self.rope;
            self.bookmarks.retain(|&m| m <= len);
            self.bookmarks.sort();
            self.bookmarks.dedup_by_key(|m| rope.char_to_line(*m));
            self.folds.retain(|&(a, b)| a < b && b <= len && rope.char_to_line(a) < rope.char_to_line(b));
        }
        self.version += 1;
        inverse
    }

    /// Apply a set of non-overlapping changes as one undoable step.
    /// `kind` lets consecutive steps of the same kind (e.g. typing) coalesce.
    pub fn transact(&mut self, mut changes: Vec<Change>, sels_after: Vec<Selection>, kind: Option<&'static str>) {
        changes.retain(|c| c.start != c.end || !c.text.is_empty());
        changes.sort_by_key(|c| c.start);
        let sels_before = self.sels.clone();
        let mut sels_after = sels_after;
        if changes.is_empty() {
            normalize(&mut sels_after);
            self.sels = sels_after;
            return;
        }
        let inverse = self.apply_raw(&changes);
        let len = self.len();
        for s in &mut sels_after {
            s.anchor = s.anchor.min(len);
            s.head = s.head.min(len);
        }
        normalize(&mut sels_after);
        self.sels = sels_after.clone();
        self.redo.clear();
        let txn = Txn { forward: changes, inverse };
        if let (Some(k), Some(last)) = (kind, self.undo.last_mut()) {
            if last.kind == Some(k) {
                last.txns.push(txn);
                last.sels_after = sels_after;
                return;
            }
        }
        self.undo.push(UndoEntry { txns: vec![txn], sels_before, sels_after, kind });
    }

    /// Stop coalescing: the next edit starts a new undo step.
    pub fn break_undo_group(&mut self) {
        if let Some(last) = self.undo.last_mut() {
            last.kind = None;
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(e) = self.undo.pop() else { return false };
        for t in e.txns.iter().rev() {
            self.apply_raw(&t.inverse);
        }
        self.sels = e.sels_before.clone();
        self.redo.push(UndoEntry { kind: None, ..e });
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(e) = self.redo.pop() else { return false };
        for t in &e.txns {
            self.apply_raw(&t.forward);
        }
        self.sels = e.sels_after.clone();
        self.undo.push(e);
        true
    }

    /// Replace each selection with the text produced by `f`, caret after it.
    fn replace_each(&mut self, kind: Option<&'static str>, mut f: impl FnMut(&Self, &Selection) -> String) {
        let mut changes = Vec::new();
        let mut sels = Vec::new();
        let mut delta: isize = 0;
        for s in &self.sels {
            let text = f(self, s);
            let n = text.chars().count();
            let ns = (s.start() as isize + delta) as usize;
            sels.push(Selection::caret(ns + n));
            delta += n as isize - (s.end() - s.start()) as isize;
            changes.push(Change { start: s.start(), end: s.end(), text });
        }
        self.transact(changes, sels, kind);
    }

    pub fn insert(&mut self, text: &str) {
        let kind = if text.chars().all(is_word_char) { Some("type") } else { None };
        self.replace_each(kind, |_, _| text.to_string());
    }

    /// Insert one string per selection (multi-cursor paste).
    pub fn insert_per_selection(&mut self, texts: &[String]) {
        let mut i = 0;
        self.replace_each(None, |_, _| {
            let t = texts[i.min(texts.len() - 1)].clone();
            i += 1;
            t
        });
    }

    pub fn newline(&mut self) {
        self.replace_each(None, |b, s| {
            let line = b.line_text(b.line_of(s.start()));
            let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            let prev = b.rope.slice(b.line_start(b.line_of(s.start()))..s.start()).to_string();
            let extra = if prev.trim_end().ends_with(['{', '(', '[', ':']) { b.indent_unit() } else { String::new() };
            format!("\n{indent}{extra}")
        });
    }

    pub fn indent_unit(&self) -> String {
        if self.translate_tabs { " ".repeat(self.tab_size) } else { "\t".into() }
    }

    pub fn tab(&mut self) {
        if self.sels.iter().any(|s| self.line_of(s.start()) != self.line_of(s.end())) {
            return self.indent();
        }
        self.replace_each(None, |b, s| {
            if b.translate_tabs {
                let col = b.col_of(s.start());
                " ".repeat(b.tab_size - col % b.tab_size)
            } else {
                "\t".into()
            }
        });
    }

    /// Delete each selection, or the range produced by `range` for empty ones.
    fn delete_with(&mut self, kind: Option<&'static str>, range: impl Fn(&Self, usize) -> (usize, usize)) {
        let ranges: Vec<_> = self
            .sels
            .iter()
            .map(|s| if s.is_empty() { range(self, s.head) } else { (s.start(), s.end()) })
            .collect();
        let mut changes = Vec::new();
        let mut sels = Vec::new();
        let mut delta: isize = 0;
        let mut last_end = 0;
        for (a, b) in ranges {
            let a = a.max(last_end);
            let b = b.max(a);
            let ns = (a as isize + delta) as usize;
            sels.push(Selection::caret(ns));
            delta -= (b - a) as isize;
            changes.push(Change { start: a, end: b, text: String::new() });
            last_end = b;
        }
        self.transact(changes, sels, kind);
    }

    pub fn backspace(&mut self) {
        // Inside an empty auto-pair like "(|)": delete both halves.
        if self.auto_pair
            && !self.sels.is_empty()
            && self.sels.iter().all(|s| s.is_empty() && s.head > 0 && s.head < self.len() && is_pair(self.rope.char(s.head - 1), self.rope.char(s.head)))
        {
            return self.delete_with(Some("delete"), |_, p| (p - 1, p + 1));
        }
        self.delete_with(Some("delete"), |b, p| {
            // Delete a whole indent unit when inside leading spaces.
            let col = b.col_of(p);
            let ls = p - col;
            let before = b.rope.slice(ls..p).to_string();
            if col > 0 && b.translate_tabs && before.chars().all(|c| c == ' ') {
                let n = ((col - 1) % b.tab_size) + 1;
                return (p - n, p);
            }
            (p.saturating_sub(1), p)
        });
    }

    pub fn delete_forward(&mut self) {
        self.delete_with(Some("delete"), |b, p| (p, (p + 1).min(b.len())));
    }

    pub fn delete_word_back(&mut self) {
        self.delete_with(None, |b, p| (b.word_left(p), p));
    }

    pub fn delete_word_forward(&mut self) {
        self.delete_with(None, |b, p| (p, b.word_right(p)));
    }

    // ---------- movement ----------

    pub fn word_left(&self, mut p: usize) -> usize {
        while p > 0 && self.char_at(p - 1).is_some_and(|c| c.is_whitespace() && c != '\n') {
            p -= 1;
        }
        let word = p > 0 && self.char_at(p - 1).is_some_and(is_word_char);
        if p > 0 && !word {
            return p - 1;
        }
        while p > 0 && self.char_at(p - 1).is_some_and(is_word_char) {
            p -= 1;
        }
        p
    }

    pub fn word_right(&self, mut p: usize) -> usize {
        let len = self.len();
        while p < len && self.char_at(p).is_some_and(|c| c.is_whitespace() && c != '\n') {
            p += 1;
        }
        if p < len && !self.char_at(p).is_some_and(is_word_char) {
            return p + 1;
        }
        while p < len && self.char_at(p).is_some_and(is_word_char) {
            p += 1;
        }
        p
    }

    pub fn word_at(&self, p: usize) -> (usize, usize) {
        let mut a = p;
        let mut b = p;
        while a > 0 && self.char_at(a - 1).is_some_and(is_word_char) {
            a -= 1;
        }
        while self.char_at(b).is_some_and(is_word_char) {
            b += 1;
        }
        (a, b)
    }

    fn move_each(&mut self, extend: bool, f: impl Fn(&Self, &Selection) -> (usize, Option<usize>)) {
        let mut sels: Vec<Selection> = self
            .sels
            .iter()
            .map(|s| {
                let (head, goal) = f(self, s);
                let anchor = if extend { s.anchor } else { head };
                Selection { anchor, head, goal_col: goal }
            })
            .collect();
        normalize(&mut sels);
        self.sels = sels;
        self.break_undo_group();
    }

    pub fn move_left(&mut self, extend: bool) {
        self.move_each(extend, |_, s| {
            if !extend && !s.is_empty() { (s.start(), None) } else { (s.head.saturating_sub(1), None) }
        });
    }

    pub fn move_right(&mut self, extend: bool) {
        let len = self.len();
        self.move_each(extend, |_, s| {
            if !extend && !s.is_empty() { (s.end(), None) } else { ((s.head + 1).min(len), None) }
        });
    }

    pub fn move_word_left(&mut self, extend: bool) {
        self.move_each(extend, |b, s| (b.word_left(s.head), None));
    }

    pub fn move_word_right(&mut self, extend: bool) {
        self.move_each(extend, |b, s| (b.word_right(s.head), None));
    }

    fn vertical(&self, s: &Selection, dl: isize) -> (usize, Option<usize>) {
        let line = self.line_of(s.head) as isize;
        let goal = s.goal_col.unwrap_or_else(|| self.col_of(s.head));
        let target = line + dl;
        if target < 0 {
            return (0, Some(goal));
        }
        if target as usize >= self.rope.len_lines() {
            return (self.len(), Some(goal));
        }
        (self.pos_at(target as usize, goal), Some(goal))
    }

    pub fn move_lines(&mut self, dl: isize, extend: bool) {
        self.move_each(extend, |b, s| b.vertical(s, dl));
    }

    pub fn move_home(&mut self, extend: bool) {
        self.move_each(extend, |b, s| {
            let line = b.line_of(s.head);
            let ls = b.line_start(line);
            let indent = b.line_text(line).chars().take_while(|c| c.is_whitespace()).count();
            let first = ls + indent;
            (if s.head == first { ls } else { first }, None)
        });
    }

    pub fn move_end(&mut self, extend: bool) {
        self.move_each(extend, |b, s| (b.line_end(b.line_of(s.head)), None));
    }

    pub fn move_doc_start(&mut self, extend: bool) {
        self.move_each(extend, |_, _| (0, None));
    }

    pub fn move_doc_end(&mut self, extend: bool) {
        let len = self.len();
        self.move_each(extend, |_, _| (len, None));
    }

    pub fn set_caret(&mut self, pos: usize) {
        self.sels = vec![Selection::caret(pos.min(self.len()))];
        self.break_undo_group();
    }

    pub fn select_all(&mut self) {
        self.sels = vec![Selection::new(0, self.len())];
    }

    /// Ctrl+L: extend each selection to full lines (including newline).
    pub fn expand_to_line(&mut self) {
        let mut sels: Vec<_> = self
            .sels
            .iter()
            .map(|s| {
                let a = self.line_start(self.line_of(s.start()));
                let b = (self.line_end(self.line_of(s.end())) + 1).min(self.len());
                Selection::new(a, b)
            })
            .collect();
        normalize(&mut sels);
        self.sels = sels;
    }

    /// Ctrl+D: select word under empty carets; otherwise add next occurrence.
    pub fn select_next_occurrence(&mut self) {
        if self.sels.iter().any(|s| s.is_empty()) {
            let mut sels: Vec<_> = self
                .sels
                .iter()
                .map(|s| if s.is_empty() { let (a, b) = self.word_at(s.head); Selection::new(a, b) } else { *s })
                .collect();
            normalize(&mut sels);
            self.sels = sels;
            return;
        }
        let last = *self.sels.last().unwrap();
        let needle = self.rope.slice(last.start()..last.end()).to_string();
        let hay = self.text();
        let from = self.rope.char_to_byte(last.end());
        let found = hay[from..].find(&needle).map(|i| i + from).or_else(|| hay.find(&needle));
        if let Some(bi) = found {
            let a = self.rope.byte_to_char(bi);
            let s = Selection::new(a, a + needle.chars().count());
            if !self.sels.contains(&s) {
                self.sels.push(s);
                normalize(&mut self.sels);
                // keep the newest selection last so repeated Ctrl+D keeps advancing
                if let Some(i) = self.sels.iter().position(|x| *x == s) {
                    let x = self.sels.remove(i);
                    self.sels.push(x);
                }
            }
        }
    }

    /// Sorted copy of the selections (selection order may be non-positional after Ctrl+D).
    pub fn sorted_sels(&self) -> Vec<Selection> {
        let mut v = self.sels.clone();
        v.sort_by_key(|s| s.start());
        v
    }

    pub fn select_all_occurrences(&mut self) {
        if self.sels.iter().any(|s| s.is_empty()) {
            self.select_next_occurrence();
        }
        let last = *self.sels.last().unwrap();
        let needle = self.rope.slice(last.start()..last.end()).to_string();
        if needle.is_empty() {
            return;
        }
        let hay = self.text();
        let n = needle.chars().count();
        let mut sels: Vec<_> = hay
            .match_indices(&needle)
            .map(|(bi, _)| {
                let a = self.rope.byte_to_char(bi);
                Selection::new(a, a + n)
            })
            .collect();
        normalize(&mut sels);
        self.sels = sels;
    }

    /// Ctrl+Shift+L
    pub fn split_into_lines(&mut self) {
        let mut out = Vec::new();
        for s in &self.sels {
            let (l0, l1) = (self.line_of(s.start()), self.line_of(s.end()));
            if l0 == l1 {
                out.push(*s);
                continue;
            }
            for l in l0..=l1 {
                let a = if l == l0 { s.start() } else { self.line_start(l) };
                let b = if l == l1 { s.end() } else { self.line_end(l) };
                if l == l1 && a == b && l != l0 {
                    continue;
                }
                out.push(Selection::new(a, b));
            }
        }
        normalize(&mut out);
        self.sels = out;
    }

    /// Ctrl+Alt+Up/Down
    pub fn add_cursor_vertical(&mut self, dl: isize) {
        let edge = if dl < 0 { self.sorted_sels()[0] } else { *self.sorted_sels().last().unwrap() };
        let line = self.line_of(edge.head) as isize + dl;
        if line < 0 || line as usize >= self.rope.len_lines() {
            return;
        }
        let goal = edge.goal_col.unwrap_or_else(|| self.col_of(edge.head));
        let p = self.pos_at(line as usize, goal);
        self.sels.push(Selection { anchor: p, head: p, goal_col: Some(goal) });
        normalize(&mut self.sels);
    }

    pub fn add_caret(&mut self, pos: usize) {
        self.sels.push(Selection::caret(pos.min(self.len())));
        normalize(&mut self.sels);
    }

    pub fn single_selection(&mut self) {
        if self.sels.len() > 1 {
            self.sels = vec![self.sels[0]];
        } else if let Some(s) = self.sels.first_mut() {
            *s = Selection::caret(s.head);
        }
    }

    // ---------- line operations ----------

    /// Distinct line ranges covered by selections, as (first_line, last_line).
    fn line_blocks(&self) -> Vec<(usize, usize)> {
        let mut blocks: Vec<(usize, usize)> = Vec::new();
        for s in self.sorted_sels() {
            let l0 = self.line_of(s.start());
            let mut l1 = self.line_of(s.end());
            if l1 > l0 && s.end() == self.line_start(l1) {
                l1 -= 1;
            }
            match blocks.last_mut() {
                Some(b) if l0 <= b.1 + 1 => b.1 = b.1.max(l1),
                _ => blocks.push((l0, l1)),
            }
        }
        blocks
    }

    fn shift_sels(&self, f: impl Fn(usize) -> usize) -> Vec<Selection> {
        self.sels.iter().map(|s| Selection { anchor: f(s.anchor), head: f(s.head), goal_col: None }).collect()
    }

    pub fn duplicate_lines(&mut self) {
        let mut changes = Vec::new();
        let mut inserts: Vec<(usize, usize)> = Vec::new(); // (pos, len)
        for (l0, l1) in self.line_blocks() {
            let a = self.line_start(l0);
            let b = self.line_end(l1);
            let text = format!("{}\n", self.rope.slice(a..b));
            inserts.push((a, text.chars().count()));
            changes.push(Change { start: a, end: a, text });
        }
        let sels = self.shift_sels(|p| p + inserts.iter().filter(|(a, _)| *a <= p).map(|(_, n)| n).sum::<usize>());
        self.transact(changes, sels, None);
    }

    pub fn delete_lines(&mut self) {
        let mut changes = Vec::new();
        let mut sels = Vec::new();
        let mut removed = 0;
        for (l0, l1) in self.line_blocks() {
            let a = self.line_start(l0);
            let mut b = (self.line_end(l1) + 1).min(self.len());
            let mut a2 = a;
            if b == self.len() && a > 0 && self.line_end(l1) == self.len() {
                a2 = a - 1; // last line: eat preceding newline instead
                b = self.len();
            }
            sels.push(Selection::caret(a2 - removed));
            removed += b - a2;
            changes.push(Change { start: a2, end: b, text: String::new() });
        }
        self.transact(changes, sels, None);
    }

    /// Ctrl+Shift+Up/Down
    pub fn swap_lines(&mut self, up: bool) {
        let blocks = self.line_blocks();
        let last_line = self.rope.len_lines() - 1;
        if (up && blocks[0].0 == 0) || (!up && blocks.last().unwrap().1 >= last_line) {
            return;
        }
        let mut changes = Vec::new();
        let mut shifts: Vec<(usize, usize, isize)> = Vec::new(); // (start, end, delta) for positions in block
        for (l0, l1) in blocks {
            let (a, b) = (self.line_start(l0), self.line_end(l1));
            let block = self.rope.slice(a..b).to_string();
            if up {
                let (pa, pb) = (self.line_start(l0 - 1), self.line_end(l0 - 1));
                let prev = self.rope.slice(pa..pb).to_string();
                changes.push(Change { start: pa, end: b, text: format!("{block}\n{prev}") });
                shifts.push((a, b, -((pb - pa + 1) as isize)));
            } else {
                let (na, nb) = (self.line_start(l1 + 1), self.line_end(l1 + 1));
                let next = self.rope.slice(na..nb).to_string();
                changes.push(Change { start: a, end: nb, text: format!("{next}\n{block}") });
                shifts.push((a, b, (nb - na + 1) as isize));
            }
        }
        let sels = self.shift_sels(|p| {
            shifts.iter().find(|(a, b, _)| *a <= p && p <= *b).map_or(p, |(_, _, d)| (p as isize + d) as usize)
        });
        // Overlapping changes can occur when blocks are adjacent; merge by applying one at a time is not
        // possible in one txn, so fall back to just the first if they overlap.
        let mut ok = true;
        for w in changes.windows(2) {
            if w[1].start < w[0].end {
                ok = false;
            }
        }
        if ok {
            self.transact(changes, sels, None);
        }
    }

    pub fn indent(&mut self) {
        let unit = self.indent_unit();
        let n = unit.chars().count();
        let mut changes = Vec::new();
        let mut starts = Vec::new();
        for (l0, l1) in self.line_blocks() {
            for l in l0..=l1 {
                let a = self.line_start(l);
                starts.push(a);
                changes.push(Change { start: a, end: a, text: unit.clone() });
            }
        }
        let sels = self.shift_sels(|p| p + n * starts.iter().filter(|a| **a <= p).count());
        self.transact(changes, sels, None);
    }

    pub fn unindent(&mut self) {
        let mut changes = Vec::new();
        for (l0, l1) in self.line_blocks() {
            for l in l0..=l1 {
                let a = self.line_start(l);
                let t = self.line_text(l);
                let n = if t.starts_with('\t') { 1 } else { t.chars().take(self.tab_size).take_while(|c| *c == ' ').count() };
                if n > 0 {
                    changes.push(Change { start: a, end: a + n, text: String::new() });
                }
            }
        }
        let sels = self.shift_sels(|p| {
            let mut q = p;
            for c in &changes {
                if c.start < p {
                    q -= (c.end.min(p)) - c.start;
                }
            }
            q
        });
        self.transact(changes, sels, None);
    }

    /// Ctrl+/ with a line-comment prefix such as "//" or "#".
    pub fn toggle_comment(&mut self, prefix: &str) {
        let blocks = self.line_blocks();
        let lines: Vec<usize> = blocks.iter().flat_map(|(a, b)| *a..=*b).filter(|l| !self.line_text(*l).trim().is_empty()).collect();
        if lines.is_empty() {
            return;
        }
        let all_commented = lines.iter().all(|l| self.line_text(*l).trim_start().starts_with(prefix));
        let mut changes = Vec::new();
        if all_commented {
            for l in lines {
                let t = self.line_text(l);
                let ind = t.chars().take_while(|c| c.is_whitespace()).count();
                let rest: String = t.chars().skip(ind + prefix.chars().count()).collect();
                let n = prefix.chars().count() + usize::from(rest.starts_with(' '));
                let a = self.line_start(l) + ind;
                changes.push(Change { start: a, end: a + n, text: String::new() });
            }
        } else {
            let ind = lines.iter().map(|l| self.line_text(*l).chars().take_while(|c| c.is_whitespace()).count()).min().unwrap();
            for l in lines {
                let a = self.line_start(l) + ind;
                changes.push(Change { start: a, end: a, text: format!("{prefix} ") });
            }
        }
        let sels = self.shift_sels(|p| {
            let mut d: isize = 0;
            for c in &changes {
                if c.start < p || (c.start == p && c.end == c.start) {
                    d += c.text.chars().count() as isize - (c.end.min(p.max(c.start)) - c.start) as isize;
                }
            }
            (p as isize + d).max(0) as usize
        });
        self.transact(changes, sels, None);
    }

    /// Ctrl+J
    pub fn join_lines(&mut self) {
        let mut changes = Vec::new();
        for (l0, l1) in self.line_blocks() {
            let l1 = if l1 == l0 { l0 + 1 } else { l1 };
            for l in l0 + 1..=l1.min(self.rope.len_lines() - 1) {
                let a = self.line_end(l - 1);
                let t = self.line_text(l);
                let ind = t.chars().take_while(|c| c.is_whitespace()).count();
                let sep = if t.trim().is_empty() { "" } else { " " };
                let trail = self.line_text(l - 1).chars().rev().take_while(|c| *c == ' ' || *c == '\t').count();
                changes.push(Change { start: a - trail, end: self.line_start(l) + ind, text: sep.into() });
            }
        }
        let first = changes.first().map(|c| c.start + c.text.chars().count());
        let sels = if self.sels.len() == 1 && self.sels[0].is_empty() { vec![Selection::caret(first.unwrap_or(self.sels[0].head))] } else { self.sels.clone() };
        self.transact(changes, sels, None);
    }

    /// Ctrl+Enter / Ctrl+Shift+Enter
    pub fn insert_line(&mut self, before: bool) {
        let mut changes = Vec::new();
        let mut sels = Vec::new();
        let mut delta = 0;
        for s in self.sorted_sels() {
            let l = self.line_of(s.head);
            let indent: String = self.line_text(l).chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            let n = indent.chars().count();
            if before {
                let a = self.line_start(l);
                sels.push(Selection::caret(a + delta + n));
                delta += n + 1;
                changes.push(Change { start: a, end: a, text: format!("{indent}\n") });
            } else {
                let a = self.line_end(l);
                sels.push(Selection::caret(a + delta + 1 + n));
                delta += n + 1;
                changes.push(Change { start: a, end: a, text: format!("\n{indent}") });
            }
        }
        self.transact(changes, sels, None);
    }

    /// Delete from caret to end of line (Ctrl+K Ctrl+K) or to start (Ctrl+Shift+Backspace).
    pub fn delete_to_line_edge(&mut self, to_end: bool) {
        self.delete_with(None, |b, p| {
            let l = b.line_of(p);
            if to_end {
                let e = b.line_end(l);
                (p, if e == p { (p + 1).min(b.len()) } else { e })
            } else {
                (b.line_start(l), p)
            }
        });
    }

    /// F9: sort selected lines (whole buffer when nothing selected).
    pub fn sort_lines(&mut self, case_sensitive: bool) {
        let (l0, l1) = if self.sels.iter().all(|s| s.is_empty()) {
            (0, self.rope.len_lines() - 1)
        } else {
            let b = self.line_blocks();
            (b[0].0, b.last().unwrap().1)
        };
        let mut lines: Vec<String> = (l0..=l1).map(|l| self.line_text(l)).collect();
        if case_sensitive {
            lines.sort();
        } else {
            lines.sort_by_key(|a| a.to_lowercase());
        }
        let (a, b) = (self.line_start(l0), self.line_end(l1));
        let sels = self.sels.clone();
        self.transact(vec![Change { start: a, end: b, text: lines.join("\n") }], sels, None);
    }

    /// Brackets to highlight for a caret at `p`: the bracket just after or before it and its partner.
    pub fn matching_brackets(&self, p: usize) -> Option<(usize, usize)> {
        if self.char_at(p).is_some_and(|c| "([{".contains(c)) {
            return self.bracket_match(p + 1).filter(|(o, _)| *o == p);
        }
        if p > 0 && self.char_at(p - 1).is_some_and(|c| ")]}".contains(c)) {
            return self.bracket_match(p - 1).filter(|(_, c)| *c == p - 1);
        }
        None
    }

    fn bracket_match(&self, p: usize) -> Option<(usize, usize)> {
        // Find the innermost bracket pair enclosing p; returns (open, close) positions.
        let pairs = [('(', ')'), ('[', ']'), ('{', '}')];
        let mut depth = [0i32; 3];
        let mut i = p;
        while i > 0 {
            i -= 1;
            let c = self.rope.char(i);
            for (k, (o, cl)) in pairs.iter().enumerate() {
                if c == *cl {
                    depth[k] += 1;
                } else if c == *o {
                    if depth[k] == 0 {
                        let mut d = 0;
                        for j in i + 1..self.len() {
                            let cj = self.rope.char(j);
                            if cj == *o {
                                d += 1;
                            } else if cj == *cl {
                                if d == 0 {
                                    return Some((i, j));
                                }
                                d -= 1;
                            }
                        }
                        return None;
                    }
                    depth[k] -= 1;
                }
            }
        }
        None
    }

    /// Ctrl+M
    pub fn jump_to_bracket(&mut self) {
        let mut sels = Vec::new();
        for s in &self.sels {
            let p = s.head;
            let at = |q: usize| self.char_at(q);
            let target = if at(p).is_some_and(|c| "([{".contains(c)) {
                self.bracket_match(p + 1).map(|(_, c)| c + 1)
            } else if p > 0 && at(p - 1).is_some_and(|c| ")]}".contains(c)) {
                self.bracket_match(p - 1).map(|(o, _)| o)
            } else {
                self.bracket_match(p).map(|(_, c)| c)
            };
            sels.push(Selection::caret(target.unwrap_or(p)));
        }
        normalize(&mut sels);
        self.sels = sels;
    }

    /// Ctrl+Shift+M
    pub fn expand_to_brackets(&mut self) {
        let mut sels: Vec<_> = self
            .sels
            .iter()
            .map(|s| match self.bracket_match(s.start()) {
                Some((o, c)) if (o + 1, c) == (s.start(), s.end()) => self.bracket_match(o).map_or(*s, |(o, c)| Selection::new(o + 1, c)),
                Some((o, c)) => Selection::new(o + 1, c),
                None => *s,
            })
            .collect();
        normalize(&mut sels);
        self.sels = sels;
    }

    /// Ctrl+K Ctrl+D: drop the newest selection and add the next occurrence.
    pub fn skip_occurrence(&mut self) {
        if self.sels.len() > 1 {
            let last = self.sels.pop().unwrap();
            self.sels.push(last);
            let removed = self.sels.pop().unwrap();
            self.sels.push(removed);
            self.select_next_occurrence();
            if let Some(i) = self.sels.iter().position(|x| *x == removed) {
                self.sels.remove(i);
            }
        } else {
            self.select_next_occurrence();
        }
    }

    /// Typed text with bracket/quote auto-pairing, wrapping and over-typing (like Sublime).
    pub fn type_text(&mut self, text: &str) {
        let mut chars = text.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else { return self.insert(text) };
        if !self.auto_pair {
            return self.insert(text);
        }
        let close = match c {
            '(' => Some(')'),
            '[' => Some(']'),
            '{' => Some('}'),
            '"' | '\'' | '`' => Some(c),
            _ => None,
        };
        let next = |b: &Self, p: usize| b.char_at(p);
        // Over-type a closing char that is already there.
        if (")]}\"'`".contains(c)) && self.sels.iter().all(|s| s.is_empty() && next(self, s.head) == Some(c)) {
            if !"\"'`".contains(c) || self.sels.iter().all(|s| s.head > 0 && next(self, s.head - 1) != Some('\\')) {
                return self.move_right(false);
            }
        }
        let Some(close) = close else { return self.insert(text) };
        // Wrap non-empty selections.
        if self.sels.iter().all(|s| !s.is_empty()) {
            let mut changes = Vec::new();
            let mut sels = Vec::new();
            let mut delta = 0;
            for s in self.sorted_sels() {
                let inner = self.rope.slice(s.start()..s.end()).to_string();
                let n = inner.chars().count();
                sels.push(Selection::new(s.start() + delta + 1, s.start() + delta + 1 + n));
                delta += 2;
                changes.push(Change { start: s.start(), end: s.end(), text: format!("{c}{inner}{close}") });
            }
            return self.transact(changes, sels, None);
        }
        let ok = self.sels.iter().all(|s| {
            let after = next(self, s.head);
            let after_ok = after.is_none_or(|a| a.is_whitespace() || ")]};,:".contains(a));
            if c == close {
                // Quotes: don't pair after a word char (e.g. don't) or inside a word.
                let before = (s.head > 0).then(|| self.rope.char(s.head - 1));
                after_ok && before.is_none_or(|b| !is_word_char(b) && b != c)
            } else {
                after_ok
            }
        });
        if !ok {
            return self.insert(text);
        }
        let pair = format!("{c}{close}");
        let mut changes = Vec::new();
        let mut sels = Vec::new();
        let mut delta = 0;
        for s in self.sorted_sels() {
            sels.push(Selection::caret(s.head + delta + 1));
            delta += 2;
            changes.push(Change { start: s.head, end: s.head, text: pair.clone() });
        }
        self.transact(changes, sels, None);
    }

    /// The word fragment ending at `p` (for completion).
    pub fn prefix_at(&self, p: usize) -> String {
        let (a, _) = self.word_at(p);
        self.rope.slice(a..p).to_string()
    }

    /// Distinct words (>= 3 chars) in the buffer, most frequent first.
    pub fn words(&self) -> Vec<String> {
        let text = if self.len() > 4_000_000 { self.rope.slice(..4_000_000).to_string() } else { self.text() };
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for w in text.split(|c: char| !is_word_char(c)) {
            if w.chars().count() >= 3 && !w.starts_with(|c: char| c.is_ascii_digit()) {
                *counts.entry(w).or_default() += 1;
            }
        }
        let mut v: Vec<(&str, usize)> = counts.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        v.into_iter().map(|(w, _)| w.to_string()).collect()
    }

    /// Replace the word fragment before every caret with `word`.
    pub fn complete_with(&mut self, word: &str) {
        let mut sels: Vec<Selection> = self.sels.iter().map(|s| {
            let (a, _) = self.word_at(s.head);
            Selection::new(a, s.head)
        }).collect();
        normalize(&mut sels);
        self.sels = sels;
        self.insert(word);
        self.break_undo_group();
    }

    /// Column (box) selection between two (line, visual column) corners.
    pub fn column_select(&mut self, (l0, v0): (usize, f32), (l1, v1): (usize, f32)) {
        let (lo, hi) = (l0.min(l1), l0.max(l1));
        let mut sels = Vec::new();
        for l in lo..=hi.min(self.rope.len_lines() - 1) {
            let text = self.line_text(l);
            let ls = self.line_start(l);
            let col = |v: f32| {
                let mut vis = 0;
                for (i, ch) in text.chars().enumerate() {
                    let w = if ch == '\t' { self.tab_size - vis % self.tab_size } else { 1 };
                    if v < vis as f32 + w as f32 / 2.0 {
                        return i;
                    }
                    vis += w;
                }
                text.chars().count()
            };
            let (a, b) = (col(v0), col(v1));
            // Skip short lines unless the box is zero-width (a column of carets).
            if a == b && (v0 - v1).abs() > 0.5 && a == text.chars().count() {
                continue;
            }
            sels.push(Selection::new(ls + a, ls + b));
        }
        if !sels.is_empty() {
            self.sels = sels;
        }
    }

    // ---------- folding ----------

    fn indent_of(&self, l: usize) -> Option<usize> {
        let t = self.line_text(l);
        if t.trim().is_empty() {
            return None;
        }
        let mut v = 0;
        for c in t.chars() {
            match c {
                ' ' => v += 1,
                '\t' => v += self.tab_size - v % self.tab_size,
                _ => break,
            }
        }
        Some(v)
    }

    /// The block that line `l` opens: the following lines indented deeper (Sublime folds by indentation).
    pub fn fold_range_at(&self, l: usize) -> Option<(usize, usize)> {
        let ind = self.indent_of(l)?;
        let n = self.rope.len_lines();
        let mut last = None;
        let mut i = l + 1;
        while i < n {
            match self.indent_of(i) {
                Some(x) if x <= ind => break,
                Some(_) => last = Some(i),
                None => {}
            }
            i += 1;
        }
        // Keep a closing "}" / "end" line visible like Sublime does.
        last.map(|e| (l, e))
    }

    pub fn is_foldable(&self, l: usize) -> bool {
        self.fold_range_at(l).is_some()
    }

    pub fn fold_lines(&mut self, l0: usize, l1: usize) {
        if l1 <= l0 {
            return;
        }
        let a = self.line_end(l0);
        let b = self.line_end(l1);
        self.folds.retain(|&(x, y)| !(x >= a && y <= b));
        self.folds.push((a, b));
        self.folds.sort();
        // Move carets out of the hidden region.
        for s in &mut self.sels {
            if s.head > a && s.head <= b {
                *s = Selection::caret(a);
            }
        }
    }

    /// Ctrl+Shift+[ : fold the block at (or enclosing) each caret.
    pub fn fold(&mut self) {
        for s in self.sorted_sels() {
            let mut l = self.line_of(s.head);
            loop {
                if let Some((a, b)) = self.fold_range_at(l) {
                    if !self.is_folded(a) {
                        self.fold_lines(a, b);
                        break;
                    }
                }
                // Walk up to the enclosing block header.
                let ind = self.indent_of(l).unwrap_or(usize::MAX);
                let Some(h) = (0..l).rev().find(|&i| self.indent_of(i).is_some_and(|x| x < ind)) else { break };
                if self.fold_range_at(h).is_some_and(|(_, e)| e >= self.line_of(s.head)) && !self.is_folded(h) {
                    let (a, b) = self.fold_range_at(h).unwrap();
                    self.fold_lines(a, b);
                    break;
                }
                l = h;
            }
        }
    }

    /// Ctrl+Shift+] : unfold folds at the carets' lines.
    pub fn unfold(&mut self) {
        let lines: Vec<usize> = self.sels.iter().map(|s| self.line_of(s.head)).collect();
        let rope = &self.rope;
        self.folds.retain(|&(a, _)| !lines.contains(&rope.char_to_line(a)));
    }

    pub fn unfold_all(&mut self) {
        self.folds.clear();
    }

    /// Ctrl+K Ctrl+1..9: fold every block whose header is at the given nesting level (1 = top level).
    pub fn fold_level(&mut self, level: usize) {
        let unit = self.tab_size.max(1);
        let target = (level - 1) * unit;
        let n = self.rope.len_lines();
        let mut l = 0;
        while l < n {
            if self.indent_of(l) == Some(target) {
                if let Some((a, b)) = self.fold_range_at(l) {
                    self.fold_lines(a, b);
                    l = b + 1;
                    continue;
                }
            }
            l += 1;
        }
    }

    pub fn is_folded(&self, l: usize) -> bool {
        self.folds.iter().any(|&(a, _)| self.rope.char_to_line(a) == l)
    }

    /// Folded line ranges (header, last hidden line), sorted.
    pub fn folded_lines(&self) -> Vec<(usize, usize)> {
        let mut v: Vec<_> = self.folds.iter().map(|&(a, b)| (self.rope.char_to_line(a), self.rope.char_to_line(b))).collect();
        v.sort();
        v
    }

    /// Unfold any fold that hides a caret (e.g. after search or goto).
    pub fn reveal_carets(&mut self) {
        let heads: Vec<usize> = self.sels.iter().map(|s| s.head).collect();
        self.folds.retain(|&(a, b)| !heads.iter().any(|&h| h > a && h <= b));
    }

    // ---------- bookmarks ----------

    pub fn bookmark_lines(&self) -> Vec<usize> {
        self.bookmarks.iter().map(|&m| self.rope.char_to_line(m.min(self.len()))).collect()
    }

    /// Ctrl+F2
    pub fn toggle_bookmark(&mut self) {
        for l in self.sels.iter().map(|s| self.line_of(s.head)).collect::<std::collections::BTreeSet<_>>() {
            let ls = self.line_start(l);
            if let Some(i) = self.bookmark_lines().iter().position(|&b| b == l) {
                self.bookmarks.remove(i);
            } else {
                self.bookmarks.push(ls);
            }
        }
        self.bookmarks.sort();
    }

    /// F2 / Shift+F2, wrapping around.
    pub fn goto_bookmark(&mut self, back: bool) {
        let mut lines = self.bookmark_lines();
        if lines.is_empty() {
            return;
        }
        lines.sort();
        let cur = self.line_of(self.sels.last().unwrap().head);
        let l = if back {
            lines.iter().rev().find(|&&l| l < cur).or(lines.last())
        } else {
            lines.iter().find(|&&l| l > cur).or(lines.first())
        };
        let p = self.line_start(*l.unwrap());
        self.set_caret(p);
    }

    pub fn clear_bookmarks(&mut self) {
        self.bookmarks.clear();
    }

    /// Alt+F2
    pub fn select_bookmarks(&mut self) {
        let mut sels: Vec<_> = self.bookmark_lines().into_iter().map(|l| Selection::caret(self.line_start(l))).collect();
        if !sels.is_empty() {
            normalize(&mut sels);
            self.sels = sels;
        }
    }

    pub fn selected_texts(&self) -> Vec<String> {
        self.sorted_sels().iter().map(|s| self.rope.slice(s.start()..s.end()).to_string()).collect()
    }

    // ---------- search ----------

    /// All matches as char ranges.
    pub fn find_all(&self, query: &Query) -> Vec<(usize, usize)> {
        let Some(re) = query.regex() else { return Vec::new() };
        let hay = self.text();
        let mut out = Vec::new();
        // Convert byte offsets incrementally to keep this linear.
        let (mut last_b, mut last_c) = (0usize, 0usize);
        for m in re.find_iter(&hay) {
            if m.start() == m.end() {
                continue;
            }
            last_c += hay[last_b..m.start()].chars().count();
            last_b = m.start();
            let a = last_c;
            let b = a + m.as_str().chars().count();
            out.push((a, b));
        }
        out
    }

    /// Select the next match after the primary caret (or previous when `back`).
    pub fn find_next(&mut self, query: &Query, back: bool) -> bool {
        let matches = self.find_all(query);
        if matches.is_empty() {
            return false;
        }
        let s = self.sels.last().copied().unwrap_or(Selection::caret(0));
        let m = if back {
            matches.iter().rev().find(|m| m.1 <= s.start()).or(matches.last())
        } else {
            matches.iter().find(|m| m.0 >= s.end()).or(matches.first())
        };
        let (a, b) = *m.unwrap();
        self.sels = vec![Selection::new(a, b)];
        true
    }

    pub fn find_all_select(&mut self, query: &Query) -> bool {
        let m = self.find_all(query);
        if m.is_empty() {
            return false;
        }
        self.sels = m.into_iter().map(|(a, b)| Selection::new(a, b)).collect();
        true
    }

    pub fn replace_all(&mut self, query: &Query, replacement: &str) -> usize {
        let Some(re) = query.regex() else { return 0 };
        let hay = self.text();
        let mut changes = Vec::new();
        for caps in re.captures_iter(&hay) {
            let m = caps.get(0).unwrap();
            if m.start() == m.end() {
                continue;
            }
            let mut text = String::new();
            if query.regex {
                caps.expand(replacement, &mut text);
            } else {
                text = replacement.to_string();
            }
            let a = self.rope.byte_to_char(m.start());
            let b = self.rope.byte_to_char(m.end());
            changes.push(Change { start: a, end: b, text });
        }
        let n = changes.len();
        let caret = self.sels[0].head.min(self.len());
        self.transact(changes, vec![Selection::caret(caret)], None);
        n
    }

    /// Replace the current selection if it is a match, then find the next one.
    pub fn replace_next(&mut self, query: &Query, replacement: &str) {
        let s = *self.sels.last().unwrap();
        if self.find_all(query).contains(&(s.start(), s.end())) {
            let selected = self.rope.slice(s.start()..s.end()).to_string();
            let text = match (query.regex, query.regex()) {
                (true, Some(re)) => re.replace(&selected, replacement).into_owned(),
                _ => replacement.to_string(),
            };
            self.sels = vec![s];
            self.replace_each(None, |_, _| text.clone());
        }
        self.find_next(query, false);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

impl Query {
    pub fn regex(&self) -> Option<regex::Regex> {
        if self.text.is_empty() {
            return None;
        }
        let mut pat = if self.regex { self.text.clone() } else { regex::escape(&self.text) };
        if self.whole_word {
            pat = format!(r"\b(?:{pat})\b");
        }
        regex::RegexBuilder::new(&pat).case_insensitive(!self.case_sensitive).multi_line(true).build().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

    #[test]
    fn insert_and_undo_redo() {
        let mut b = buf("hello");
        b.set_caret(5);
        b.insert(" world");
        assert_eq!(b.text(), "hello world");
        assert!(b.undo());
        assert_eq!(b.text(), "hello");
        assert!(b.redo());
        assert_eq!(b.text(), "hello world");
    }

    #[test]
    fn typing_coalesces() {
        let mut b = buf("");
        for c in ["a", "b", "c"] {
            b.insert(c);
        }
        b.undo();
        assert_eq!(b.text(), "");
    }

    #[test]
    fn multi_cursor_insert() {
        let mut b = buf("aa\nbb\ncc");
        b.sels = vec![Selection::caret(0), Selection::caret(3), Selection::caret(6)];
        b.insert("> ");
        assert_eq!(b.text(), "> aa\n> bb\n> cc");
        assert_eq!(b.sels.iter().map(|s| s.head).collect::<Vec<_>>(), vec![2, 7, 12]);
        b.undo();
        assert_eq!(b.text(), "aa\nbb\ncc");
    }

    #[test]
    fn backspace_multi() {
        let mut b = buf("ab\ncd");
        b.sels = vec![Selection::caret(2), Selection::caret(5)];
        b.backspace();
        assert_eq!(b.text(), "a\nc");
    }

    #[test]
    fn backspace_soft_tab() {
        let mut b = buf("        x");
        b.set_caret(8);
        b.backspace();
        assert_eq!(b.text(), "    x");
    }

    #[test]
    fn ctrl_d_selects_word_then_next() {
        let mut b = buf("foo bar foo baz foo");
        b.set_caret(1);
        b.select_next_occurrence();
        assert_eq!(b.sels, vec![Selection::new(0, 3)]);
        b.select_next_occurrence();
        b.select_next_occurrence();
        assert_eq!(b.sels.len(), 3);
        b.insert("x");
        assert_eq!(b.text(), "x bar x baz x");
    }

    #[test]
    fn split_lines() {
        let mut b = buf("ab\ncd\nef");
        b.sels = vec![Selection::new(0, 8)];
        b.split_into_lines();
        assert_eq!(b.sels.len(), 3);
    }

    #[test]
    fn duplicate_and_delete_line() {
        let mut b = buf("one\ntwo");
        b.set_caret(1);
        b.duplicate_lines();
        assert_eq!(b.text(), "one\none\ntwo");
        assert_eq!(b.sels[0].head, 5);
        b.delete_lines();
        assert_eq!(b.text(), "one\ntwo");
    }

    #[test]
    fn swap_lines_up_down() {
        let mut b = buf("a\nb\nc");
        b.set_caret(2);
        b.swap_lines(true);
        assert_eq!(b.text(), "b\na\nc");
        assert_eq!(b.sels[0].head, 0);
        b.swap_lines(false);
        assert_eq!(b.text(), "a\nb\nc");
        assert_eq!(b.sels[0].head, 2);
    }

    #[test]
    fn indent_unindent() {
        let mut b = buf("a\nb");
        b.select_all();
        b.indent();
        assert_eq!(b.text(), "    a\n    b");
        b.unindent();
        assert_eq!(b.text(), "a\nb");
    }

    #[test]
    fn comment_toggle() {
        let mut b = buf("  x\n  y");
        b.select_all();
        b.toggle_comment("//");
        assert_eq!(b.text(), "  // x\n  // y");
        b.toggle_comment("//");
        assert_eq!(b.text(), "  x\n  y");
    }

    #[test]
    fn newline_autoindent() {
        let mut b = buf("fn x() {");
        b.set_caret(8);
        b.newline();
        assert_eq!(b.text(), "fn x() {\n    ");
    }

    #[test]
    fn vertical_goal_column() {
        let mut b = buf("abcdef\nab\nabcdef");
        b.set_caret(5);
        b.move_lines(1, false);
        assert_eq!(b.sels[0].head, 9);
        b.move_lines(1, false);
        assert_eq!(b.sels[0].head, 15);
    }

    #[test]
    fn word_motion() {
        let b = buf("foo.bar baz");
        assert_eq!(b.word_right(0), 3);
        assert_eq!(b.word_right(3), 4);
        assert_eq!(b.word_left(11), 8);
    }

    #[test]
    fn find_and_replace() {
        let mut b = buf("Foo foo fOO");
        let q = Query { text: "foo".into(), ..Default::default() };
        assert_eq!(b.find_all(&q).len(), 3);
        let q2 = Query { case_sensitive: true, ..q.clone() };
        assert_eq!(b.find_all(&q2), vec![(4, 7)]);
        assert_eq!(b.replace_all(&q, "x"), 3);
        assert_eq!(b.text(), "x x x");
        let mut b = buf("a1 b22");
        let q = Query { text: r"(\w)(\d+)".into(), regex: true, ..Default::default() };
        b.replace_all(&q, "$2$1");
        assert_eq!(b.text(), "1a 22b");
    }

    #[test]
    fn unicode_find() {
        let b = buf("αβγ foo");
        let q = Query { text: "foo".into(), ..Default::default() };
        assert_eq!(b.find_all(&q), vec![(4, 7)]);
    }

    #[test]
    fn dirty_tracking() {
        let mut b = buf("x");
        assert!(!b.is_dirty());
        b.insert("y");
        assert!(b.is_dirty());
    }
    #[test]
    fn untitled_name_from_first_line() {
        let mut b = buf("");
        assert_eq!(b.name(), "untitled");
        b.insert("\n  hello world  ");
        assert_eq!(b.name(), "hello world");
    }

    #[test]
    fn join_and_insert_line() {
        let mut b = buf("a  \n   b");
        b.join_lines();
        assert_eq!(b.text(), "a b");
        let mut b = buf("  x");
        b.set_caret(1);
        b.insert_line(false);
        assert_eq!(b.text(), "  x\n  ");
        b.set_caret(0);
        b.insert_line(true);
        assert_eq!(b.text(), "  \n  x\n  ");
    }

    #[test]
    fn brackets() {
        let mut b = buf("f(a, [b])");
        b.set_caret(6);
        b.expand_to_brackets();
        assert_eq!(b.selected_texts(), vec!["b"]);
        b.expand_to_brackets();
        assert_eq!(b.selected_texts(), vec!["a, [b]"]);
        b.set_caret(1);
        b.jump_to_bracket();
        assert_eq!(b.sels[0].head, 9);
    }

    #[test]
    fn sort_and_delete_to_eol() {
        let mut b = buf("b\nC\na");
        b.sort_lines(false);
        assert_eq!(b.text(), "a\nb\nC");
        b.set_caret(0);
        b.delete_to_line_edge(true);
        assert_eq!(b.text(), "\nb\nC");
    }
    #[test]
    fn auto_pair_and_overtype() {
        let mut b = buf("");
        b.type_text("(");
        assert_eq!(b.text(), "()");
        assert_eq!(b.sels[0].head, 1);
        b.type_text("x");
        b.type_text(")");
        assert_eq!(b.text(), "(x)");
        assert_eq!(b.sels[0].head, 3);
        let mut b = buf("don");
        b.set_caret(3);
        b.type_text("'");
        assert_eq!(b.text(), "don'");
        let mut b = buf("");
        b.type_text("[");
        b.backspace();
        assert_eq!(b.text(), "");
    }

    #[test]
    fn auto_pair_wraps_selection() {
        let mut b = buf("abc");
        b.select_all();
        b.type_text("\"");
        assert_eq!(b.text(), "\"abc\"");
        assert_eq!(b.selected_texts(), vec!["abc"]);
    }

    #[test]
    fn matching_brackets_and_words() {
        let b = buf("f(a[1])");
        assert_eq!(b.matching_brackets(1), Some((1, 6)));
        assert_eq!(b.matching_brackets(7), Some((1, 6)));
        assert_eq!(b.matching_brackets(6), Some((3, 5)));
        let mut b = buf("alpha alpine al");
        assert_eq!(b.words(), vec!["alpha", "alpine"]);
        b.set_caret(15);
        assert_eq!(b.prefix_at(15), "al");
        b.complete_with("alpine");
        assert_eq!(b.text(), "alpha alpine alpine");
    }

    #[test]
    fn column_selection() {
        let mut b = buf("abcd\nab\nabcd");
        b.column_select((0, 1.0), (2, 3.0));
        assert_eq!(b.selected_texts(), vec!["bc", "b", "bc"]);
    }

    #[test]
    fn folds_follow_edits() {
        let mut b = buf("fn a() {\n    x;\n    y;\n}\nz");
        b.set_caret(0);
        b.fold();
        assert_eq!(b.folded_lines(), vec![(0, 2)]);
        b.set_caret(0);
        b.insert("// c\n");
        assert_eq!(b.folded_lines(), vec![(1, 3)]);
        b.undo();
        assert_eq!(b.folded_lines(), vec![(0, 2)]);
        b.unfold_all();
        b.fold_level(1);
        assert_eq!(b.folded_lines(), vec![(0, 2)]);
    }
    #[test]
    fn bookmarks_follow_edits() {
        let mut b = buf("a\nb\nc");
        b.set_caret(2);
        b.toggle_bookmark();
        assert_eq!(b.bookmark_lines(), vec![1]);
        b.set_caret(0);
        b.insert("x\n");
        assert_eq!(b.bookmark_lines(), vec![2]);
        b.set_caret(0);
        b.goto_bookmark(false);
        assert_eq!(b.line_of(b.sels[0].head), 2);
        b.toggle_bookmark();
        assert!(b.bookmark_lines().is_empty());
    }
    #[test]
    fn roundtrip_encoding_line_endings_and_reload() {
        let dir = std::env::temp_dir().join(format!("sublimers-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("greek.txt");
        std::fs::write(&p, crate::encoding::encode("Γεια σου κόσμε\r\nδεύτερη γραμμή", "Greek (Windows 1253)")).unwrap();
        let mut b = Buffer::open(&p).unwrap();
        assert_eq!(b.encoding, "Greek (Windows 1253)");
        assert_eq!(b.line_ending, LineEnding::Windows);
        assert_eq!(b.text(), "Γεια σου κόσμε\nδεύτερη γραμμή");
        b.set_caret(0);
        b.insert("Α");
        b.save().unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert!(raw.windows(2).any(|w| w == b"\r\n"));
        assert_eq!(crate::encoding::decode(&raw, Some("Greek (Windows 1253)")).0, "ΑΓεια σου κόσμε\r\nδεύτερη γραμμή");
        assert_eq!(b.disk_state(), DiskState::Unchanged);
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&p, "changed outside\n").unwrap();
        assert_eq!(b.disk_state(), DiskState::Modified);
        b.reload(None).unwrap();
        assert_eq!(b.text(), "changed outside\n");
        assert!(!b.is_dirty());
        b.undo();
        assert!(b.text().starts_with("ΑΓεια"));
        std::fs::remove_file(&p).unwrap();
        assert_eq!(b.disk_state(), DiskState::Deleted);
        std::fs::remove_dir_all(&dir).ok();
    }
}

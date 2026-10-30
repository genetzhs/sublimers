//! The text view: rendering, keyboard/mouse input, scrolling, word wrap, folding,
//! completion popup and minimap.

use eframe::egui::{self, pos2, vec2, Color32, FontId, Id, Rect, Sense, Stroke};
use egui::text::{LayoutJob, TextFormat};
use sublimers_core::buffer::is_word_char;
use sublimers_core::{Buffer, Selection};
use sublimers_syntax::{Color, Syntaxes};
use syntect::highlighting::Theme;

use crate::settings::Settings;
use crate::Doc;

const MAX_RENDER_CHARS: usize = 4000;
pub const MAX_GROUPS: usize = 4;

pub fn c(c: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

pub fn editor_id(group: usize) -> Id {
    Id::new(("editor_area", group))
}

pub fn editor_has_focus(ctx: &egui::Context) -> bool {
    ctx.memory(|m| (0..MAX_GROUPS).any(|g| m.has_focus(editor_id(g))))
}

/// Visual column of char column `col` with tabs expanded.
fn vis_col(line: &str, col: usize, tab: usize) -> usize {
    let mut v = 0;
    for ch in line.chars().take(col) {
        v += if ch == '\t' { tab - v % tab } else { 1 };
    }
    v
}

fn col_from_vis(line: &str, target: f32, tab: usize) -> usize {
    let mut v = 0;
    for (i, ch) in line.chars().enumerate() {
        let w = if ch == '\t' { tab - v % tab } else { 1 };
        if target < v as f32 + w as f32 / 2.0 {
            return i;
        }
        v += w;
    }
    line.chars().count()
}

// ---------- display rows (word wrap + folding) ----------

/// One screen row: chars `start..end` of `line`. `end == usize::MAX` means "to end of line".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Row {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Default)]
pub struct RowCache {
    key: Option<(u64, Vec<(usize, usize)>, usize)>,
    rows: Vec<Row>,
}

enum Rows<'a> {
    /// No wrap, no folds: row i is line i.
    Identity(usize),
    List(&'a [Row]),
}

impl Rows<'_> {
    fn len(&self) -> usize {
        match self {
            Rows::Identity(n) => *n,
            Rows::List(v) => v.len(),
        }
    }
    fn get(&self, i: usize) -> Row {
        match self {
            Rows::Identity(_) => Row { line: i, start: 0, end: usize::MAX },
            Rows::List(v) => v[i.min(v.len() - 1)],
        }
    }
    /// Row containing char column `col` of `line` (or the nearest visible row).
    fn row_of(&self, line: usize, col: usize) -> usize {
        match self {
            Rows::Identity(n) => line.min(n - 1),
            Rows::List(v) => {
                let i = v.partition_point(|r| (r.line, r.start) <= (line, col));
                let i = i.saturating_sub(1);
                // A caret at the exact wrap point belongs to the next row, except at line end.
                i.min(v.len() - 1)
            }
        }
    }
}

/// Split a line into wrapped segments of at most `width` visual columns, breaking after spaces.
fn wrap_line(text: &str, width: usize, tab: usize) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() || width == 0 {
        return vec![(0, chars.len())];
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut v = 0;
        let mut i = start;
        let mut last_space = None;
        while i < chars.len() {
            let w = if chars[i] == '\t' { tab - v % tab } else { 1 };
            if v + w > width {
                break;
            }
            v += w;
            if chars[i] == ' ' {
                last_space = Some(i + 1);
            }
            i += 1;
        }
        if i >= chars.len() {
            out.push((start, chars.len()));
            break;
        }
        let end = match last_space {
            Some(s) if s > start => s,
            _ => i.max(start + 1),
        };
        out.push((start, end));
        start = end;
    }
    out
}

fn ensure_rows(doc: &mut Doc, wrap_cols: Option<usize>) {
    let b = &doc.buf;
    if wrap_cols.is_none() && b.folds.is_empty() {
        doc.rows.key = None;
        return;
    }
    let key = (b.version, b.folds.clone(), wrap_cols.unwrap_or(0));
    if doc.rows.key.as_ref() == Some(&key) {
        return;
    }
    let folded = b.folded_lines();
    let n = b.rope.len_lines();
    let mut rows = Vec::with_capacity(n);
    let mut l = 0;
    let mut fi = 0;
    while l < n {
        match wrap_cols {
            Some(w) => {
                for (s, e) in wrap_line(&b.line_text(l), w, b.tab_size) {
                    rows.push(Row { line: l, start: s, end: e });
                }
            }
            None => rows.push(Row { line: l, start: 0, end: usize::MAX }),
        }
        while fi < folded.len() && folded[fi].0 < l {
            fi += 1;
        }
        if fi < folded.len() && folded[fi].0 == l {
            l = folded[fi].1 + 1;
        } else {
            l += 1;
        }
    }
    doc.rows = RowCache { key: Some(key), rows };
}

fn rows_of(doc: &Doc) -> Rows<'_> {
    match doc.rows.key {
        Some(_) => Rows::List(&doc.rows.rows),
        None => Rows::Identity(doc.buf.rope.len_lines()),
    }
}

/// (row index, visual column within the row) of a char position.
fn locate(b: &Buffer, rows: &Rows, pos: usize) -> (usize, usize) {
    let line = b.line_of(pos);
    let col = b.col_of(pos);
    let mut r = rows.row_of(line, col);
    let row = rows.get(r);
    // A caret exactly at a wrap boundary shows at the start of the next row.
    if row.line == line && row.end != usize::MAX && col >= row.end && r + 1 < rows.len() && rows.get(r + 1).line == line {
        r += 1;
    }
    let row = rows.get(r);
    if row.line != line {
        return (r, 0); // hidden by a fold
    }
    let text = b.line_text(line);
    let v = vis_col(&text, col, b.tab_size) - vis_col(&text, row.start, b.tab_size);
    (r, v)
}

fn pos_at(b: &Buffer, rows: &Rows, r: usize, vis: f32) -> usize {
    let row = rows.get(r.min(rows.len().saturating_sub(1)));
    let text = b.line_text(row.line);
    let n = text.chars().count();
    let end = row.end.min(n);
    let base = vis_col(&text, row.start, b.tab_size) as f32;
    let mut col = col_from_vis(&text, vis + base, b.tab_size).clamp(row.start, end);
    // Don't place the caret after the break of a wrapped row (it would show on the next row).
    if row.end != usize::MAX && row.end < n && col == end && col > row.start {
        col -= 1;
    }
    b.line_start(row.line) + col
}

// ---------- completion ----------

#[derive(Default)]
pub struct Completion {
    pub items: Vec<String>,
    pub selected: usize,
    /// Items from the language server; filtered by the typed prefix instead of buffer words.
    pub lsp: Vec<String>,
}

/// Language-server items matching `prefix`: prefix matches first, then fuzzy ones.
pub fn filter_lsp(items: &[String], prefix: &str) -> Vec<String> {
    let lp = prefix.to_lowercase();
    let mut starts = Vec::new();
    let mut fuzzy = Vec::new();
    for w in items {
        let lw = w.to_lowercase();
        if starts.contains(w) || fuzzy.contains(w) || *w == prefix {
            continue;
        }
        if lw.starts_with(&lp) {
            starts.push(w.clone());
        } else if !lp.is_empty() && is_subsequence(&lp, &lw) {
            fuzzy.push(w.clone());
        }
    }
    starts.extend(fuzzy);
    starts.truncate(12);
    starts
}

fn update_completion(doc: &mut Doc) {
    let b = &doc.buf;
    let Some(s) = b.sels.last() else { return };
    let prefix = b.prefix_at(s.head);
    if let Some(c) = doc.completion.as_mut().filter(|c| !c.lsp.is_empty()) {
        c.items = filter_lsp(&c.lsp, &prefix);
        c.selected = 0;
        if c.items.is_empty() {
            doc.completion = None;
        }
        return;
    }
    let at_word_end = b.rope.get_char(s.head).is_none_or(|c| !is_word_char(c));
    if prefix.chars().count() < 2 || !at_word_end || !b.sels.iter().all(|s| s.is_empty()) {
        doc.completion = None;
        return;
    }
    // Rebuild the word list when a new completion starts; reuse it while typing on.
    if doc.completion.is_none() || doc.words.0 == 0 {
        doc.words = (b.version, b.words());
    }
    let lp = prefix.to_lowercase();
    let mut starts: Vec<String> = Vec::new();
    let mut fuzzy: Vec<String> = Vec::new();
    for w in &doc.words.1 {
        if *w == prefix || w.chars().count() <= prefix.chars().count() {
            continue;
        }
        let lw = w.to_lowercase();
        if lw.starts_with(&lp) {
            starts.push(w.clone());
        } else if is_subsequence(&lp, &lw) && lw.starts_with(lp.chars().next().unwrap()) {
            fuzzy.push(w.clone());
        }
        if starts.len() >= 12 {
            break;
        }
    }
    starts.extend(fuzzy);
    starts.truncate(12);
    doc.completion = (!starts.is_empty()).then(|| Completion { items: starts, selected: 0, lsp: Vec::new() });
    if prefix.chars().count() == 2 {
        // A new word: ask the language server too; its answer replaces the buffer words.
        doc.lsp_req.push(crate::LspReq::Completion(s.head));
    }
}

/// Ctrl+Space: open the completion popup for the word before the caret.
pub fn trigger_completion(doc: &mut Doc) {
    doc.words.0 = 0;
    doc.completion = None;
    update_completion(doc);
    if let Some(s) = doc.buf.sels.last() {
        doc.lsp_req.push(crate::LspReq::Completion(s.head));
    }
}

fn is_subsequence(needle: &str, hay: &str) -> bool {
    let mut it = hay.chars();
    needle.chars().all(|c| it.any(|h| h == c))
}

// ---------- the view ----------

struct Metrics {
    char_w: f32,
    row_h: f32,
    text_left: f32,
    top: f32,
}

pub struct ViewArgs<'a> {
    pub syn: &'a Syntaxes,
    pub theme: &'a Theme,
    pub st: &'a Settings,
    pub group: usize,
    /// This view should take keyboard focus if nothing else has it.
    pub want_focus: bool,
}

pub struct ViewResponse {
    pub focused: bool,
    pub context_menu: Option<&'static str>,
}

pub fn show(ui: &mut egui::Ui, doc: &mut Doc, a: ViewArgs) -> ViewResponse {
    let (syn, theme, st) = (a.syn, a.theme, a.st);
    let ctx = ui.ctx().clone();
    let full = ui.available_rect_before_wrap();
    let font = FontId::monospace(st.font_size);
    let char_w = ui.fonts(|f| f.glyph_width(&font, 'M'));
    let row_h = (st.font_size * 1.45).round();
    let digits = doc.buf.rope.len_lines().to_string().len().max(3);
    let fold_w = char_w * 1.6;
    let gutter_w = if st.line_numbers { (digits as f32 + 2.0) * char_w + fold_w } else { char_w + fold_w };
    let minimap_w = if st.minimap { 110.0 } else { 0.0 };
    let text_rect = Rect::from_min_max(pos2(full.left() + gutter_w, full.top()), pos2(full.right() - minimap_w, full.bottom()));
    let m = Metrics { char_w, row_h, text_left: text_rect.left() + 4.0, top: full.top() + 2.0 };
    let wrap_cols = st.word_wrap.then(|| (((text_rect.width() - 16.0) / char_w).floor() as usize).max(10));
    if st.word_wrap {
        doc.scroll.x = 0.0;
    }

    let id = editor_id(a.group);
    let resp = ui.interact(text_rect, id, Sense::click_and_drag());
    if a.want_focus && ctx.memory(|mem| mem.focused().is_none()) {
        resp.request_focus();
    }
    ctx.memory_mut(|mem| mem.set_focus_lock_filter(id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false }));
    if resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::Text);
        // Wake up once the pointer has rested, for the hover popup.
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }

    let visible_rows = (text_rect.height() / row_h).floor().max(1.0) as usize;
    ensure_rows(doc, wrap_cols);
    if resp.has_focus() {
        handle_keys(&ctx, doc, visible_rows, wrap_cols);
    }
    handle_mouse(ui, &resp, doc, &m, wrap_cols);
    if resp.clicked() || resp.drag_started() {
        doc.completion = None;
    }
    let mut context_menu = None;
    resp.context_menu(|ui| {
        ui.spacing_mut().button_padding = vec2(14.0, 6.0);
        for (label, cmd) in [
            ("Goto Definition", "goto_definition"),
            ("Cut", "cut"),
            ("Copy", "copy"),
            ("Paste", "paste"),
            ("Select All", "select_all"),
            ("Toggle Comment", "toggle_comment"),
            ("Fold", "fold"),
            ("Unfold All", "unfold_all"),
            ("Toggle Bookmark", "toggle_bookmark"),
        ] {
            if ui.add(egui::Button::new(label).min_size(vec2(180.0, 0.0))).clicked() {
                context_menu = Some(cmd);
                ui.close_menu();
            }
        }
    });

    // Moving a caret into a fold opens it, like Sublime.
    if !doc.buf.folds.is_empty() {
        let before = doc.buf.folds.len();
        doc.buf.reveal_carets();
        if doc.buf.folds.len() != before {
            ensure_rows(doc, wrap_cols);
        }
    }
    ensure_rows(doc, wrap_cols);

    // Scrolling
    if ui.rect_contains_pointer(full) {
        let d = ui.input(|i| i.smooth_scroll_delta);
        doc.scroll -= d;
        if d != egui::Vec2::ZERO {
            doc.completion = None;
        }
    }
    let n_rows = rows_of(doc).len();
    let max_y = (n_rows.saturating_sub(1)) as f32 * row_h;
    if doc.reveal {
        doc.reveal = false;
        let rows = rows_of(doc);
        let s = *doc.buf.sels.last().unwrap();
        let (r, v) = locate(&doc.buf, &rows, s.head);
        let y = r as f32 * row_h;
        if y < doc.scroll.y {
            doc.scroll.y = y;
        } else if y + row_h > doc.scroll.y + text_rect.height() {
            doc.scroll.y = y + row_h - text_rect.height() + 4.0;
        }
        if wrap_cols.is_none() {
            let x = v as f32 * char_w;
            let w = text_rect.width() - 12.0;
            if x < doc.scroll.x {
                doc.scroll.x = (x - w / 3.0).max(0.0);
            } else if x > doc.scroll.x + w {
                doc.scroll.x = x - w * 2.0 / 3.0;
            }
        }
    }
    doc.scroll.y = doc.scroll.y.clamp(0.0, max_y);
    doc.scroll.x = doc.scroll.x.max(0.0);

    if let Some(l) = doc.buf.take_dirty_line() {
        doc.hl.invalidate_from(l);
    }

    // ---------- paint ----------
    let ts = &theme.settings;
    let bg = ts.background.map(c).unwrap_or(Color32::from_gray(30));
    let fg = ts.foreground.map(c).unwrap_or(Color32::from_gray(220));
    let sel_bg = ts.selection.map(c).unwrap_or(Color32::from_rgba_unmultiplied(80, 110, 160, 120));
    let caret_c = ts.caret.map(c).unwrap_or(fg);
    let line_hl = ts.line_highlight.map(c).unwrap_or(fg.gamma_multiply(0.06));
    let gutter_fg = ts.gutter_foreground.map(c).unwrap_or(fg.gamma_multiply(0.45));
    let gutter_bg = ts.gutter.map(c).unwrap_or(bg);
    let accent = Color32::from_rgb(0xff, 0x9a, 0x3d);

    let painter = ui.painter_at(full);
    painter.rect_filled(full, 0.0, bg);
    let first = (doc.scroll.y / row_h).floor() as usize;
    let last = (first + visible_rows + 1).min(n_rows.saturating_sub(1));
    let (sx, sy) = (doc.scroll.x, doc.scroll.y);
    let y_of = |r: usize| m.top + r as f32 * row_h - sy;
    let x_of = |vis: usize| m.text_left + vis as f32 * char_w - sx;
    let tab = doc.buf.tab_size;
    let tp = ui.painter_at(text_rect);

    let sels = doc.buf.sorted_sels();
    let caret_lines: std::collections::HashSet<usize> = sels.iter().map(|s| doc.buf.line_of(s.head)).collect();
    let folded = doc.buf.folded_lines();
    let bookmarks = doc.buf.bookmark_lines();

    // Gather the visible rows with their line text once.
    struct Vis {
        r: usize,
        row: Row,
        text: std::rc::Rc<String>,
        base: usize, // visual column of row.start within the line
    }
    let mut vis_rows: Vec<Vis> = Vec::new();
    {
        let rows = rows_of(doc);
        let mut cur: Option<(usize, std::rc::Rc<String>)> = None;
        for r in first..=last.min(rows.len().saturating_sub(1)) {
            let row = rows.get(r);
            let text = match &cur {
                Some((l, t)) if *l == row.line => t.clone(),
                _ => {
                    let t = std::rc::Rc::new(doc.buf.line_text(row.line));
                    cur = Some((row.line, t.clone()));
                    t
                }
            };
            let base = vis_col(&text, row.start, tab);
            vis_rows.push(Vis { r, row, text, base });
        }
    }
    let row_end = |v: &Vis| v.row.end.min(v.text.chars().count());

    let caret_rows: std::collections::HashSet<usize> = {
        let rows = rows_of(doc);
        sels.iter().map(|s| locate(&doc.buf, &rows, s.head).0).collect()
    };
    for v in &vis_rows {
        if st.highlight_line && caret_rows.contains(&v.r) {
            tp.rect_filled(Rect::from_min_size(pos2(text_rect.left(), y_of(v.r)), vec2(text_rect.width(), row_h)), 0.0, line_hl);
        }
    }
    for r in &st.rulers {
        let x = x_of(*r);
        tp.line_segment([pos2(x, text_rect.top()), pos2(x, text_rect.bottom())], Stroke::new(1.0_f32, fg.gamma_multiply(0.15)));
    }

    // Selections
    for s in sels.iter().filter(|s| !s.is_empty()) {
        for v in &vis_rows {
            let ls = doc.buf.line_start(v.row.line);
            let (ra, rb) = (ls + v.row.start, ls + row_end(v));
            if s.end() < ra || s.start() > rb || (s.end() == ra && v.row.start > 0) {
                continue;
            }
            let a = s.start().max(ra) - ls;
            let b = s.end().min(rb) - ls;
            let va = vis_col(&v.text, a, tab) - v.base;
            let mut vb = vis_col(&v.text, b, tab) - v.base;
            if s.end() > rb && row_end(v) == v.text.chars().count() {
                vb += 1; // show the selected newline
            }
            if va == vb {
                continue;
            }
            tp.rect_filled(Rect::from_min_max(pos2(x_of(va), y_of(v.r)), pos2(x_of(vb), y_of(v.r) + row_h)), 2.0, sel_bg);
        }
    }

    // Occurrences of the word under the caret / the selected word.
    if let Some(word) = highlight_word(&doc.buf) {
        let wl = word.chars().count();
        for v in &vis_rows {
            let ls = doc.buf.line_start(v.row.line);
            let chars: Vec<char> = v.text.chars().collect();
            let mut i = v.row.start;
            let end = row_end(v);
            while i + wl <= end {
                let hit = chars[i..i + wl].iter().copied().eq(word.chars())
                    && (i == 0 || !is_word_char(chars[i - 1]))
                    && chars.get(i + wl).is_none_or(|c| !is_word_char(*c));
                if hit {
                    let is_sel = sels.iter().any(|s| s.start() == ls + i && s.end() == ls + i + wl);
                    if !is_sel {
                        let r = Rect::from_min_max(pos2(x_of(vis_col(&v.text, i, tab) - v.base), y_of(v.r) + 1.0), pos2(x_of(vis_col(&v.text, i + wl, tab) - v.base), y_of(v.r) + row_h - 1.0));
                        tp.rect_stroke(r, 3.0, Stroke::new(1.0_f32, fg.gamma_multiply(0.45)), egui::StrokeKind::Inside);
                    }
                    i += wl;
                } else {
                    i += 1;
                }
            }
        }
    }

    // Matching brackets around the primary caret.
    if let Some(s) = doc.buf.sels.last().filter(|s| s.is_empty()) {
        if let Some((o, cl)) = doc.buf.matching_brackets(s.head) {
            let rows = rows_of(doc);
            for p in [o, cl] {
                let (r, vc) = locate(&doc.buf, &rows, p);
                if (first..=last).contains(&r) {
                    let y = y_of(r) + row_h - 3.0;
                    tp.line_segment([pos2(x_of(vc) + 1.0, y), pos2(x_of(vc + 1) - 1.0, y)], Stroke::new(2.0_f32, accent));
                }
            }
        }
    }

    // Text
    for v in &vis_rows {
        let spans = doc.hl.line(syn, &st.color_scheme, &doc.buf.rope, v.row.line).to_vec();
        let mut job = LayoutJob::default();
        let chars: Vec<char> = v.text.chars().take(MAX_RENDER_CHARS).collect();
        let (rs, re) = (v.row.start.min(chars.len()), row_end(v).min(chars.len()));
        let mut vis = v.base;
        let push = |job: &mut LayoutJob, from: usize, to: usize, color: Color32, italic: bool, vis: &mut usize| {
            let (from, to) = (from.max(rs), to.min(re));
            if from >= to {
                return;
            }
            let mut s = String::new();
            for ch in &chars[from..to] {
                if *ch == '\t' {
                    let n = tab - *vis % tab;
                    s.extend(std::iter::repeat_n(' ', n));
                    *vis += n;
                } else if crate::emoji::is_emoji(*ch) {
                    // Hidden glyph keeps the layout; the color bitmap is painted over it.
                    job.append(&std::mem::take(&mut s), 0.0, TextFormat { font_id: font.clone(), color, italics: italic, ..Default::default() });
                    job.append(&ch.to_string(), 0.0, TextFormat { font_id: font.clone(), color: Color32::TRANSPARENT, ..Default::default() });
                    *vis += 1;
                } else {
                    s.push(*ch);
                    *vis += 1;
                }
            }
            job.append(&s, 0.0, TextFormat { font_id: font.clone(), color, italics: italic, ..Default::default() });
        };
        let mut cur = 0;
        for sp in &spans {
            if sp.start > cur {
                push(&mut job, cur, sp.start, fg, false, &mut vis);
            }
            push(&mut job, sp.start, sp.end, c(sp.fg), sp.italic, &mut vis);
            cur = sp.end;
        }
        if cur < chars.len() {
            push(&mut job, cur, chars.len(), fg, false, &mut vis);
        }
        let galley = ui.fonts(|f| f.layout_job(job));
        let gy = y_of(v.r) + (row_h - galley.size().y) / 2.0;
        let width = galley.size().x;
        tp.galley(pos2(x_of(0), gy), galley.clone(), fg);
        crate::emoji::paint_galley(&tp, pos2(x_of(0), gy), &galley);
        // Folded block marker after the header line.
        if row_end(v) == v.text.chars().count() && folded.iter().any(|(h, _)| *h == v.row.line) {
            let r = Rect::from_min_size(pos2(x_of(0) + width + char_w, y_of(v.r) + 3.0), vec2(char_w * 3.0, row_h - 6.0));
            tp.rect_filled(r, 4.0, fg.gamma_multiply(0.15));
            tp.text(r.center(), egui::Align2::CENTER_CENTER, "⋯", font.clone(), fg);
        }
    }

    // Carets
    if resp.has_focus() || sels.len() > 1 {
        let rows = rows_of(doc);
        for s in &sels {
            let (r, vc) = locate(&doc.buf, &rows, s.head);
            if r < first || r > last {
                continue;
            }
            tp.rect_filled(Rect::from_min_size(pos2(x_of(vc) - 0.5, y_of(r) + 1.0), vec2(2.0, row_h - 2.0)), 0.0, caret_c);
        }
    }

    // Gutter: line numbers and fold arrows.
    let gutter = Rect::from_min_max(full.min, pos2(text_rect.left(), full.bottom()));
    painter.rect_filled(gutter, 0.0, gutter_bg);
    let gutter_hover = ui.rect_contains_pointer(gutter);
    let fold_x = gutter.right() - fold_w / 2.0 - 2.0;
    let mut toggle_fold = None;
    for v in vis_rows.iter().filter(|v| v.row.start == 0) {
        let y = y_of(v.r) + row_h / 2.0;
        if st.line_numbers {
            let color = if caret_lines.contains(&v.row.line) { fg } else { gutter_fg };
            painter.text(pos2(gutter.right() - fold_w - char_w * 0.6, y), egui::Align2::RIGHT_CENTER, (v.row.line + 1).to_string(), font.clone(), color);
        }
        // Git change bar along the gutter's left edge.
        if let Some(Some(ch)) = doc.git_marks.1.get(v.row.line) {
            let x = gutter.left() + 1.0;
            let top = y_of(v.r);
            match ch {
                crate::git::LineChange::Added | crate::git::LineChange::Modified => {
                    let col = if *ch == crate::git::LineChange::Added { Color32::from_rgb(0x8c, 0xc2, 0x65) } else { Color32::from_rgb(0xe5, 0xa5, 0x4b) };
                    painter.rect_filled(Rect::from_min_size(pos2(x, top), vec2(3.0, row_h)), 0.0, col);
                }
                crate::git::LineChange::DeletedAbove => {
                    let k = (row_h * 0.22).max(3.0);
                    painter.add(egui::Shape::convex_polygon(vec![pos2(x, top - k), pos2(x + k * 1.4, top), pos2(x, top + k)], Color32::from_rgb(0xe0, 0x6c, 0x75), Stroke::NONE));
                }
            }
        }
        if bookmarks.contains(&v.row.line) {
            painter.circle_filled(pos2(gutter.left() + char_w * 0.7, y), (row_h * 0.18).max(3.0), Color32::from_rgb(0x4f, 0x9c, 0xff));
        }
        let is_folded = folded.iter().any(|(h, _)| *h == v.row.line);
        let foldable = is_folded || (gutter_hover && quick_foldable(&doc.buf, v.row.line, &v.text));
        if foldable {
            let r = Rect::from_center_size(pos2(fold_x, y), vec2(fold_w, row_h));
            let hr = ui.interact(r, Id::new(("fold", a.group, v.row.line)), Sense::click());
            let col = if hr.hovered() || is_folded { fg } else { gutter_fg };
            // Drawn as a triangle: ▸ when folded, ▾ when open (fonts often lack these glyphs).
            let k = (st.font_size * 0.28).max(3.0);
            let cpt = r.center();
            let pts = if is_folded {
                vec![pos2(cpt.x - k * 0.6, cpt.y - k), pos2(cpt.x + k * 0.8, cpt.y), pos2(cpt.x - k * 0.6, cpt.y + k)]
            } else {
                vec![pos2(cpt.x - k, cpt.y - k * 0.6), pos2(cpt.x + k, cpt.y - k * 0.6), pos2(cpt.x, cpt.y + k * 0.8)]
            };
            painter.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
            if hr.clicked() {
                toggle_fold = Some((v.row.line, is_folded));
            }
        }
    }
    if let Some((line, is_folded)) = toggle_fold {
        if is_folded {
            let rope = &doc.buf.rope;
            doc.buf.folds.retain(|&(a, _)| rope.char_to_line(a) != line);
        } else if let Some((a, b)) = doc.buf.fold_range_at(line) {
            doc.buf.fold_lines(a, b);
        }
    }

    if st.minimap {
        minimap(ui, doc, syn, st, Rect::from_min_max(pos2(text_rect.right(), full.top()), full.max), bg, fg, visible_rows, row_h, a.group);
    }

    // Diagnostics: wavy underline, red for errors, yellow for warnings.
    if !doc.diagnostics.is_empty() {
        let rows = rows_of(doc);
        for d in &doc.diagnostics {
            if d.start.0 >= doc.buf.rope.len_lines() {
                continue;
            }
            let col = match d.severity {
                crate::lsp::Severity::Error => Color32::from_rgb(0xf0, 0x55, 0x5a),
                crate::lsp::Severity::Warning => Color32::from_rgb(0xe5, 0xc0, 0x4b),
                crate::lsp::Severity::Info => Color32::from_rgb(0x6c, 0xa8, 0xe0),
            };
            let to_pos = |(l, c): (usize, usize)| {
                let l = l.min(doc.buf.rope.len_lines() - 1);
                doc.buf.line_start(l) + c.min(line_chars(&doc.buf, l))
            };
            let (a, mut b) = (to_pos(d.start), to_pos(d.end));
            if b <= a {
                b = (a + 1).min(doc.buf.line_start(d.start.0) + line_chars(&doc.buf, d.start.0)).max(a);
            }
            let (r0, v0) = locate(&doc.buf, &rows, a);
            let (r1, v1) = locate(&doc.buf, &rows, b);
            for r in r0.max(first)..=r1.min(last) {
                let xa = if r == r0 { x_of(v0) } else { x_of(0) };
                let xb = if r == r1 { x_of(v1.max(if r == r0 { v0 + 1 } else { 1 })) } else { text_rect.right() };
                let y = y_of(r) + row_h - 2.5;
                let mut pts = Vec::new();
                let mut x = xa;
                let mut up = true;
                while x < xb {
                    pts.push(pos2(x, if up { y - 1.5 } else { y + 1.0 }));
                    x += 2.5;
                    up = !up;
                }
                pts.push(pos2(xb, y));
                tp.add(egui::Shape::line(pts, Stroke::new(1.2_f32, col)));
            }
        }
    }

    // Hover: after the pointer rests over text, show diagnostics and language-server info.
    let mut hover_lines: Vec<(String, Color32)> = Vec::new();
    let mut hover_at = None;
    if let Some(p) = resp.hover_pos().filter(|_| doc.completion.is_none() && ui.input(|i| i.pointer.time_since_last_movement() > 0.45)) {
        let (pos, line, _) = point_to(doc, &m, p);
        let col = pos - doc.buf.line_start(line);
        let on_text = doc.buf.rope.get_char(pos).is_some_and(|c| !c.is_whitespace());
        for d in &doc.diagnostics {
            let inside = (d.start.0, d.start.1) <= (line, col) && (line, col) <= (d.end.0, d.end.1.max(d.start.1 + 1));
            if inside {
                let col = if d.severity == crate::lsp::Severity::Error { Color32::from_rgb(0xf0, 0x80, 0x80) } else { fg };
                hover_lines.push((d.message.clone(), col));
            }
        }
        if on_text {
            let (ws, we) = doc.buf.word_at(pos);
            match &doc.hover {
                Some((hp, text)) if (ws..we.max(ws + 1)).contains(hp) => hover_lines.push((text.clone(), fg)),
                Some((hp, _)) if *hp == pos => {}
                _ if !doc.lsp_req.contains(&crate::LspReq::Hover(pos)) => {
                    doc.hover = Some((pos, String::new()));
                    doc.lsp_req.push(crate::LspReq::Hover(pos));
                }
                _ => {}
            }
        }
        hover_lines.retain(|(t, _)| !t.trim().is_empty());
        hover_at = Some(p);
    }
    if let Some(p) = hover_at.filter(|_| !hover_lines.is_empty()) {
        egui::Area::new(Id::new(("hover", a.group))).fixed_pos(p + vec2(8.0, 14.0)).order(egui::Order::Tooltip).interactable(false).show(&ctx, |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                ui.set_max_width(640.0);
                for (i, (t, col)) in hover_lines.iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    let t: String = t.lines().take(24).collect::<Vec<_>>().join("\n");
                    ui.label(egui::RichText::new(t).font(FontId::monospace((st.font_size - 1.0).max(9.0))).color(*col));
                }
            });
        });
    }

    // Completion popup under the primary caret.
    let mut accept = None;
    if resp.has_focus() {
        if let Some(comp) = &doc.completion {
            let rows = rows_of(doc);
            let s = *doc.buf.sels.last().unwrap();
            let (r, vc) = locate(&doc.buf, &rows, s.head);
            let prefix_len = doc.buf.prefix_at(s.head).chars().count();
            let at = pos2(x_of(vc.saturating_sub(prefix_len)) - 6.0, y_of(r) + row_h + 2.0);
            egui::Area::new(Id::new(("completion", a.group))).fixed_pos(at).order(egui::Order::Foreground).show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (i, w) in comp.items.iter().enumerate() {
                        let rt = egui::RichText::new(w).font(font.clone());
                        if ui.add(egui::Button::new(rt).selected(i == comp.selected).min_size(vec2(200.0, row_h + 2.0))).clicked() {
                            accept = Some(w.clone());
                        }
                    }
                });
            });
        }
    }
    if let Some(w) = accept {
        doc.buf.complete_with(&w);
        doc.completion = None;
        resp.request_focus();
    }

    ui.advance_cursor_after_rect(full);
    ViewResponse { focused: resp.has_focus() || resp.clicked(), context_menu }
}

/// Cheap check for the fold arrow: the next non-blank line is indented deeper.
fn quick_foldable(b: &Buffer, line: usize, text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let ind = |t: &str| t.chars().take_while(|c| c.is_whitespace()).map(|c| if c == '\t' { b.tab_size } else { 1 }).sum::<usize>();
    let me = ind(text);
    let n = b.rope.len_lines();
    (line + 1..(line + 50).min(n)).map(|l| b.line_text(l)).find(|t| !t.trim().is_empty()).is_some_and(|t| ind(&t) > me)
}

/// The word whose other occurrences get outlined.
fn highlight_word(b: &Buffer) -> Option<String> {
    if b.sels.len() != 1 {
        return None;
    }
    let s = b.sels[0];
    let (a, e) = if s.is_empty() { b.word_at(s.head) } else { (s.start(), s.end()) };
    if e <= a || e - a > 100 {
        return None;
    }
    let w = b.rope.slice(a..e).to_string();
    if !w.chars().all(is_word_char) {
        return None;
    }
    // A selection must be the whole word.
    if !s.is_empty() && b.word_at(a) != (a, e) {
        return None;
    }
    Some(w)
}

#[allow(clippy::too_many_arguments)]
fn minimap(ui: &mut egui::Ui, doc: &mut Doc, syn: &Syntaxes, st: &Settings, rect: Rect, bg: Color32, fg: Color32, visible_rows: usize, row_h: f32, group: usize) {
    const LH: f32 = 2.0;
    const CW: f32 = 1.0;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, bg);
    painter.line_segment([rect.left_top(), rect.left_bottom()], Stroke::new(1.0_f32, fg.gamma_multiply(0.1)));
    let lines = doc.buf.rope.len_lines();
    let mm_rows = (rect.height() / LH) as usize;
    let first_vis = {
        let rows = rows_of(doc);
        rows.get((doc.scroll.y / row_h) as usize).line
    };
    let mm_first = if lines > mm_rows { ((first_vis as f32 / (lines - visible_rows.min(lines)).max(1) as f32) * (lines - mm_rows) as f32) as usize } else { 0 };
    let mm_first = mm_first.min(lines.saturating_sub(mm_rows));
    for (i, line) in (mm_first..lines).take(mm_rows).enumerate() {
        let y = rect.top() + i as f32 * LH;
        let text = doc.buf.line_text(line);
        let spans = doc.hl.line(syn, &st.color_scheme, &doc.buf.rope, line).to_vec();
        for sp in spans {
            let seg: String = text.chars().skip(sp.start).take(sp.end - sp.start).collect();
            let lead = seg.chars().take_while(|c| c.is_whitespace()).count();
            let len = seg.trim_end().chars().count().saturating_sub(lead);
            if len == 0 {
                continue;
            }
            let x = rect.left() + 4.0 + (sp.start + lead) as f32 * CW;
            if x > rect.right() {
                break;
            }
            let w = (len as f32 * CW).min(rect.right() - x);
            painter.rect_filled(Rect::from_min_size(pos2(x, y), vec2(w, LH - 0.5)), 0.0, c(sp.fg).gamma_multiply(0.7));
        }
    }
    let vy = rect.top() + (first_vis.saturating_sub(mm_first)) as f32 * LH;
    let view = Rect::from_min_size(pos2(rect.left(), vy), vec2(rect.width(), visible_rows as f32 * LH));
    painter.rect_filled(view, 0.0, fg.gamma_multiply(0.08));
    let r = ui.interact(rect, Id::new(("minimap", group)), Sense::click_and_drag());
    if let Some(p) = r.interact_pointer_pos() {
        if r.is_pointer_button_down_on() {
            let line = (mm_first as f32 + (p.y - rect.top()) / LH - visible_rows as f32 / 2.0).max(0.0) as usize;
            let rows = rows_of(doc);
            doc.scroll.y = rows.row_of(line.min(lines - 1), 0) as f32 * row_h;
        }
    }
}

/// Up/Down by screen rows (respects word wrap and folds), keeping a visual goal column.
fn move_rows(doc: &mut Doc, dr: isize, extend: bool) {
    let rows = rows_of(doc);
    let b = &doc.buf;
    let n = rows.len() as isize;
    let mut sels: Vec<Selection> = b
        .sels
        .iter()
        .map(|s| {
            let (r, v) = locate(b, &rows, s.head);
            let goal = s.goal_col.unwrap_or(v);
            let t = r as isize + dr;
            let head = if t < 0 {
                0
            } else if t >= n {
                b.len()
            } else {
                pos_at(b, &rows, t as usize, goal as f32)
            };
            Selection { anchor: if extend { s.anchor } else { head }, head, goal_col: Some(goal) }
        })
        .collect();
    sublimers_core::selection::normalize(&mut sels);
    doc.buf.sels = sels;
    doc.buf.break_undo_group();
}

/// Home/End move to the edges of the wrapped row first, then the line.
fn row_edge(doc: &mut Doc, end: bool, extend: bool) -> bool {
    let rows = rows_of(doc);
    let b = &doc.buf;
    if !matches!(rows, Rows::List(_)) {
        return false;
    }
    let mut changed = false;
    let mut sels: Vec<Selection> = b
        .sels
        .iter()
        .map(|s| {
            let (r, _) = locate(b, &rows, s.head);
            let row = rows.get(r);
            let n = b.line_text(row.line).chars().count();
            let ls = b.line_start(row.line);
            let target = if end { ls + if row.end >= n { n } else { row.end - 1 } } else { ls + row.start };
            // Only use the row edge if this row is wrapped and we're not already there.
            let wrapped = row.start > 0 || row.end < n;
            if wrapped && target != s.head && !(row.start == 0 && !end) {
                changed = true;
                Selection { anchor: if extend { s.anchor } else { target }, head: target, goal_col: None }
            } else {
                *s
            }
        })
        .collect();
    if changed {
        sublimers_core::selection::normalize(&mut sels);
        doc.buf.sels = sels;
    }
    changed
}

fn handle_keys(ctx: &egui::Context, doc: &mut Doc, page: usize, wrap_cols: Option<usize>) {
    use egui::{Event, Key};
    // The completion popup gets first pick of navigation keys.
    if doc.completion.is_some() {
        let (up, down, accept) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, Key::Tab) || i.consume_key(egui::Modifiers::NONE, Key::Enter),
            )
        });
        let comp = doc.completion.as_mut().unwrap();
        let n = comp.items.len();
        if down {
            comp.selected = (comp.selected + 1) % n;
        }
        if up {
            comp.selected = (comp.selected + n - 1) % n;
        }
        if accept {
            let w = comp.items[comp.selected].clone();
            doc.buf.complete_with(&w);
            doc.completion = None;
            doc.reveal = true;
        }
    }
    let events = ctx.input(|i| i.events.clone());
    for e in events {
        let mut acted = true;
        let mut keep_completion = false;
        match e {
            Event::Text(t) => {
                if t != "\n" && t != "\r" {
                    doc.buf.type_text(&t);
                    if t.chars().all(is_word_char) {
                        update_completion(doc);
                        keep_completion = true;
                    } else if t == "." || (t == ":" && doc.buf.sels.last().is_some_and(|s| s.head >= 2 && doc.buf.rope.char(s.head - 2) == ':')) {
                        // Member access: ask the language server for completions.
                        doc.completion = None;
                        if let Some(s) = doc.buf.sels.last() {
                            doc.lsp_req.push(crate::LspReq::Completion(s.head));
                        }
                    }
                }
            }
            Event::Copy | Event::Cut => {
                let b = &mut doc.buf;
                let cut = matches!(e, Event::Cut);
                if b.sels.iter().all(|s| s.is_empty()) {
                    // Sublime copies/cuts whole lines when nothing is selected.
                    let saved = b.sels.clone();
                    b.expand_to_line();
                    ctx.copy_text(b.selected_texts().concat());
                    if cut {
                        b.delete_lines();
                    } else {
                        b.sels = saved;
                    }
                } else {
                    ctx.copy_text(b.selected_texts().join("\n"));
                    if cut {
                        b.insert("");
                    }
                }
            }
            Event::Paste(s) => {
                let b = &mut doc.buf;
                let s = s.replace("\r\n", "\n");
                let parts: Vec<String> = s.split('\n').map(String::from).collect();
                if b.sels.len() > 1 && parts.len() == b.sels.len() {
                    b.sels = b.sorted_sels();
                    b.insert_per_selection(&parts);
                } else {
                    b.insert(&s);
                }
                b.break_undo_group();
            }
            Event::Key { key, pressed: true, modifiers: md, .. } => {
                let (sh, word) = (md.shift, md.command);
                match key {
                    Key::ArrowLeft if word => doc.buf.move_word_left(sh),
                    Key::ArrowLeft => doc.buf.move_left(sh),
                    Key::ArrowRight if word => doc.buf.move_word_right(sh),
                    Key::ArrowRight => doc.buf.move_right(sh),
                    Key::ArrowUp if !md.alt && !word => move_rows(doc, -1, sh),
                    Key::ArrowDown if !md.alt && !word => move_rows(doc, 1, sh),
                    Key::ArrowUp if word && !sh => doc.scroll.y -= 20.0,
                    Key::ArrowDown if word && !sh => doc.scroll.y += 20.0,
                    Key::PageUp => move_rows(doc, -(page as isize), sh),
                    Key::PageDown => move_rows(doc, page as isize, sh),
                    Key::Home if word => doc.buf.move_doc_start(sh),
                    Key::Home => {
                        if wrap_cols.is_none() || !row_edge(doc, false, sh) {
                            doc.buf.move_home(sh)
                        }
                    }
                    Key::End if word => doc.buf.move_doc_end(sh),
                    Key::End => {
                        if wrap_cols.is_none() || !row_edge(doc, true, sh) {
                            doc.buf.move_end(sh)
                        }
                    }
                    Key::Backspace if word => doc.buf.delete_word_back(),
                    Key::Backspace => {
                        doc.buf.backspace();
                        if doc.completion.is_some() {
                            update_completion(doc);
                            keep_completion = true;
                        }
                    }
                    Key::Delete if word => doc.buf.delete_word_forward(),
                    Key::Delete => doc.buf.delete_forward(),
                    Key::Enter => doc.buf.newline(),
                    Key::Tab if !sh => doc.buf.tab(),
                    _ => acted = false,
                }
                // Edits and moves change the rows; refresh before the next event.
                ensure_rows(doc, wrap_cols);
            }
            _ => acted = false,
        }
        if acted {
            doc.reveal = true;
            if !keep_completion {
                doc.completion = None;
            }
        }
    }
}

fn line_chars(b: &Buffer, l: usize) -> usize {
    b.rope.line(l).chars().filter(|c| *c != '\n' && *c != '\r').count()
}

fn point_to(doc: &Doc, m: &Metrics, p: egui::Pos2) -> (usize, usize, f32) {
    // (pos, line, visual column within the line)
    let rows = rows_of(doc);
    let r = ((p.y - m.top + doc.scroll.y) / m.row_h).floor().max(0.0) as usize;
    if r >= rows.len() {
        let b = &doc.buf;
        return (b.len(), b.rope.len_lines() - 1, f32::MAX);
    }
    let vis = ((p.x - m.text_left + doc.scroll.x) / m.char_w).max(0.0);
    let row = rows.get(r);
    let text = doc.buf.line_text(row.line);
    let base = vis_col(&text, row.start, doc.buf.tab_size) as f32;
    (pos_at(&doc.buf, &rows, r, vis), row.line, vis + base)
}

fn handle_mouse(ui: &egui::Ui, resp: &egui::Response, doc: &mut Doc, m: &Metrics, wrap_cols: Option<usize>) {
    let Some(p) = resp.interact_pointer_pos() else { return };
    let (pressed, mods, middle, secondary) = ui.input(|i| (i.pointer.primary_pressed(), i.modifiers, i.pointer.middle_down(), i.pointer.secondary_down()));
    let (pos, line, vis) = point_to(doc, m, p);
    let anchor_id = Id::new("drag_anchor");
    let col_id = Id::new("column_anchor");

    // Column selection: middle-drag, or Shift + right-drag (Sublime on Linux).
    if middle || (secondary && mods.shift) {
        let start = ui.ctx().data(|d| d.get_temp::<(usize, f32)>(col_id));
        match start {
            Some(s) if resp.dragged() => {
                doc.buf.column_select(s, (line, vis));
                doc.reveal = true;
            }
            None => ui.ctx().data_mut(|d| d.insert_temp(col_id, (line, vis))),
            _ => {}
        }
        resp.request_focus();
        return;
    }
    ui.ctx().data_mut(|d| d.remove::<(usize, f32)>(col_id));
    if secondary {
        return; // context menu
    }

    let b = &mut doc.buf;
    if resp.triple_clicked() {
        let l = b.line_of(pos);
        let end = (b.line_end(l) + 1).min(b.len());
        b.sels = vec![Selection::new(b.line_start(l), end)];
    } else if resp.double_clicked() {
        let (a, e) = b.word_at(pos);
        if mods.command {
            b.sels.push(Selection::new(a, e));
            sublimers_core::selection::normalize(&mut b.sels);
        } else {
            b.sels = vec![Selection::new(a, e)];
        }
    } else if pressed {
        resp.request_focus();
        if mods.shift {
            let last = b.sels.last_mut().unwrap();
            last.head = pos;
        } else if mods.command {
            b.add_caret(pos);
        } else {
            b.set_caret(pos);
        }
        let anchor = if mods.shift { b.sels.last().unwrap().anchor } else { pos };
        ui.ctx().data_mut(|d| d.insert_temp(anchor_id, anchor));
    } else if resp.dragged() {
        if let Some(anchor) = ui.ctx().data(|d| d.get_temp::<usize>(anchor_id)) {
            let sel = Selection::new(anchor, pos);
            if let Some(i) = b.sels.iter().position(|s| s.anchor == anchor) {
                b.sels[i] = sel;
            } else {
                b.sels.push(sel);
            }
            if !mods.command {
                b.sels.retain(|s| s.anchor == anchor);
            }
            doc.reveal = true;
        }
    }
    let _ = wrap_cols;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_spaces() {
        assert_eq!(wrap_line("aaa bbb ccc", 8, 4), vec![(0, 8), (8, 11)]);
        assert_eq!(wrap_line("abcdefghij", 4, 4), vec![(0, 4), (4, 8), (8, 10)]);
        assert_eq!(wrap_line("", 4, 4), vec![(0, 0)]);
    }
}

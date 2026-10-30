//! Command palette, Goto Anything (files, @symbols, :line), theme and syntax pickers.

use eframe::egui;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

use crate::App;

/// `("-", "")` entries are separators; items named `"Group: Item"` in a row become a submenu.
pub const MENUS: &[(&str, &[(&str, &str)])] = &[
    ("File", &[
        ("New File", "new_file"),
        ("New Window", "new_window"),
        ("-", ""),
        ("Open File…", "open_file"),
        ("Open Folder…", "open_folder"),
        ("-", ""),
        ("Save", "save"),
        ("Save As…", "save_as"),
        ("Save All", "save_all"),
        ("-", ""),
        ("Reopen Closed File", "reopen_last_file"),
        ("Close File", "close_file"),
        ("Close Window", "close_window"),
        ("-", ""),
        ("Exit", "exit"),
    ]),
    ("Edit", &[
        ("Undo", "undo"),
        ("Redo", "redo"),
        ("Soft Undo", "soft_undo"),
        ("Soft Redo", "soft_redo"),
        ("-", ""),
        ("Insert Line After", "insert_line_after"),
        ("Insert Line Before", "insert_line_before"),
        ("Join Lines", "join_lines"),
        ("Delete to End of Line", "delete_to_eol"),
        ("Delete to Beginning of Line", "delete_to_bol"),
        ("-", ""),
        ("Sort Lines", "sort_lines"),
        ("Sort Lines (Case Sensitive)", "sort_lines_case_sensitive"),
        ("Duplicate Line", "duplicate_line"),
        ("Delete Line", "delete_line"),
        ("Swap Line Up", "swap_line_up"),
        ("Swap Line Down", "swap_line_down"),
        ("-", ""),
        ("Indent", "indent"),
        ("Unindent", "unindent"),
        ("Toggle Comment", "toggle_comment"),
        ("-", ""),
        ("Convert to Upper Case", "upper_case"),
        ("Convert to Lower Case", "lower_case"),
        ("-", ""),
        ("Code Folding: Fold", "fold"),
        ("Code Folding: Unfold", "unfold"),
        ("Code Folding: Unfold All", "unfold_all"),
        ("Code Folding: Fold Level 1", "fold_level_1"),
        ("Code Folding: Fold Level 2", "fold_level_2"),
        ("-", ""),
        ("Show Completions", "auto_complete"),
    ]),
    ("Selection", &[
        ("Select All", "select_all"),
        ("Expand Selection to Line", "expand_selection_to_line"),
        ("-", ""),
        ("Quick Add Next", "find_under_expand"),
        ("Quick Skip Next", "find_under_expand_skip"),
        ("Quick Find All", "find_all_under"),
        ("-", ""),
        ("Split into Lines", "split_selection_into_lines"),
        ("Add Cursor Above", "add_cursor_above"),
        ("Add Cursor Below", "add_cursor_below"),
        ("Expand Selection to Brackets", "expand_selection_to_brackets"),
        ("-", ""),
        ("Single Selection", "single_selection"),
    ]),
    ("Find", &[
        ("Find…", "show_find"),
        ("Find Next", "find_next"),
        ("Find Previous", "find_prev"),
        ("-", ""),
        ("Quick Find", "find_under"),
        ("Quick Find Previous", "find_under_prev"),
        ("Use Selection for Find", "slurp_find_string"),
        ("-", ""),
        ("Replace…", "show_replace"),
        ("Find in Files…", "show_find_in_files"),
    ]),
    ("View", &[
        ("Toggle Side Bar", "toggle_side_bar"),
        ("Toggle Minimap", "toggle_minimap"),
        ("Word Wrap", "toggle_word_wrap"),
        ("-", ""),
        ("Layout: Single", "set_layout_1"),
        ("Layout: Columns 2", "set_layout_2"),
        ("Layout: Columns 3", "set_layout_3"),
        ("Layout: Columns 4", "set_layout_4"),
        ("Layout: Rows 2", "set_layout_rows_2"),
        ("Layout: Rows 3", "set_layout_rows_3"),
        ("Layout: Grid 4", "set_layout_grid"),
        ("-", ""),
        ("Focus Group 1", "focus_group_1"),
        ("Focus Group 2", "focus_group_2"),
        ("Move File to Group 1", "move_to_group_1"),
        ("Move File to Group 2", "move_to_group_2"),
        ("-", ""),
        ("Enter Full Screen", "toggle_full_screen"),
        ("Enter Distraction Free Mode", "toggle_distraction_free"),
        ("Syntax…", "set_syntax"),
        ("Font: Larger", "increase_font_size"),
        ("Font: Smaller", "decrease_font_size"),
        ("Font: Reset", "reset_font_size"),
    ]),
    ("Goto", &[
        ("Goto Anything…", "show_overlay_goto"),
        ("Goto Symbol…", "goto_symbol"),
        ("Goto Line…", "goto_line"),
        ("Goto Definition", "goto_definition"),
        ("Goto Word…", "goto_word"),
        ("Jump to Matching Bracket", "move_to_bracket"),
        ("Bookmarks: Toggle", "toggle_bookmark"),
        ("Bookmarks: Next", "next_bookmark"),
        ("Bookmarks: Previous", "prev_bookmark"),
        ("Bookmarks: Clear All", "clear_bookmarks"),
        ("Bookmarks: Select All", "select_all_bookmarks"),
        ("Next File", "next_view"),
        ("Previous File", "prev_view"),
    ]),
    ("Tools", &[("Command Palette…", "show_overlay_command"), ("Terminal", "toggle_terminal"), ("Build", "build"), ("Build With…", "build_with"), ("Cancel Build", "cancel_build"), ("Build Results: Show/Hide", "show_panel_output"), ("Next Result", "next_result"), ("Previous Result", "prev_result"), ("Format File (LSP)", "lsp_format"), ("Restart Language Servers", "lsp_restart")]),
    ("Help", &[("Check for Updates…", "check_for_updates")]),
    ("Preferences", &[
        ("Select Color Scheme…", "select_theme"),
        ("Font Size: Larger", "increase_font_size"),
        ("Font Size: Smaller", "decrease_font_size"),
        ("Font Size: Reset", "reset_font_size"),
        ("-", ""),
        ("Font Weight: Light", "font_weight_light"),
        ("Font Weight: Regular", "font_weight_regular"),
        ("Font Weight: Medium", "font_weight_medium"),
        ("Font Weight: Bold", "font_weight_bold"),
        ("-", ""),
        ("Settings", "open_settings"),
        ("Key Bindings", "open_keybindings"),
        ("Reload Settings", "reload_settings"),
    ]),
];

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum PaletteMode {
    #[default]
    Files,
    Commands,
    Themes,
    Syntaxes,
}

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    pub mode: PaletteMode,
    pub input: String,
    pub selected: usize,
    focus: bool,
    /// Theme to restore if a theme preview is cancelled.
    original_theme: Option<String>,
}

impl Palette {
    pub fn show(&mut self, mode: PaletteMode, prefix: &str) {
        self.open = true;
        self.mode = mode;
        self.input = prefix.to_string();
        self.selected = 0;
        self.focus = true;
        self.original_theme = None;
    }
}

#[derive(Clone)]
enum Target {
    File(std::path::PathBuf),
    Doc(usize),
    Pos(usize),
    Command(String),
    Theme(String),
    Syntax(String),
}

fn fuzzy<T: Clone>(query: &str, items: Vec<(String, T)>) -> Vec<(String, T)> {
    if query.trim().is_empty() {
        return items;
    }
    let mut m = Matcher::new(Config::DEFAULT.match_paths());
    let pat = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let labels: Vec<&str> = items.iter().map(|(l, _)| l.as_str()).collect();
    let mut scored: Vec<(usize, u32)> = labels
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let mut buf = Vec::new();
            pat.score(nucleo_matcher::Utf32Str::new(l, &mut buf), &mut m).map(|s| (i, s))
        })
        .collect();
    scored.sort_by_key(|(i, s)| (std::cmp::Reverse(*s), labels[*i].len()));
    scored.into_iter().map(|(i, _)| items[i].clone()).collect()
}

/// Lines that look like definitions, for Goto Symbol.
fn symbols(app: &App) -> Vec<(String, usize)> {
    let re = regex_lite();
    let b = &app.docs[app.active].buf;
    (0..b.rope.len_lines())
        .filter_map(|l| {
            let t = b.line_text(l);
            re.captures(&t).map(|c| (c.get(1).unwrap().as_str().to_string(), b.line_start(l) + t.find(c.get(1).unwrap().as_str()).map_or(0, |i| t[..i].chars().count())))
        })
        .collect()
}

fn regex_lite() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:export\s+)?(?:default\s+)?(?:fn|def|class|struct|enum|trait|impl|interface|function|func|type|mod|module)\s+([A-Za-z_][\w:<>]*)").unwrap()
    })
}

fn candidates(app: &App) -> Vec<(String, Target)> {
    let p = &app.palette;
    match p.mode {
        PaletteMode::Commands => {
            let mut v: Vec<(String, Target)> = MENUS
                .iter()
                .flat_map(|(menu, items)| items.iter().filter(|(l, _)| *l != "-").map(move |(l, c)| (format!("{menu}: {}", l.trim_end_matches('…')), Target::Command(c.to_string()))))
                .collect();
            v.extend(app.syn.syntax_names().into_iter().map(|s| (format!("Set Syntax: {s}"), Target::Syntax(s))));
            v.extend(app.syn.theme_names().into_iter().map(|s| (format!("Color Scheme: {s}"), Target::Theme(s))));
            fuzzy(&p.input, v)
        }
        PaletteMode::Themes => fuzzy(&p.input, app.syn.theme_names().into_iter().map(|t| (t.clone(), Target::Theme(t))).collect()),
        PaletteMode::Syntaxes => fuzzy(&p.input, app.syn.syntax_names().into_iter().map(|t| (t.clone(), Target::Syntax(t))).collect()),
        PaletteMode::Files => {
            if let Some(rest) = p.input.strip_prefix(':') {
                let b = &app.docs[app.active].buf;
                return match rest.trim().parse::<usize>() {
                    Ok(n) => vec![(format!("Go to line {n}"), Target::Pos(b.line_start(n.saturating_sub(1).min(b.rope.len_lines() - 1))))],
                    Err(_) => vec![],
                };
            }
            if let Some(rest) = p.input.strip_prefix('#') {
                if rest.is_empty() {
                    return vec![];
                }
                let b = &app.docs[app.active].buf;
                let text = b.text();
                let mut seen = std::collections::HashSet::new();
                let mut words = Vec::new();
                let mut ci = 0;
                for (bi, w) in text.split_inclusive(|c: char| !sublimers_core::buffer::is_word_char(c)).map(|w| (0, w)) {
                    let _ = bi;
                    let word: String = w.chars().take_while(|c| sublimers_core::buffer::is_word_char(*c)).collect();
                    if word.chars().count() > 1 && seen.insert(word.clone()) {
                        words.push((word, Target::Pos(ci)));
                    }
                    ci += w.chars().count();
                }
                let mut v = fuzzy(rest, words);
                v.truncate(200);
                return v;
            }
            if let Some(rest) = p.input.strip_prefix('@') {
                return fuzzy(rest, symbols(app).into_iter().map(|(s, pos)| (s, Target::Pos(pos))).collect());
            }
            let mut v: Vec<(String, Target)> = app.docs.iter().enumerate().map(|(i, d)| (d.buf.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| d.buf.name()), Target::Doc(i))).collect();
            for f in &app.files {
                if app.docs.iter().any(|d| d.buf.path.as_ref() == Some(f)) {
                    continue;
                }
                let rel = app.root.as_ref().and_then(|r| f.strip_prefix(r).ok()).unwrap_or(f);
                v.push((rel.display().to_string(), Target::File(f.clone())));
            }
            let mut v = fuzzy(&p.input, v);
            v.truncate(500);
            v
        }
    }
}

pub fn show(ctx: &egui::Context, app: &mut App) {
    let items = candidates(app);
    let n = items.len();
    let (up, down, enter, esc) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
        )
    });
    let p = &mut app.palette;
    if down && n > 0 {
        p.selected = (p.selected + 1) % n;
    }
    if up && n > 0 {
        p.selected = (p.selected + n - 1) % n;
    }
    p.selected = p.selected.min(n.saturating_sub(1));
    let mut chosen = None;
    let hint = match p.mode {
        PaletteMode::Files => "Goto Anything  (@symbol, :line, #word)",
        PaletteMode::Commands => "Command",
        PaletteMode::Themes => "Color Scheme",
        PaletteMode::Syntaxes => "Syntax",
    };
    let width = 600.0;
    egui::Area::new(egui::Id::new("palette"))
        .anchor(egui::Align2::CENTER_TOP, [0.0, 60.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            crate::ui_kit::card_frame(&ui.visuals().clone()).inner_margin(egui::Margin::same(10)).corner_radius(14.0).show(ui, |ui| {
                ui.set_width(width);
                let r = ui.add(egui::TextEdit::singleline(&mut p.input).hint_text(hint).desired_width(width).font(egui::FontId::proportional(16.0)).margin(egui::Margin::symmetric(10, 8)));
                ui.add_space(6.0);
                if p.focus {
                    r.request_focus();
                    p.focus = false;
                }
                if r.changed() {
                    p.selected = 0;
                }
                egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                    for (i, (label, _)) in items.iter().enumerate().take(200) {
                        let r = ui.add(egui::Button::new(egui::RichText::new(label).size(14.0)).selected(i == p.selected).min_size(egui::vec2(width, 28.0)));
                        if i == p.selected && (up || down) {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            chosen = Some(i);
                        }
                    }
                });
            });
        });
    if enter && n > 0 {
        chosen = Some(p.selected);
    }
    // Live theme preview while browsing themes.
    if p.mode == PaletteMode::Themes && !esc && chosen.is_none() {
        if let Some((_, Target::Theme(t))) = items.get(p.selected) {
            if p.original_theme.is_none() {
                p.original_theme = Some(app.settings.color_scheme.clone());
            }
            if *t != app.settings.color_scheme {
                app.settings.color_scheme = t.clone();
                let t = t.clone();
                app.set_theme(ctx, &t);
            }
        }
    }
    let p = &mut app.palette;
    if esc {
        p.open = false;
        if let Some(t) = p.original_theme.take() {
            app.set_theme(ctx, &t);
        }
        return;
    }
    // A goto-line preview: move caret as the user types.
    let Some(i) = chosen else { return };
    p.open = false;
    match &items[i].1 {
        Target::File(f) => app.open_file(&f.clone()),
        Target::Doc(d) => app.active = *d,
        Target::Pos(pos) => {
            let d = &mut app.docs[app.active];
            d.buf.set_caret(*pos);
            d.reveal = true;
        }
        Target::Command(c) => app.run_command(ctx, &c.clone()),
        Target::Theme(t) => app.set_theme(ctx, &t.clone()),
        Target::Syntax(s) => {
            if let Some(syn) = app.syn.by_name(s) {
                app.docs[app.active].hl.set_syntax(syn);
            }
        }
    }
}

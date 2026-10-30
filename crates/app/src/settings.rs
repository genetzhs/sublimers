//! User settings and keymap, stored as Sublime-style JSON in ~/.config/sublimers.

use std::path::PathBuf;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use serde::{Deserialize, Serialize};
use sublimers_core::Buffer;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub color_scheme: String,
    pub font_size: f32,
    pub tab_size: usize,
    pub translate_tabs_to_spaces: bool,
    pub word_wrap: bool,
    pub minimap: bool,
    pub line_numbers: bool,
    pub highlight_line: bool,
    pub rulers: Vec<usize>,
    pub trim_trailing_white_space_on_save: bool,
    pub auto_match_enabled: bool,
    /// Start language servers (rust-analyzer, pylsp, gopls, clangd...) found on PATH.
    pub lsp_enabled: bool,
    #[serde(skip)]
    pub keymap: Vec<Binding>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            color_scheme: "Mariana".into(),
            font_size: 16.0,
            tab_size: 4,
            translate_tabs_to_spaces: true,
            word_wrap: false,
            minimap: false,
            line_numbers: true,
            highlight_line: true,
            rulers: vec![],
            trim_trailing_white_space_on_save: false,
            auto_match_enabled: true,
            lsp_enabled: true,
            keymap: default_keymap(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Binding {
    pub keys: Vec<String>,
    pub command: String,
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("sublimers")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("Preferences.sublime-settings")
}

pub fn keymap_path() -> PathBuf {
    config_dir().join("Default.sublime-keymap")
}

pub fn ensure_keymap() {
    let p = keymap_path();
    if !p.exists() {
        let _ = std::fs::create_dir_all(config_dir());
        let _ = std::fs::write(&p, serde_json::to_string_pretty(&default_keymap()).unwrap());
    }
}

impl Settings {
    pub fn load() -> Self {
        let mut s: Settings = std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        s.keymap = default_keymap();
        if let Some(user) = std::fs::read_to_string(keymap_path()).ok().and_then(|t| serde_json::from_str::<Vec<Binding>>(&t).ok()) {
            // User bindings override defaults with the same keys.
            for b in user {
                s.keymap.retain(|d| d.keys != b.keys);
                s.keymap.push(b);
            }
        }
        s
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(config_dir());
        let _ = std::fs::write(settings_path(), serde_json::to_string_pretty(self).unwrap());
    }

    pub fn apply_to(&self, b: &mut Buffer) {
        b.tab_size = self.tab_size.max(1);
        b.translate_tabs = self.translate_tabs_to_spaces;
        b.auto_pair = self.auto_match_enabled;
    }
}

/// Sublime Text's default Linux key bindings (for the commands implemented here).
fn default_keymap() -> Vec<Binding> {
    let mut v: Vec<(&str, &str)> = vec![
        // File
        ("ctrl+n", "new_file"),
        ("ctrl+o", "open_file"),
        ("ctrl+s", "save"),
        ("ctrl+shift+s", "save_as"),
        ("ctrl+w", "close_file"),
        ("ctrl+f4", "close_file"),
        ("ctrl+shift+n", "new_window"),
        ("ctrl+shift+w", "close_window"),
        ("ctrl+q", "exit"),
        ("ctrl+shift+t", "reopen_last_file"),
        // Tabs
        ("ctrl+pagedown", "next_view"),
        ("ctrl+pageup", "prev_view"),
        ("ctrl+tab", "next_view"),
        ("ctrl+shift+tab", "prev_view"),
        // Undo
        ("ctrl+z", "undo"),
        ("ctrl+shift+z", "redo"),
        ("ctrl+y", "redo"),
        ("ctrl+u", "soft_undo"),
        ("ctrl+shift+u", "soft_redo"),
        // Selection
        ("ctrl+a", "select_all"),
        ("ctrl+l", "expand_selection_to_line"),
        ("ctrl+d", "find_under_expand"),
        ("ctrl+k, ctrl+d", "find_under_expand_skip"),
        ("alt+f3", "find_all_under"),
        ("ctrl+shift+l", "split_selection_into_lines"),
        ("alt+shift+up", "add_cursor_above"),
        ("alt+shift+down", "add_cursor_below"),
        ("ctrl+shift+m", "expand_selection_to_brackets"),
        ("escape", "single_selection"),
        // Editing
        ("ctrl+shift+d", "duplicate_line"),
        ("ctrl+shift+k", "delete_line"),
        ("ctrl+shift+up", "swap_line_up"),
        ("ctrl+shift+down", "swap_line_down"),
        ("ctrl+]", "indent"),
        ("ctrl+[", "unindent"),
        ("shift+tab", "unindent"),
        ("ctrl+/", "toggle_comment"),
        ("ctrl+j", "join_lines"),
        ("ctrl+enter", "insert_line_after"),
        ("ctrl+shift+enter", "insert_line_before"),
        ("ctrl+m", "move_to_bracket"),
        ("ctrl+k, ctrl+k", "delete_to_eol"),
        ("ctrl+k, ctrl+backspace", "delete_to_bol"),
        ("ctrl+shift+backspace", "delete_to_bol"),
        ("ctrl+k, ctrl+u", "upper_case"),
        ("ctrl+k, ctrl+l", "lower_case"),
        ("f9", "sort_lines"),
        ("ctrl+f9", "sort_lines_case_sensitive"),
        // Find
        ("ctrl+f", "show_find"),
        ("ctrl+i", "show_find"),
        ("ctrl+h", "show_replace"),
        ("ctrl+shift+f", "show_find_in_files"),
        ("f3", "find_next"),
        ("shift+f3", "find_prev"),
        ("ctrl+f3", "find_under"),
        ("ctrl+shift+f3", "find_under_prev"),
        ("ctrl+e", "slurp_find_string"),
        // Goto
        ("ctrl+p", "show_overlay_goto"),
        ("ctrl+shift+p", "show_overlay_command"),
        ("ctrl+r", "goto_symbol"),
        ("ctrl+shift+r", "goto_symbol"),
        ("ctrl+g", "goto_line"),
        ("ctrl+;", "goto_word"),
        // View
        ("ctrl+k, ctrl+b", "toggle_side_bar"),
        ("f11", "toggle_full_screen"),
        ("shift+f11", "toggle_distraction_free"),
        // Split view (Sublime: View → Layout, Focus Group, Move File to Group)
        ("alt+shift+1", "set_layout_1"),
        ("alt+shift+2", "set_layout_2"),
        ("alt+shift+3", "set_layout_3"),
        ("alt+shift+4", "set_layout_4"),
        ("alt+shift+8", "set_layout_rows_2"),
        ("alt+shift+9", "set_layout_rows_3"),
        ("alt+shift+5", "set_layout_grid"),
        ("ctrl+1", "focus_group_1"),
        ("ctrl+2", "focus_group_2"),
        ("ctrl+3", "focus_group_3"),
        ("ctrl+4", "focus_group_4"),
        ("ctrl+shift+1", "move_to_group_1"),
        ("ctrl+shift+2", "move_to_group_2"),
        ("ctrl+shift+3", "move_to_group_3"),
        ("ctrl+shift+4", "move_to_group_4"),
        // Code folding
        ("ctrl+shift+[", "fold"),
        ("ctrl+shift+]", "unfold"),
        ("ctrl+k, ctrl+0", "unfold_all"),
        ("ctrl+k, ctrl+j", "unfold_all"),
        ("ctrl+k, ctrl+1", "fold_level_1"),
        ("ctrl+k, ctrl+2", "fold_level_2"),
        ("ctrl+k, ctrl+3", "fold_level_3"),
        ("ctrl+k, ctrl+4", "fold_level_4"),
        // Bookmarks
        ("f12", "goto_definition"),
        ("ctrl+`", "toggle_terminal"),
        ("ctrl+b", "build"),
        ("f7", "build"),
        ("ctrl+shift+b", "build_with"),
        ("f4", "next_result"),
        ("shift+f4", "prev_result"),
        ("ctrl+alt+f", "lsp_format"),
        ("ctrl+f2", "toggle_bookmark"),
        ("f2", "next_bookmark"),
        ("shift+f2", "prev_bookmark"),
        ("ctrl+shift+f2", "clear_bookmarks"),
        ("alt+f2", "select_all_bookmarks"),
        // Completion
        ("ctrl+space", "auto_complete"),
        ("ctrl+=", "increase_font_size"),
        ("ctrl+shift+=", "increase_font_size"),
        ("ctrl+-", "decrease_font_size"),
    ];
    let tabs: Vec<(String, String)> = (1..=9).map(|n| (format!("alt+{n}"), format!("select_by_index_{}", n - 1))).chain([("alt+0".to_string(), "select_by_index_9".to_string())]).collect();
    let mut out: Vec<Binding> = v.drain(..).map(|(k, c)| Binding { keys: k.split(", ").map(String::from).collect(), command: c.into() }).collect();
    out.extend(tabs.into_iter().map(|(k, c)| Binding { keys: vec![k], command: c }));
    out
}

fn parse_key(name: &str) -> Option<Key> {
    let k = match name {
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "enter" => Key::Enter,
        "escape" => Key::Escape,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "/" => Key::Slash,
        "[" => Key::OpenBracket,
        "]" => Key::CloseBracket,
        "=" | "+" => Key::Equals,
        "-" => Key::Minus,
        "," => Key::Comma,
        "." => Key::Period,
        ";" => Key::Semicolon,
        "space" => Key::Space,
        other => return Key::from_name(&other.to_uppercase()).or_else(|| Key::from_name(other)),
    };
    Some(k)
}

pub fn parse_shortcut(s: &str) -> Option<KeyboardShortcut> {
    let mut m = Modifiers::NONE;
    let parts: Vec<&str> = s.split('+').collect();
    // "ctrl+=" and "ctrl++" end with an empty part for '+'.
    let (key, mods) = if s.ends_with("++") { ("+", &parts[..parts.len() - 2]) } else { (*parts.last()?, &parts[..parts.len() - 1]) };
    for p in mods {
        match *p {
            "ctrl" | "super" | "primary" => m = m | Modifiers::COMMAND,
            "shift" => m = m | Modifiers::SHIFT,
            "alt" => m = m | Modifiers::ALT,
            _ => return None,
        }
    }
    Some(KeyboardShortcut::new(m, parse_key(key)?))
}

const ALWAYS: &[&str] = &["show_overlay_goto", "show_overlay_command", "show_find", "show_replace", "save", "single_selection", "find_next", "find_prev"];

fn hit(sc: &KeyboardShortcut, key: Key, m: Modifiers) -> bool {
    key == sc.logical_key && m.command == sc.modifiers.command && m.shift == sc.modifiers.shift && m.alt == sc.modifiers.alt
}

/// Consume key presses matching bindings (including two-key chords like "ctrl+k, ctrl+b")
/// and return the commands to run.
pub fn match_keys(ctx: &egui::Context, keymap: &[Binding], typing_elsewhere: bool) -> Vec<String> {
    let parsed: Vec<(Vec<KeyboardShortcut>, &str)> = keymap
        .iter()
        .filter(|b| !typing_elsewhere || ALWAYS.contains(&b.command.as_str()))
        .filter_map(|b| Some((b.keys.iter().map(|k| parse_shortcut(k)).collect::<Option<Vec<_>>>()?, b.command.as_str())))
        .filter(|(k, _)| !k.is_empty())
        .collect();
    let chord_id = egui::Id::new("chord_prefix");
    let mut prefix: Option<KeyboardShortcut> = ctx.data(|d| d.get_temp(chord_id)).flatten();
    let mut out = Vec::new();
    ctx.input_mut(|i| {
        let mut idx = 0;
        while idx < i.events.len() {
            let (key, m) = match &i.events[idx] {
                // Prefer the physical key so shortcuts work with shift and non-US layouts (e.g. Greek).
                egui::Event::Key { key, physical_key, pressed: true, modifiers, .. } => (physical_key.unwrap_or(*key), *modifiers),
                _ => {
                    // Any typed text cancels a pending chord.
                    if matches!(i.events[idx], egui::Event::Text(_)) {
                        prefix = None;
                    }
                    idx += 1;
                    continue;
                }
            };
            let mut consumed = false;
            if let Some(p) = prefix.take() {
                if let Some((_, cmd)) = parsed.iter().find(|(k, _)| k.len() == 2 && k[0] == p && hit(&k[1], key, m)) {
                    out.push(cmd.to_string());
                }
                consumed = true; // an unmatched second key is swallowed, like Sublime
            } else if let Some((_, cmd)) = parsed.iter().find(|(k, _)| k.len() == 1 && hit(&k[0], key, m)) {
                out.push(cmd.to_string());
                consumed = true;
            } else if let Some((k, _)) = parsed.iter().find(|(k, _)| k.len() == 2 && hit(&k[0], key, m)) {
                prefix = Some(k[0]);
                consumed = true;
            }
            if consumed {
                i.events.remove(idx);
            } else {
                idx += 1;
            }
        }
    });
    ctx.data_mut(|d| d.insert_temp(chord_id, prefix));
    out
}

/// Human-readable shortcut for a command, e.g. "Ctrl+Shift+P" or "Ctrl+K, Ctrl+B".
pub fn shortcut_label(keymap: &[Binding], command: &str) -> String {
    // User bindings are appended last, so prefer the most recent one.
    let Some(b) = keymap.iter().rev().find(|b| b.command == command) else { return String::new() };
    b.keys
        .iter()
        .map(|k| {
            k.split('+')
                .map(|p| match p {
                    "ctrl" => "Ctrl".to_string(),
                    "shift" => "Shift".into(),
                    "alt" => "Alt".into(),
                    "pageup" => "PgUp".into(),
                    "pagedown" => "PgDn".into(),
                    "escape" => "Esc".into(),
                    "backspace" => "Backspace".into(),
                    "enter" => "Enter".into(),
                    "tab" => "Tab".into(),
                    "up" => "↑".into(),
                    "down" => "↓".into(),
                    "" => "+".into(),
                    other => other.to_uppercase(),
                })
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod backtick_tests {
    #[test]
    fn parses_ctrl_backtick() {
        assert!(super::parse_shortcut("ctrl+`").is_some());
    }
}

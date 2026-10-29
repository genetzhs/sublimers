//! Integrated terminal (Ctrl+`): the user's shell in a pseudo-terminal, drawn in the
//! bottom panel. `vt100` keeps the screen state; this file turns it into egui text and
//! turns key presses into the bytes a terminal would send.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};

use eframe::egui::{self, pos2, vec2, Color32, FontId, Rect, Sense};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

pub struct Terminal {
    parser: vt100::Parser,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    rx: Receiver<Vec<u8>>,
    size: (u16, u16),
    pub exited: bool,
    /// Lines scrolled back into history (0 = live screen).
    scrollback: usize,
}

impl Terminal {
    pub fn spawn(cwd: &Path) -> Result<Self, String> {
        let pty = native_pty_system();
        let size = (24, 80);
        let pair = pty.openpty(PtySize { rows: size.0, cols: size.1, pixel_width: 0, pixel_height: 0 }).map_err(|e| e.to_string())?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        let mut cmd = CommandBuilder::new(shell);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Ok(Terminal { parser: vt100::Parser::new(size.0, size.1, 5000), master: pair.master, writer, child, rx, size, exited: false, scrollback: 0 })
    }

    /// Feed shell output into the screen; true if something changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(b) = self.rx.try_recv() {
            self.parser.process(&b);
            changed = true;
        }
        if changed && self.scrollback > 0 {
            self.scrollback = 0;
            self.parser.set_scrollback(0);
        }
        if !self.exited && self.child.try_wait().ok().flatten().is_some() {
            self.exited = true;
            changed = true;
        }
        changed
    }

    fn send(&mut self, b: &[u8]) {
        let _ = self.writer.write_all(b);
        let _ = self.writer.flush();
        if self.scrollback > 0 {
            self.scrollback = 0;
            self.parser.set_scrollback(0);
        }
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        if (rows, cols) != self.size && rows > 1 && cols > 1 {
            self.size = (rows, cols);
            self.parser.set_size(rows, cols);
            let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
    }

    /// Draw the terminal and handle its input. Returns whether it has keyboard focus.
    pub fn show(&mut self, ui: &mut egui::Ui, font_size: f32, bg: Color32, fg: Color32, want_focus: bool) -> bool {
        let font = FontId::monospace(font_size);
        let char_w = ui.fonts(|f| f.glyph_width(&font, 'M'));
        let row_h = ui.fonts(|f| f.row_height(&font)).ceil();
        let rect = ui.available_rect_before_wrap();
        let id = egui::Id::new("terminal");
        let resp = ui.interact(rect, id, Sense::click());
        if resp.clicked() || want_focus {
            resp.request_focus();
        }
        ui.ctx().memory_mut(|m| m.set_focus_lock_filter(id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true }));
        let focused = resp.has_focus();
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }

        let rows = ((rect.height() - 4.0) / row_h).floor().max(2.0) as u16;
        let cols = ((rect.width() - 8.0) / char_w).floor().max(10.0) as u16;
        self.resize(rows, cols);

        if focused {
            self.handle_input(ui.ctx());
        }
        if resp.hovered() {
            let dy = ui.input(|i| i.raw_scroll_delta.y);
            if dy != 0.0 {
                let lines = (dy / row_h).round() as i64;
                let n = (self.scrollback as i64 + lines).clamp(0, 5000) as usize;
                self.parser.set_scrollback(n);
                self.scrollback = self.parser.screen().scrollback();
            }
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, bg);
        let screen = self.parser.screen();
        let (cr, cc) = screen.cursor_position();
        let left = rect.left() + 4.0;
        let top = rect.top() + 2.0;
        for r in 0..rows {
            let y = top + r as f32 * row_h;
            let mut job = egui::text::LayoutJob::default();
            let mut c = 0;
            while c < cols {
                let Some(cell) = screen.cell(r, c) else { break };
                let (mut f, mut b) = (color(cell.fgcolor(), fg), color(cell.bgcolor(), Color32::TRANSPARENT));
                if cell.inverse() {
                    std::mem::swap(&mut f, &mut b);
                    if f == Color32::TRANSPARENT {
                        f = bg;
                    }
                    if b == fg || b == Color32::TRANSPARENT {
                        b = fg;
                    }
                }
                if cell.bold() && f == fg {
                    f = Color32::WHITE;
                }
                let s = if cell.has_contents() { cell.contents() } else { " ".to_string() };
                if b != Color32::TRANSPARENT {
                    let x = left + c as f32 * char_w;
                    painter.rect_filled(Rect::from_min_size(pos2(x, y), vec2(char_w * if cell.is_wide() { 2.0 } else { 1.0 }, row_h)), 0.0, b);
                }
                job.append(&s, 0.0, egui::TextFormat { font_id: font.clone(), color: f, italics: cell.italic(), underline: if cell.underline() { egui::Stroke::new(1.0_f32, f) } else { egui::Stroke::NONE }, ..Default::default() });
                c += if cell.is_wide() { 2 } else { 1 };
            }
            let galley = ui.fonts(|f| f.layout_job(job));
            painter.galley(pos2(left, y), galley, fg);
        }
        if !screen.hide_cursor() && self.scrollback == 0 {
            let r = Rect::from_min_size(pos2(left + cc as f32 * char_w, top + cr as f32 * row_h), vec2(char_w, row_h));
            if focused {
                painter.rect_filled(r, 1.0, fg.gamma_multiply(0.7));
            } else {
                painter.rect_stroke(r, 1.0, egui::Stroke::new(1.0_f32, fg.gamma_multiply(0.6)), egui::StrokeKind::Inside);
            }
        }
        if self.exited {
            painter.text(rect.right_bottom() - vec2(10.0, 8.0), egui::Align2::RIGHT_BOTTOM, "[process exited — Ctrl+` twice to restart]", FontId::proportional(12.0), fg.gamma_multiply(0.6));
        }
        ui.advance_cursor_after_rect(rect);
        focused
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        use egui::{Event, Key};
        let app_cursor = self.parser.screen().application_cursor();
        let events = ctx.input(|i| i.events.clone());
        for e in events {
            match e {
                Event::Text(t) => self.send(t.as_bytes()),
                Event::Paste(t) => {
                    if self.parser.screen().bracketed_paste() {
                        self.send(format!("\x1b[200~{t}\x1b[201~").as_bytes());
                    } else {
                        self.send(t.replace("\r\n", "\r").replace('\n', "\r").as_bytes());
                    }
                }
                Event::Copy => {
                    // Ctrl+Shift+C copies in terminals; plain Ctrl+C is the interrupt.
                    if !ctx.input(|i| i.modifiers.shift) {
                        self.send(b"\x03");
                    }
                }
                Event::Cut => self.send(b"\x18"),
                Event::Key { key, pressed: true, modifiers: m, .. } => {
                    let arrow = |c: char| if m.ctrl { format!("\x1b[1;5{c}") } else if app_cursor { format!("\x1bO{c}") } else { format!("\x1b[{c}") };
                    let seq: Option<String> = match key {
                        Key::Enter => Some("\r".into()),
                        Key::Backspace => Some(if m.ctrl { "\x17".into() } else { "\x7f".into() }),
                        Key::Tab => Some(if m.shift { "\x1b[Z".into() } else { "\t".into() }),
                        Key::Escape => Some("\x1b".into()),
                        Key::ArrowUp => Some(arrow('A')),
                        Key::ArrowDown => Some(arrow('B')),
                        Key::ArrowRight => Some(arrow('C')),
                        Key::ArrowLeft => Some(arrow('D')),
                        Key::Home => Some("\x1b[H".into()),
                        Key::End => Some("\x1b[F".into()),
                        Key::PageUp => Some("\x1b[5~".into()),
                        Key::PageDown => Some("\x1b[6~".into()),
                        Key::Delete => Some("\x1b[3~".into()),
                        Key::Insert => Some("\x1b[2~".into()),
                        _ if m.ctrl && !m.shift && !m.alt => {
                            // Ctrl+letter -> control character (Ctrl+A = 0x01 ...).
                            let name = key.name();
                            let ch = name.chars().next().filter(|c| name.len() == 1 && c.is_ascii_alphabetic());
                            ch.map(|c| ((c.to_ascii_uppercase() as u8 - b'A' + 1) as char).to_string())
                                .or_else(|| match key {
                                    Key::OpenBracket => Some("\x1b".into()),
                                    Key::Backslash => Some("\x1c".into()),
                                    Key::CloseBracket => Some("\x1d".into()),
                                    Key::Space => Some("\0".into()),
                                    _ => None,
                                })
                        }
                        _ if m.alt && !m.ctrl => {
                            let name = key.name();
                            (name.len() == 1).then(|| format!("\x1b{}", name.to_lowercase()))
                        }
                        _ => None,
                    };
                    if let Some(s) = seq {
                        // Copy/Cut/Paste already arrive as their own events.
                        let is_clip = m.ctrl && matches!(key, Key::C | Key::X | Key::V);
                        if !is_clip {
                            self.send(s.as_bytes());
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn color(c: vt100::Color, default: Color32) -> Color32 {
    const BASE: [(u8, u8, u8); 16] = [
        (0x1d, 0x1f, 0x21), (0xe0, 0x6c, 0x75), (0x98, 0xc3, 0x79), (0xe5, 0xc0, 0x7b),
        (0x61, 0xaf, 0xef), (0xc6, 0x78, 0xdd), (0x56, 0xb6, 0xc2), (0xc8, 0xcc, 0xd4),
        (0x5c, 0x63, 0x70), (0xf0, 0x80, 0x88), (0xb5, 0xe0, 0x90), (0xf5, 0xd0, 0x8b),
        (0x82, 0xc4, 0xff), (0xd8, 0x9a, 0xee), (0x7a, 0xd4, 0xdf), (0xff, 0xff, 0xff),
    ];
    match c {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        vt100::Color::Idx(i) => match i {
            0..=15 => {
                let (r, g, b) = BASE[i as usize];
                Color32::from_rgb(r, g, b)
            }
            16..=231 => {
                let i = i - 16;
                let v = |x: u8| if x == 0 { 0 } else { 55 + x * 40 };
                Color32::from_rgb(v(i / 36), v((i / 6) % 6), v(i % 6))
            }
            _ => {
                let g = 8 + (i - 232) * 10;
                Color32::from_gray(g)
            }
        },
    }
}

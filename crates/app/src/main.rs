mod editor;
mod git;
mod palette;
mod session;
mod settings;
mod updater;

use std::path::{Path, PathBuf};

use eframe::egui;
use sublimers_core::{Buffer, Query};
use sublimers_syntax::{DocHighlight, Syntaxes};

use palette::{Palette, PaletteMode};
use settings::Settings;

pub struct Doc {
    /// Stable identity, so async results (e.g. a Save As dialog) find the right tab.
    pub id: u64,
    pub buf: Buffer,
    pub hl: DocHighlight,
    /// Request to scroll the primary caret into view on next frame.
    pub reveal: bool,
    pub scroll: eframe::egui::Vec2,
    /// Split-view group (pane) this tab lives in.
    pub group: usize,
    pub rows: editor::RowCache,
    pub completion: Option<editor::Completion>,
    /// Completion word list and the buffer version it was built from.
    pub words: (u64, Vec<String>),
    /// Preview tab (single click in the sidebar): replaced by the next preview until edited.
    pub preview: bool,
    /// The file changed on disk while this buffer had unsaved edits.
    pub disk_conflict: bool,
    pub disk_deleted: bool,
    /// File content at git HEAD: None = not loaded yet, Some(None) = untracked / no repo.
    pub git_base: Option<Option<String>>,
    /// Changed-line markers and the buffer version they were computed for.
    pub git_marks: (u64, Vec<Option<git::LineChange>>),
}

impl Doc {
    fn new(buf: Buffer, hl: DocHighlight, group: usize) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self { id, buf, hl, reveal: false, scroll: Default::default(), group, rows: Default::default(), completion: None, words: (0, Vec::new()), preview: false, disk_conflict: false, disk_deleted: false, git_base: None, git_marks: (u64::MAX, Vec::new()) }
    }
}

/// Split-view layout (Sublime's View → Layout).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Default)]
pub enum Layout {
    #[default]
    Single,
    Columns(usize),
    Rows(usize),
    Grid,
}

impl Layout {
    fn groups(self) -> usize {
        match self {
            Layout::Single => 1,
            Layout::Columns(n) | Layout::Rows(n) => n,
            Layout::Grid => 4,
        }
    }

    fn rects(self, r: egui::Rect) -> Vec<egui::Rect> {
        let n = self.groups();
        match self {
            Layout::Single => vec![r],
            Layout::Columns(_) => (0..n).map(|i| egui::Rect::from_min_max(egui::pos2(r.left() + r.width() * i as f32 / n as f32, r.top()), egui::pos2(r.left() + r.width() * (i + 1) as f32 / n as f32, r.bottom()))).collect(),
            Layout::Rows(_) => (0..n).map(|i| egui::Rect::from_min_max(egui::pos2(r.left(), r.top() + r.height() * i as f32 / n as f32), egui::pos2(r.right(), r.top() + r.height() * (i + 1) as f32 / n as f32))).collect(),
            Layout::Grid => {
                let c = r.center();
                vec![
                    egui::Rect::from_min_max(r.min, c),
                    egui::Rect::from_min_max(egui::pos2(c.x, r.top()), egui::pos2(r.right(), c.y)),
                    egui::Rect::from_min_max(egui::pos2(r.left(), c.y), egui::pos2(c.x, r.bottom())),
                    egui::Rect::from_min_max(c, r.max),
                ]
            }
        }
    }
}

#[derive(Default)]
struct FindPanel {
    open: bool,
    replace: bool,
    in_files: bool,
    query: Query,
    replacement: String,
    focus: bool,
    status: String,
    results: Vec<(PathBuf, usize, String)>,
}

pub struct App {
    syn: Syntaxes,
    settings: Settings,
    docs: Vec<Doc>,
    active: usize,
    root: Option<PathBuf>,
    files: Vec<PathBuf>,
    files_rx: Option<std::sync::mpsc::Receiver<Vec<PathBuf>>>,
    palette: Palette,
    find: FindPanel,
    show_sidebar: bool,
    status: String,
    pending_close: Option<usize>,
    closed: Vec<PathBuf>,
    last_session_save: std::time::Instant,
    fullscreen: bool,
    layout: Layout,
    /// Focused split-view group.
    focus: usize,
    /// The selected tab (doc index) of each group.
    group_active: Vec<Option<usize>>,
    last_disk_check: std::time::Instant,
    file_dialog: Option<FileDialog>,
    /// This window's session file (each window has its own; see `session`).
    session_file: PathBuf,
    primary: bool,
    update: UpdateState,
    update_rx: Option<std::sync::mpsc::Receiver<UpdateState>>,
    /// Show the update window (manual check); automatic checks only show a status-bar notice.
    update_window: bool,
    /// Result of a native file dialog running on a background thread.
    dialog_rx: Option<std::sync::mpsc::Receiver<DialogResult>>,
    git_status: std::collections::HashMap<PathBuf, git::FileStatus>,
    git_head: Option<String>,
    git_rx: Option<std::sync::mpsc::Receiver<(std::collections::HashMap<PathBuf, git::FileStatus>, Option<String>)>>,
    last_git_check: Option<std::time::Instant>,
}

/// Native file dialogs run off the UI thread: a blocking dialog freezes the window, and
/// after a few seconds the desktop offers to force-quit the "unresponsive" app.
enum DialogResult {
    Files(Vec<PathBuf>),
    Folder(PathBuf),
    SaveAs { doc: u64, path: PathBuf, close_after: bool },
    Cancelled,
}

#[derive(Clone, Debug, Default)]
enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(updater::Release),
    Installing,
    Done(String),
    Error(String),
}

/// Sidebar file operations awaiting user input.
enum FileDialog {
    NewFile(PathBuf, String),
    NewFolder(PathBuf, String),
    Rename(PathBuf, String),
    Delete(PathBuf),
}

impl App {
    fn new(cc: &eframe::CreationContext, args: Vec<PathBuf>, window: session::Window) -> Self {
        trace("App::new start");
        let settings = Settings::load();
        let mut app = Self {
            syn: Syntaxes::load(Some(&settings::config_dir())),
            settings,
            docs: Vec::new(),
            active: 0,
            root: None,
            files: Vec::new(),
            files_rx: None,
            palette: Palette::default(),
            find: FindPanel::default(),
            show_sidebar: true,
            status: String::new(),
            pending_close: None,
            closed: Vec::new(),
            last_session_save: std::time::Instant::now(),
            fullscreen: false,
            layout: Layout::Single,
            focus: 0,
            group_active: vec![None],
            last_disk_check: std::time::Instant::now(),
            file_dialog: None,
            session_file: session::main_path(),
            primary: true,
            update: UpdateState::Idle,
            update_rx: None,
            update_window: false,
            dialog_rx: None,
            git_status: Default::default(),
            git_head: None,
            git_rx: None,
            last_git_check: None,
        };
        trace("syntaxes loaded");
        match window {
            session::Window::Main => {
                session::claim(&app.session_file);
                app.restore_session();
                // Reopen the other windows from last time.
                session::spawn_orphan_windows();
            }
            session::Window::Restore(f) => {
                app.session_file = f;
                app.primary = false;
                session::claim(&app.session_file);
                app.restore_session();
            }
            session::Window::New => {
                app.session_file = session::new_window_path();
                app.primary = false;
                session::claim(&app.session_file);
            }
        }
        trace("session restored");
        for a in args {
            if a.is_dir() {
                app.open_folder(&a);
            } else {
                app.open_file(&a);
            }
        }
        if app.docs.is_empty() {
            app.new_file();
        }
        app.apply_visuals(&cc.egui_ctx);
        if app.primary && updater::due_for_auto_check() {
            app.check_updates(false);
        }
        app
    }

    fn new_file(&mut self) {
        let mut buf = Buffer::default();
        self.settings.apply_to(&mut buf);
        let hl = DocHighlight::new(self.syn.set.find_syntax_plain_text());
        self.docs.push(Doc::new(buf, hl, self.focus));
        self.active = self.docs.len() - 1;
    }

    pub fn open_file(&mut self, path: &Path) {
        self.open_file_ext(path, false);
    }

    /// Open (or switch to) a file; `preview` opens it in the transient preview tab.
    pub fn open_file_ext(&mut self, path: &Path, preview: bool) {
        let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let path = abs.as_path();
        if let Some(i) = self.docs.iter().position(|d| d.buf.path.as_deref() == Some(path)) {
            self.active = i;
            if !preview {
                self.docs[i].preview = false;
            }
            self.sync_groups();
            return;
        }
        // Reuse this group's preview tab.
        if preview {
            if let Some(i) = self.docs.iter().position(|d| d.preview && d.group == self.focus && !d.buf.is_dirty()) {
                if let Ok(mut buf) = Buffer::open(path) {
                    self.settings.apply_to(&mut buf);
                    let first = buf.line_text(0);
                    let hl = DocHighlight::new(self.syn.detect(Some(path), &first));
                    let mut d = Doc::new(buf, hl, self.focus);
                    d.preview = true;
                    self.docs[i] = d;
                    self.active = i;
                    self.sync_groups();
                }
                return;
            }
        }
        match Buffer::open(path) {
            Ok(mut buf) => {
                self.settings.apply_to(&mut buf);
                let first = buf.line_text(0);
                let hl = DocHighlight::new(self.syn.detect(Some(path), &first));
                // Replace a pristine untitled tab.
                if self.docs.len() == 1 && self.docs[0].buf.path.is_none() && self.docs[0].buf.is_empty() && !self.docs[0].buf.is_dirty() {
                    self.docs.clear();
                }
                let mut d = Doc::new(buf, hl, self.focus);
                d.preview = preview;
                self.docs.push(d);
                self.active = self.docs.len() - 1;
                self.sync_groups();
            }
            Err(e) => self.status = format!("Cannot open {}: {e}", path.display()),
        }
    }

    fn open_folder(&mut self, dir: &Path) {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        self.root = Some(dir.clone());
        self.files.clear();
        // Index files in the background so a large folder never blocks startup.
        let (tx, rx) = std::sync::mpsc::channel();
        self.files_rx = Some(rx);
        std::thread::spawn(move || {
            let mut files: Vec<PathBuf> = ignore::WalkBuilder::new(&dir)
                .build()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
                .map(|e| e.into_path())
                .take(200_000)
                .collect();
            files.sort();
            let _ = tx.send(files);
        });
    }

    /// Hot exit: remember open files, unsaved contents and carets across restarts.
    fn save_session(&self) {
        let docs = self
            .docs
            .iter()
            .filter(|d| d.buf.path.is_some() || !d.buf.is_empty())
            .map(|d| session::SessionDoc {
                path: d.buf.path.clone(),
                content: d.buf.is_dirty().then(|| d.buf.text()),
                caret: d.buf.sels.last().map_or(0, |s| s.head),
                syntax: d.hl.syntax_name.clone(),
                scroll_y: d.scroll.y,
                group: d.group,
                folds: d.buf.folds.clone(),
                bookmarks: d.buf.bookmarks.clone(),
                encoding: Some(d.buf.encoding.clone()),
                line_ending: Some(d.buf.line_ending.name().into()),
            })
            .collect();
        session::save(&self.session_file, &session::Session { docs, active: self.active, root: self.root.clone(), show_sidebar: self.show_sidebar, layout: self.layout });
    }

    fn restore_session(&mut self) {
        let Some(sess) = session::load(&self.session_file) else { return };
        for sd in sess.docs {
            let mut buf = match (&sd.content, &sd.path) {
                (Some(text), path) => {
                    let mut b = Buffer::from_str(text);
                    b.path = path.clone();
                    b.mark_dirty();
                    // Watch the file from now on, so later external edits are noticed.
                    b.ignore_disk_change();
                    if let Some(e) = sd.encoding.as_deref() {
                        b.encoding = e.to_string();
                    }
                    if let Some(le) = sublimers_core::LineEnding::ALL.into_iter().find(|l| Some(l.name()) == sd.line_ending.as_deref()) {
                        b.line_ending = le;
                    }
                    b
                }
                (None, Some(p)) => match Buffer::open(p) {
                    Ok(b) => b,
                    Err(_) => continue,
                },
                (None, None) => continue,
            };
            self.settings.apply_to(&mut buf);
            let syntax = self.syn.by_name(&sd.syntax).unwrap_or_else(|| self.syn.detect(sd.path.as_deref(), &buf.line_text(0)));
            let hl = DocHighlight::new(syntax);
            buf.set_caret(sd.caret);
            let mut doc = Doc::new(buf, hl, sd.group.min(editor::MAX_GROUPS - 1));
            doc.scroll = egui::vec2(0.0, sd.scroll_y);
            doc.buf.folds = sd.folds.into_iter().filter(|&(a, b)| a < b && b <= doc.buf.len()).collect();
            doc.buf.bookmarks = sd.bookmarks.into_iter().filter(|&m| m <= doc.buf.len()).collect();
            self.docs.push(doc);
        }
        self.active = sess.active.min(self.docs.len().saturating_sub(1));
        self.layout = sess.layout;
        self.group_active = vec![None; self.layout.groups()];
        for d in &mut self.docs {
            d.group = d.group.min(self.layout.groups() - 1);
        }
        self.sync_groups();
        self.show_sidebar = sess.show_sidebar;
        // Relative roots from older sessions depend on the launch directory; ignore them.
        if let Some(r) = sess.root.filter(|r| r.is_absolute() && r.is_dir()) {
            self.open_folder(&r);
        }
    }

    fn doc(&mut self) -> &mut Doc {
        &mut self.docs[self.active]
    }

    fn save(&mut self, save_as: bool) {
        self.save_doc(self.active, save_as, false);
    }

    /// Save tab `i`, asking for a path (asynchronously) when needed.
    fn save_doc(&mut self, i: usize, save_as: bool, close_after: bool) {
        let doc = &self.docs[i];
        if save_as || doc.buf.path.is_none() {
            let (id, name) = (doc.id, doc.buf.name());
            let dir = doc.buf.path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).or_else(|| self.root.clone());
            self.run_dialog(move || {
                let mut d = rfd::FileDialog::new().set_file_name(name);
                if let Some(dir) = dir {
                    d = d.set_directory(dir);
                }
                d.save_file().map_or(DialogResult::Cancelled, |path| DialogResult::SaveAs { doc: id, path, close_after })
            });
            return;
        }
        let path = doc.buf.path.clone().unwrap();
        self.write_doc(i, &path);
        if close_after && !self.docs[i].buf.is_dirty() {
            self.close(i, true);
        }
    }

    fn write_doc(&mut self, i: usize, path: &Path) {
        let doc = &mut self.docs[i];
        if self.settings.trim_trailing_white_space_on_save {
            trim_trailing(&mut doc.buf);
        }
        match doc.buf.save_as(path) {
            Ok(()) => {
                self.status = format!("Saved {}", path.display());
                doc.preview = false;
                doc.disk_deleted = false;
                if doc.hl.syntax_name == "Plain Text" {
                    let first = doc.buf.line_text(0);
                    doc.hl.set_syntax(self.syn.detect(Some(path), &first));
                }
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    /// Refresh `git status` for the sidebar in the background every few seconds.
    fn poll_git(&mut self) {
        if let Some((st, head)) = self.git_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.git_rx = None;
            self.git_status = st;
            if head != self.git_head {
                // A commit/checkout changed HEAD: re-read the base of every file.
                self.git_head = head;
                for d in &mut self.docs {
                    d.git_base = None;
                }
            }
        }
        let due = self.last_git_check.is_none_or(|t| t.elapsed().as_secs() >= 3);
        if !due || self.git_rx.is_some() {
            return;
        }
        self.last_git_check = Some(std::time::Instant::now());
        let dir = self.root.clone().or_else(|| self.docs[self.active].buf.path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)));
        let Some(dir) = dir else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        self.git_rx = Some(rx);
        std::thread::spawn(move || {
            let head = git::repo_root(&dir).and_then(|r| git::head_commit(&r));
            let _ = tx.send((git::status(&dir), head));
        });
    }

    /// Keep the changed-line markers of a visible tab up to date.
    fn update_git_marks(d: &mut Doc) {
        let Some(path) = d.buf.path.clone() else { return };
        if d.git_base.is_none() {
            d.git_base = Some(git::head_content(&path));
            d.git_marks.0 = u64::MAX;
        }
        if d.git_marks.0 == d.buf.version {
            return;
        }
        d.git_marks = match d.git_base.as_ref().unwrap() {
            // Skip huge files: diffing them on every keystroke would be noticeable.
            Some(base) if d.buf.len() < 2_000_000 => (d.buf.version, git::line_changes(base, &d.buf.text())),
            _ => (d.buf.version, Vec::new()),
        };
    }

    fn run_dialog(&mut self, f: impl FnOnce() -> DialogResult + Send + 'static) {
        if self.dialog_rx.is_some() {
            return; // one dialog at a time
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.dialog_rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
    }

    fn poll_dialog(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.dialog_rx else { return };
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        let r = match rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => DialogResult::Cancelled,
        };
        self.dialog_rx = None;
        match r {
            DialogResult::Files(ps) => {
                for p in ps {
                    self.open_file(&p);
                }
            }
            DialogResult::Folder(p) => self.open_folder(&p),
            DialogResult::SaveAs { doc, path, close_after } => {
                if let Some(i) = self.docs.iter().position(|d| d.id == doc) {
                    self.write_doc(i, &path);
                    if close_after && !self.docs[i].buf.is_dirty() {
                        self.close(i, true);
                    }
                }
            }
            DialogResult::Cancelled => {}
        }
    }

    fn close(&mut self, i: usize, force: bool) {
        if !force && self.docs[i].buf.is_dirty() {
            self.pending_close = Some(i);
            return;
        }
        if let Some(p) = self.docs[i].buf.path.clone() {
            self.closed.push(p);
        }
        let g = self.docs[i].group;
        self.docs.remove(i);
        for ga in &mut self.group_active {
            *ga = match *ga {
                Some(x) if x == i => None,
                Some(x) if x > i => Some(x - 1),
                other => other,
            };
        }
        if self.active > i || self.active >= self.docs.len() {
            self.active = self.active.saturating_sub(1);
        }
        // Prefer a neighbour tab in the same group.
        let same: Vec<usize> = (0..self.docs.len()).filter(|&d| self.docs[d].group == g).collect();
        if let Some(&n) = same.iter().find(|&&d| d >= i).or(same.last()) {
            self.active = n;
        } else if g == self.focus && self.layout.groups() > 1 {
            // Empty pane keeps focus with a fresh untitled tab, like an empty Sublime group.
            self.new_file();
        }
        if self.docs.is_empty() {
            self.new_file();
        }
        self.active = self.active.min(self.docs.len() - 1);
        self.sync_groups();
    }

    /// Keep `focus`, `group_active` and the active doc consistent.
    fn sync_groups(&mut self) {
        let n = self.layout.groups();
        self.group_active.resize(n, None);
        for d in &mut self.docs {
            d.group = d.group.min(n - 1);
        }
        if let Some(d) = self.docs.get(self.active) {
            self.focus = d.group;
            self.group_active[self.focus] = Some(self.active);
        }
        for g in 0..n {
            let ok = self.group_active[g].is_some_and(|i| i < self.docs.len() && self.docs[i].group == g);
            if !ok {
                self.group_active[g] = (0..self.docs.len()).rev().find(|&i| self.docs[i].group == g);
            }
        }
    }

    fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
        let n = layout.groups();
        for d in &mut self.docs {
            d.group = d.group.min(n - 1);
        }
        self.group_active = vec![None; n];
        self.sync_groups();
    }

    fn focus_group(&mut self, g: usize) {
        if g >= self.layout.groups() {
            return;
        }
        self.sync_groups();
        match self.group_active[g] {
            Some(i) => self.active = i,
            None => {
                self.focus = g;
                self.new_file();
            }
        }
        self.sync_groups();
    }

    fn move_to_group(&mut self, g: usize) {
        if g >= self.layout.groups() {
            return;
        }
        self.docs[self.active].group = g;
        self.group_active[g] = Some(self.active);
        self.sync_groups();
    }

    /// Notice files changed or deleted by other programs (Sublime reloads clean files silently).
    fn check_disk(&mut self) {
        for d in &mut self.docs {
            match d.buf.disk_state() {
                sublimers_core::DiskState::Unchanged => {}
                sublimers_core::DiskState::Modified if !d.buf.is_dirty() => {
                    if d.buf.reload(None).is_ok() {
                        self.status = format!("Reloaded {} (changed on disk)", d.buf.name());
                    }
                }
                sublimers_core::DiskState::Modified => d.disk_conflict = true,
                sublimers_core::DiskState::Deleted => {
                    if !d.disk_deleted {
                        d.disk_deleted = true;
                        d.buf.mark_dirty();
                        self.status = format!("{} was deleted on disk", d.buf.name());
                    }
                }
            }
        }
    }

    fn file_dialog(&mut self, ctx: &egui::Context) {
        let Some(dlg) = &mut self.file_dialog else { return };
        let (title, prompt) = match dlg {
            FileDialog::NewFile(..) => ("New File", "File name:"),
            FileDialog::NewFolder(..) => ("New Folder", "Folder name:"),
            FileDialog::Rename(..) => ("Rename", "New name:"),
            FileDialog::Delete(..) => ("Delete", ""),
        };
        let (mut ok, mut cancel) = (false, false);
        egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_TOP, [0.0, 120.0]).show(ctx, |ui| {
            match dlg {
                FileDialog::Delete(p) => {
                    ui.label(format!("Move “{}” to the trash?", p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
                }
                FileDialog::NewFile(_, name) | FileDialog::NewFolder(_, name) | FileDialog::Rename(_, name) => {
                    ui.label(prompt);
                    let r = ui.add(egui::TextEdit::singleline(name).desired_width(320.0));
                    r.request_focus();
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        ok = true;
                    }
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(if matches!(dlg, FileDialog::Delete(_)) { "Move to Trash" } else { "OK" }).clicked() {
                    ok = true;
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
            });
        });
        if cancel {
            self.file_dialog = None;
            return;
        }
        if !ok {
            return;
        }
        let result: std::io::Result<()> = match self.file_dialog.take().unwrap() {
            FileDialog::NewFile(dir, name) => {
                let p = dir.join(name.trim());
                let r = std::fs::OpenOptions::new().write(true).create_new(true).open(&p).map(|_| ());
                if r.is_ok() {
                    self.open_file(&p);
                }
                r
            }
            FileDialog::NewFolder(dir, name) => std::fs::create_dir(dir.join(name.trim())),
            FileDialog::Rename(p, name) => {
                let to = p.with_file_name(name.trim());
                let r = std::fs::rename(&p, &to);
                if r.is_ok() {
                    // Keep open tabs pointing at the renamed file(s).
                    for d in &mut self.docs {
                        if let Some(old) = d.buf.path.clone() {
                            if let Ok(rest) = old.strip_prefix(&p) {
                                d.buf.path = Some(if rest.as_os_str().is_empty() { to.clone() } else { to.join(rest) });
                                d.buf.ignore_disk_change();
                            }
                        }
                    }
                }
                r
            }
            FileDialog::Delete(p) => trash::delete(&p).map_err(std::io::Error::other),
        };
        match result {
            Ok(()) => {
                if let Some(r) = self.root.clone() {
                    self.open_folder(&r);
                }
            }
            Err(e) => self.status = format!("Error: {e}"),
        }
    }

    fn check_updates(&mut self, show_window: bool) {
        self.update_window |= show_window;
        self.update = UpdateState::Checking;
        let (tx, rx) = std::sync::mpsc::channel();
        self.update_rx = Some(rx);
        std::thread::spawn(move || {
            let st = match updater::fetch_latest() {
                Ok(r) if updater::is_newer(&r.version, updater::CURRENT) => UpdateState::Available(r),
                Ok(_) => UpdateState::UpToDate,
                Err(e) => UpdateState::Error(e),
            };
            let _ = tx.send(st);
        });
    }

    fn install_update(&mut self, rel: updater::Release) {
        self.update = UpdateState::Installing;
        self.update_window = true;
        let (tx, rx) = std::sync::mpsc::channel();
        self.update_rx = Some(rx);
        std::thread::spawn(move || {
            let st = match updater::apply(&rel, &updater::install_kind()) {
                Ok(m) => UpdateState::Done(m),
                Err(e) => UpdateState::Error(e),
            };
            let _ = tx.send(st);
        });
    }

    fn update_ui(&mut self, ctx: &egui::Context) {
        if let Some(st) = self.update_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.update = st;
            self.update_rx = None;
        }
        if !self.update_window {
            return;
        }
        let mut open = true;
        let mut install = None;
        let mut restart = false;
        egui::Window::new("Software Update").open(&mut open).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_TOP, [0.0, 120.0]).show(ctx, |ui| {
            ui.set_min_width(380.0);
            ui.label(format!("Installed version: {}", updater::CURRENT));
            ui.add_space(6.0);
            match &self.update {
                UpdateState::Idle | UpdateState::Checking => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Checking for updates…");
                    });
                }
                UpdateState::UpToDate => {
                    ui.label("You're up to date.");
                }
                UpdateState::Available(r) => {
                    ui.label(egui::RichText::new(format!("Version {} is available.", r.version)).strong());
                    if !r.notes.trim().is_empty() {
                        egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| ui.label(&r.notes));
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let label = match updater::install_kind() {
                            updater::Install::AppImage(_) => Some("Download and Replace AppImage"),
                            updater::Install::Deb => Some("Download and Install .deb"),
                            updater::Install::Rpm => Some("Download and Install .rpm"),
                            updater::Install::Other => None,
                        };
                        if let Some(l) = label {
                            if ui.button(l).clicked() {
                                install = Some(r.clone());
                            }
                        }
                        ui.hyperlink_to("Release page", &r.page);
                    });
                }
                UpdateState::Installing => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Downloading and installing…");
                    });
                }
                UpdateState::Done(m) => {
                    ui.label(m);
                    if ui.button("Restart Now").clicked() {
                        restart = true;
                    }
                }
                UpdateState::Error(e) => {
                    ui.colored_label(egui::Color32::from_rgb(0xff, 0x8a, 0x80), e);
                }
            }
        });
        if !open {
            self.update_window = false;
        }
        if let Some(r) = install {
            self.install_update(r);
        }
        if restart {
            // Save state, start the new version, and close this one (hot exit keeps unsaved work).
            self.save_session();
            session::spawn_window(&[]);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn clipboard_text() -> Option<String> {
        arboard::Clipboard::new().ok()?.get_text().ok()
    }

    fn apply_visuals(&self, ctx: &egui::Context) {
        let theme = self.syn.theme(&self.settings.color_scheme);
        let bg = theme.settings.background.map(editor::c).unwrap_or(egui::Color32::from_gray(30));
        let dark = (bg.r() as u32 + bg.g() as u32 + bg.b() as u32) < 384;
        let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        let shade = |d: i16| {
            let f = |x: u8| (x as i16 + if dark { d } else { -d }).clamp(0, 255) as u8;
            egui::Color32::from_rgb(f(bg.r()), f(bg.g()), f(bg.b()))
        };
        v.panel_fill = shade(-8);
        v.window_fill = shade(10);
        v.extreme_bg_color = shade(-4);
        v.widgets.noninteractive.bg_fill = shade(-8);
        let accent = egui::Color32::from_rgb(0x4f, 0x9c, 0xff);
        let r = egui::CornerRadius::same(6);
        v.window_corner_radius = egui::CornerRadius::same(10);
        v.menu_corner_radius = egui::CornerRadius::same(8);
        v.window_shadow = egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: egui::Color32::from_black_alpha(110) };
        v.popup_shadow = v.window_shadow;
        v.window_stroke = egui::Stroke::new(1.0_f32, shade(28));
        v.selection.bg_fill = accent.gamma_multiply(0.45);
        v.selection.stroke = egui::Stroke::new(1.0_f32, accent);
        v.hyperlink_color = accent;
        for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
            w.corner_radius = r;
        }
        v.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
        v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        v.widgets.hovered.weak_bg_fill = shade(14);
        v.widgets.hovered.bg_stroke = egui::Stroke::NONE;
        v.widgets.active.weak_bg_fill = shade(20);
        ctx.set_visuals(v);
        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 4.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.menu_margin = egui::Margin::same(8);
        style.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(13.5));
        style.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(13.5));
        style.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(11.0));
        style.text_styles.insert(egui::TextStyle::Monospace, egui::FontId::monospace(self.settings.font_size));
        ctx.set_style(style);
    }

    pub fn set_theme(&mut self, ctx: &egui::Context, name: &str) {
        self.settings.color_scheme = name.to_string();
        self.settings.save();
        self.apply_visuals(ctx);
    }

    /// Run a named command (shared by keymap and command palette).
    pub fn run_command(&mut self, ctx: &egui::Context, cmd: &str) {
        self.sync_groups();
        let comment = Syntaxes::comment_prefix(&self.docs[self.active].hl.syntax_name);
        let d = &mut self.docs[self.active];
        d.reveal = true;
        let b = &mut d.buf;
        match cmd {
            "new_file" => self.new_file(),
            "open_file" => {
                let dir = self.root.clone();
                self.run_dialog(move || {
                    let mut d = rfd::FileDialog::new();
                    if let Some(dir) = dir {
                        d = d.set_directory(dir);
                    }
                    d.pick_files().map_or(DialogResult::Cancelled, DialogResult::Files)
                });
            }
            "open_folder" => {
                self.run_dialog(|| rfd::FileDialog::new().pick_folder().map_or(DialogResult::Cancelled, DialogResult::Folder));
            }
            "save" => self.save(false),
            "save_as" => self.save(true),
            "save_all" => {
                let a = self.active;
                for i in 0..self.docs.len() {
                    if self.docs[i].buf.is_dirty() && self.docs[i].buf.path.is_some() {
                        self.active = i;
                        self.save(false);
                    }
                }
                self.active = a;
            }
            "close_file" => self.close(self.active, false),
            "next_view" | "prev_view" => {
                // Cycle within the focused group, like Sublime.
                let tabs: Vec<usize> = (0..self.docs.len()).filter(|&i| self.docs[i].group == self.focus).collect();
                if let Some(p) = tabs.iter().position(|&i| i == self.active) {
                    let n = tabs.len();
                    self.active = tabs[if cmd == "next_view" { (p + 1) % n } else { (p + n - 1) % n }];
                }
            }
            "fold" => b.fold(),
            "unfold" => b.unfold(),
            "unfold_all" => b.unfold_all(),
            c if c.starts_with("fold_level_") => b.fold_level(c["fold_level_".len()..].parse().unwrap_or(1)),
            "toggle_bookmark" => b.toggle_bookmark(),
            "next_bookmark" => b.goto_bookmark(false),
            "prev_bookmark" => b.goto_bookmark(true),
            "clear_bookmarks" => b.clear_bookmarks(),
            "select_all_bookmarks" => b.select_bookmarks(),
            "auto_complete" => editor::trigger_completion(d),
            "copy" | "cut" => {
                if b.sels.iter().all(|s| s.is_empty()) {
                    let saved = b.sels.clone();
                    b.expand_to_line();
                    ctx.copy_text(b.selected_texts().concat());
                    if cmd == "cut" {
                        b.delete_lines();
                    } else {
                        b.sels = saved;
                    }
                } else {
                    ctx.copy_text(b.selected_texts().join("\n"));
                    if cmd == "cut" {
                        b.insert("");
                    }
                }
            }
            "paste" => {
                if let Some(t) = Self::clipboard_text() {
                    b.insert(&t.replace("\r\n", "\n"));
                }
            }
            "new_window" => session::spawn_window(&["--new-window".as_ref()]),
            "check_for_updates" => self.check_updates(true),
            "close_window" => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            "set_layout_1" => self.set_layout(Layout::Single),
            "set_layout_2" => self.set_layout(Layout::Columns(2)),
            "set_layout_3" => self.set_layout(Layout::Columns(3)),
            "set_layout_4" => self.set_layout(Layout::Columns(4)),
            "set_layout_rows_2" => self.set_layout(Layout::Rows(2)),
            "set_layout_rows_3" => self.set_layout(Layout::Rows(3)),
            "set_layout_grid" => self.set_layout(Layout::Grid),
            c if c.starts_with("focus_group_") => self.focus_group(c["focus_group_".len()..].parse::<usize>().unwrap_or(1) - 1),
            c if c.starts_with("move_to_group_") => self.move_to_group(c["move_to_group_".len()..].parse::<usize>().unwrap_or(1) - 1),
            "reopen_last_file" => {
                if let Some(p) = self.closed.pop() {
                    self.open_file(&p);
                }
            }
            c if c.starts_with("select_by_index_") => {
                let tabs: Vec<usize> = (0..self.docs.len()).filter(|&i| self.docs[i].group == self.focus).collect();
                let n: usize = c["select_by_index_".len()..].parse().unwrap_or(0);
                let n = if n == 9 { tabs.len() - 1 } else { n };
                if let Some(&i) = tabs.get(n) {
                    self.active = i;
                }
            }
            "soft_undo" => {
                b.undo();
            }
            "soft_redo" => {
                b.redo();
            }
            "find_under_expand_skip" => b.skip_occurrence(),
            "expand_selection_to_brackets" => b.expand_to_brackets(),
            "move_to_bracket" => b.jump_to_bracket(),
            "join_lines" => b.join_lines(),
            "insert_line_after" => b.insert_line(false),
            "insert_line_before" => b.insert_line(true),
            "delete_to_eol" => b.delete_to_line_edge(true),
            "delete_to_bol" => b.delete_to_line_edge(false),
            "sort_lines" => b.sort_lines(false),
            "sort_lines_case_sensitive" => b.sort_lines(true),
            "slurp_find_string" | "find_under" | "find_under_prev" => {
                if b.sels.last().unwrap().is_empty() {
                    let (a, e) = b.word_at(b.sels.last().unwrap().head);
                    b.sels = vec![sublimers_core::Selection::new(a, e)];
                }
                let text = b.selected_texts().pop().unwrap_or_default();
                if !text.is_empty() {
                    self.find.query = Query { text, ..Default::default() };
                    if cmd != "slurp_find_string" {
                        let q = self.find.query.clone();
                        b.find_next(&q, cmd == "find_under_prev");
                    }
                }
            }
            "goto_word" => self.palette.show(PaletteMode::Files, "#"),
            "toggle_full_screen" => {
                self.fullscreen = !self.fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            }
            "toggle_distraction_free" => {
                self.fullscreen = !self.fullscreen;
                self.show_sidebar = !self.fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            }
            "undo" => {
                b.undo();
            }
            "redo" => {
                b.redo();
            }
            "select_all" => b.select_all(),
            "expand_selection_to_line" => b.expand_to_line(),
            "find_under_expand" => b.select_next_occurrence(),
            "find_all_under" => b.select_all_occurrences(),
            "split_selection_into_lines" => b.split_into_lines(),
            "add_cursor_above" => b.add_cursor_vertical(-1),
            "add_cursor_below" => b.add_cursor_vertical(1),
            "single_selection" => {
                if d.completion.is_some() {
                    d.completion = None;
                } else if self.find.open || self.palette.open {
                    self.find.open = false;
                    self.palette.open = false;
                } else {
                    b.single_selection();
                }
            }
            "duplicate_line" => b.duplicate_lines(),
            "delete_line" => b.delete_lines(),
            "swap_line_up" => b.swap_lines(true),
            "swap_line_down" => b.swap_lines(false),
            "indent" => b.indent(),
            "unindent" => b.unindent(),
            "toggle_comment" => b.toggle_comment(comment),
            "upper_case" | "lower_case" => {
                let texts: Vec<String> = b
                    .selected_texts()
                    .into_iter()
                    .map(|t| if cmd == "upper_case" { t.to_uppercase() } else { t.to_lowercase() })
                    .collect();
                let sels = b.sorted_sels();
                b.sels = sels.clone();
                b.insert_per_selection(&texts);
                b.sels = sels;
            }
            "show_overlay_goto" => self.palette.show(PaletteMode::Files, ""),
            "show_overlay_command" => self.palette.show(PaletteMode::Commands, ""),
            "goto_symbol" => self.palette.show(PaletteMode::Files, "@"),
            "goto_line" => self.palette.show(PaletteMode::Files, ":"),
            "select_theme" => self.palette.show(PaletteMode::Themes, ""),
            "set_syntax" => self.palette.show(PaletteMode::Syntaxes, ""),
            "show_find" | "show_replace" | "show_find_in_files" => {
                if !b.sels[0].is_empty() {
                    self.find.query.text = b.selected_texts()[0].clone();
                }
                self.find.open = true;
                self.find.replace = cmd != "show_find";
                self.find.in_files = cmd == "show_find_in_files";
                self.find.focus = true;
            }
            "find_next" | "find_prev" => {
                let q = self.find.query.clone();
                if !b.find_next(&q, cmd == "find_prev") {
                    self.status = "No matches".into();
                }
            }
            "toggle_side_bar" => self.show_sidebar = !self.show_sidebar,
            "increase_font_size" | "decrease_font_size" => {
                self.settings.font_size = (self.settings.font_size + if cmd == "increase_font_size" { 1.0 } else { -1.0 }).clamp(6.0, 48.0);
                self.settings.save();
                self.apply_visuals(ctx);
            }
            "open_settings" => {
                let p = settings::settings_path();
                self.settings.save();
                self.open_file(&p);
            }
            "open_keybindings" => {
                let p = settings::keymap_path();
                settings::ensure_keymap();
                self.open_file(&p);
            }
            "reload_settings" => {
                self.settings = Settings::load();
                for d in &mut self.docs {
                    self.settings.apply_to(&mut d.buf);
                }
                self.apply_visuals(ctx);
            }
            "toggle_word_wrap" => {
                self.settings.word_wrap = !self.settings.word_wrap;
                self.settings.save();
            }
            "toggle_minimap" => {
                self.settings.minimap = !self.settings.minimap;
            }
            "exit" => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            other => self.status = format!("Unknown command: {other}"),
        }
        self.sync_groups();
    }

    fn tab_strip(&mut self, pui: &mut egui::Ui, g: usize) {
        let fill = pui.visuals().panel_fill;
        let r = pui.available_rect_before_wrap();
        let strip = egui::Rect::from_min_size(r.min, egui::vec2(r.width(), 36.0));
        pui.painter().rect_filled(strip, 0.0, fill);
        pui.advance_cursor_after_rect(strip);
        let mut sui = pui.new_child(egui::UiBuilder::new().max_rect(strip.shrink2(egui::vec2(6.0, 0.0)).translate(egui::vec2(0.0, 3.0))));
        let ui = &mut sui;
            egui::ScrollArea::horizontal().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let mut close = None;
                let mut switch = None;
                let mut tab_cmd: Option<(usize, &str)> = None;
                let mut keep = None;
                let editor_bg = self.syn.theme(&self.settings.color_scheme).settings.background.map(editor::c).unwrap_or(egui::Color32::from_gray(30));
                for (i, d) in self.docs.iter().enumerate().filter(|(_, d)| d.group == g) {
                    let active = Some(i) == self.group_active.get(g).copied().flatten();
                    let name = d.buf.name();
                    let font = egui::FontId::proportional(13.0);
                    let tw = ui.fonts(|f| f.layout_no_wrap(name.clone(), font.clone(), egui::Color32::WHITE).size().x);
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(tw + 46.0 + if d.buf.is_dirty() { 14.0 } else { 0.0 }, 30.0), egui::Sense::click());
                    let vis = ui.visuals();
                    let fill = if active { editor_bg } else if resp.hovered() { vis.widgets.hovered.weak_bg_fill } else { egui::Color32::TRANSPARENT };
                    ui.painter().rect_filled(rect, egui::CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, fill);
                    if active {
                        ui.painter().line_segment([rect.left_top() + egui::vec2(8.0, 0.5), rect.right_top() + egui::vec2(-8.0, 0.5)], egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(0x4f, 0x9c, 0xff)));
                    }
                    let tc = if active { vis.strong_text_color() } else { vis.weak_text_color() };
                    let mut tx = rect.left() + 14.0;
                    if d.buf.is_dirty() {
                        ui.painter().circle_filled(egui::pos2(tx + 3.0, rect.center().y), 3.5, egui::Color32::from_rgb(0xff, 0x9a, 0x3d));
                        tx += 14.0;
                    }
                    if d.preview {
                        // Preview tabs are italic, like Sublime's transient tabs.
                        let mut job = egui::text::LayoutJob::default();
                        job.append(&name, 0.0, egui::TextFormat { font_id: font.clone(), color: tc, italics: true, ..Default::default() });
                        let g = ui.fonts(|f| f.layout_job(job));
                        ui.painter().galley(egui::pos2(tx, rect.center().y - g.size().y / 2.0), g, tc);
                    } else {
                        ui.painter().text(egui::pos2(tx, rect.center().y), egui::Align2::LEFT_CENTER, &name, font, tc);
                    }
                    if resp.double_clicked() {
                        keep = Some(i);
                    }
                    let xr = egui::Rect::from_center_size(rect.right_center() - egui::vec2(16.0, 0.0), egui::vec2(18.0, 18.0));
                    let xresp = ui.interact(xr, egui::Id::new(("tabx", i)), egui::Sense::click());
                    if xresp.hovered() {
                        ui.painter().rect_filled(xr, 4.0, vis.widgets.active.weak_bg_fill);
                    }
                    // The close button is always visible; unsaved tabs get a dot before the name.
                    let xc = if xresp.hovered() { vis.strong_text_color() } else { tc };
                    ui.painter().text(xr.center(), egui::Align2::CENTER_CENTER, "×", egui::FontId::proportional(15.0), xc);
                    if xresp.clicked() || resp.middle_clicked() {
                        close = Some(i);
                    } else if resp.clicked() {
                        switch = Some(i);
                    }
                    resp.context_menu(|ui| {
                        for (label, cmd) in [("Close", "close"), ("Close Others", "close_others"), ("Close Tabs to the Right", "close_right"), ("Copy File Path", "copy_path"), ("Reveal in Side Bar", "reveal_sidebar")] {
                            if ui.button(label).clicked() {
                                tab_cmd = Some((i, cmd));
                                ui.close_menu();
                            }
                        }
                    });
                }
                if let Some(i) = switch {
                    self.active = i;
                    self.sync_groups();
                }
                if let Some(i) = keep {
                    self.docs[i].preview = false;
                }
                if let Some(i) = close {
                    self.close(i, false);
                }
                if let Some((i, cmd)) = tab_cmd {
                    self.tab_command(ui.ctx(), i, cmd);
                }
            });
        });
    }

    fn tab_command(&mut self, ctx: &egui::Context, i: usize, cmd: &str) {
        let g = self.docs[i].group;
        match cmd {
            "close" => self.close(i, false),
            "close_others" | "close_right" => {
                // Close from the end so indices stay valid; unsaved tabs prompt one at a time.
                for j in (0..self.docs.len()).rev() {
                    let target = self.docs[j].group == g && j != i && (cmd == "close_others" || j > i);
                    if target && !self.docs[j].buf.is_dirty() {
                        self.close(j, true);
                    }
                }
            }
            "copy_path" => {
                if let Some(p) = &self.docs[i].buf.path {
                    ctx.copy_text(p.display().to_string());
                }
            }
            "reveal_sidebar" => self.show_sidebar = true,
            _ => {}
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("OPEN FILES").small().weak());
        let mut switch = None;
        let mut close = None;
        for (i, d) in self.docs.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.small_button("×").clicked() {
                    close = Some(i);
                }
                let r = ui.selectable_label(i == self.active, d.buf.name());
                if r.clicked() {
                    switch = Some(i);
                }
                if d.buf.is_dirty() {
                    // Painted, since the UI font has no "●" glyph.
                    ui.painter().circle_filled(egui::pos2(r.rect.right() + 8.0, r.rect.center().y), 3.5, egui::Color32::from_rgb(0xff, 0x9a, 0x3d));
                }
            });
        }
        if let Some(i) = switch {
            self.active = i;
        }
        if let Some(i) = close {
            self.close(i, false);
        }
        let Some(root) = self.root.clone() else { return };
        ui.add_space(8.0);
        ui.label(egui::RichText::new("FOLDERS").small().weak());
        let root_resp = ui.label(egui::RichText::new(root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).strong());
        let mut act = None;
        tree_menu(&root_resp, &root, true, &mut act);
        let current = self.docs[self.active].buf.path.clone();
        egui::ScrollArea::vertical().show(ui, |ui| {
            tree(ui, &root, &mut act, 0, current.as_deref(), &self.git_status);
        });
        match act {
            Some(TreeAction::Preview(p)) => self.open_file_ext(&p, true),
            Some(TreeAction::Open(p)) => self.open_file_ext(&p, false),
            Some(TreeAction::NewFile(d)) => self.file_dialog = Some(FileDialog::NewFile(d, String::new())),
            Some(TreeAction::NewFolder(d)) => self.file_dialog = Some(FileDialog::NewFolder(d, String::new())),
            Some(TreeAction::Rename(p)) => {
                let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.file_dialog = Some(FileDialog::Rename(p, n));
            }
            Some(TreeAction::Delete(p)) => self.file_dialog = Some(FileDialog::Delete(p)),
            Some(TreeAction::CopyPath(p)) => ui.ctx().copy_text(p.display().to_string()),
            Some(TreeAction::OpenContaining(d)) => {
                let _ = std::process::Command::new("xdg-open").arg(d).spawn();
            }
            None => {}
        }
    }

    fn find_panel(&mut self, ui: &mut egui::Ui) {
        let mut action = None;
        ui.horizontal(|ui| {
            let q = &mut self.find.query;
            ui.toggle_value(&mut q.regex, ".*").on_hover_text("Regular expression");
            ui.toggle_value(&mut q.case_sensitive, "Aa").on_hover_text("Case sensitive");
            ui.toggle_value(&mut q.whole_word, "\"\"").on_hover_text("Whole word");
            let r = ui.add(egui::TextEdit::singleline(&mut q.text).hint_text("Find").desired_width(320.0));
            if self.find.focus {
                r.request_focus();
                self.find.focus = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                action = Some(if self.find.in_files { "in_files" } else if ui.input(|i| i.modifiers.shift) { "prev" } else { "next" });
                r.request_focus();
            }
            if self.find.in_files {
                if ui.button("Find").clicked() {
                    action = Some("in_files");
                }
            } else {
                if ui.button("Find").clicked() {
                    action = Some("next");
                }
                if ui.button("Find Prev").clicked() {
                    action = Some("prev");
                }
                if ui.button("Find All").clicked() {
                    action = Some("all");
                }
            }
            ui.label(&self.find.status);
            if ui.button("✖").clicked() {
                self.find.open = false;
            }
        });
        if self.find.replace && !self.find.in_files {
            ui.horizontal(|ui| {
                ui.add_space(90.0);
                ui.add(egui::TextEdit::singleline(&mut self.find.replacement).hint_text("Replace").desired_width(320.0));
                if ui.button("Replace").clicked() {
                    action = Some("replace");
                }
                if ui.button("Replace All").clicked() {
                    action = Some("replace_all");
                }
            });
        }
        let q = self.find.query.clone();
        let count = |b: &Buffer| b.find_all(&q).len();
        let buf = &mut self.docs[self.active].buf;
        match action {
            Some("next") | Some("prev") => {
                let ok = buf.find_next(&q, action == Some("prev"));
                self.find.status = if ok { format!("{} matches", count(buf)) } else { "No matches".into() };
                self.docs[self.active].reveal = true;
            }
            Some("all") => {
                buf.find_all_select(&q);
                self.find.status = format!("{} selected", buf.sels.len());
                self.find.open = false;
            }
            Some("replace") => {
                buf.replace_next(&q, &self.find.replacement);
                self.docs[self.active].reveal = true;
            }
            Some("replace_all") => {
                let n = buf.replace_all(&q, &self.find.replacement);
                self.find.status = format!("Replaced {n}");
            }
            Some("in_files") => self.find_in_files(),
            _ => {}
        }
        if self.find.in_files && !self.find.results.is_empty() {
            let mut open = None;
            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                for (p, line, text) in &self.find.results {
                    let rel = self.root.as_ref().and_then(|r| p.strip_prefix(r).ok()).unwrap_or(p);
                    if ui.link(format!("{}:{}  {}", rel.display(), line + 1, text.trim())).clicked() {
                        open = Some((p.clone(), *line));
                    }
                }
            });
            if let Some((p, line)) = open {
                self.open_file(&p);
                let d = self.doc();
                let pos = d.buf.line_start(line);
                d.buf.set_caret(pos);
                d.reveal = true;
            }
        }
    }

    fn find_in_files(&mut self) {
        self.find.results.clear();
        let Some(re) = self.find.query.regex() else { return };
        for p in &self.files {
            let Ok(text) = std::fs::read_to_string(p) else { continue };
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    self.find.results.push((p.clone(), i, line.chars().take(200).collect()));
                    if self.find.results.len() >= 5000 {
                        break;
                    }
                }
            }
        }
        self.find.status = format!("{} results", self.find.results.len());
    }
}

/// What a click in the folder tree asked for.
enum TreeAction {
    Preview(PathBuf),
    Open(PathBuf),
    NewFile(PathBuf),
    NewFolder(PathBuf),
    Rename(PathBuf),
    Delete(PathBuf),
    CopyPath(PathBuf),
    OpenContaining(PathBuf),
}

fn tree_menu(r: &egui::Response, p: &Path, is_dir: bool, act: &mut Option<TreeAction>) {
    r.context_menu(|ui| {
        ui.spacing_mut().button_padding = egui::vec2(14.0, 6.0);
        let dir = if is_dir { p.to_path_buf() } else { p.parent().map(Path::to_path_buf).unwrap_or_default() };
        let mut item = |label: &str, a: TreeAction| {
            if ui.add(egui::Button::new(label).min_size(egui::vec2(200.0, 0.0))).clicked() {
                *act = Some(a);
                ui.close_menu();
            }
        };
        if !is_dir {
            item("Open", TreeAction::Open(p.to_path_buf()));
        }
        item("New File…", TreeAction::NewFile(dir.clone()));
        item("New Folder…", TreeAction::NewFolder(dir.clone()));
        item("Rename…", TreeAction::Rename(p.to_path_buf()));
        item("Delete", TreeAction::Delete(p.to_path_buf()));
        item("Copy Path", TreeAction::CopyPath(p.to_path_buf()));
        item("Open Containing Folder", TreeAction::OpenContaining(dir));
    });
}

fn git_color(st: git::FileStatus) -> egui::Color32 {
    match st {
        git::FileStatus::Modified => egui::Color32::from_rgb(0xe5, 0xa5, 0x4b),
        git::FileStatus::Added | git::FileStatus::Untracked => egui::Color32::from_rgb(0x8c, 0xc2, 0x65),
        git::FileStatus::Deleted | git::FileStatus::Conflict => egui::Color32::from_rgb(0xe0, 0x6c, 0x75),
    }
}

fn tree(ui: &mut egui::Ui, dir: &Path, act: &mut Option<TreeAction>, depth: usize, current: Option<&Path>, gs: &std::collections::HashMap<PathBuf, git::FileStatus>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.') || n == "target" || n == "node_modules")).collect();
    entries.sort_by_key(|p| (!p.is_dir(), p.file_name().map(|n| n.to_ascii_lowercase())));
    for p in entries {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if p.is_dir() {
            // Folders containing changes are tinted like Sublime's sidebar.
            let changed = gs.keys().any(|k| k.starts_with(&p));
            let title = if changed { egui::RichText::new(name).color(git_color(git::FileStatus::Modified)) } else { egui::RichText::new(name) };
            let r = egui::CollapsingHeader::new(title).id_salt(&p).default_open(false).show(ui, |ui| tree(ui, &p, act, depth + 1, current, gs));
            tree_menu(&r.header_response, &p, true, act);
        } else {
            let label = match gs.get(&p) {
                Some(st) => egui::RichText::new(name).color(git_color(*st)),
                None => egui::RichText::new(name),
            };
            let r = ui.selectable_label(current == Some(p.as_path()), label);
            if r.double_clicked() {
                *act = Some(TreeAction::Open(p.clone()));
            } else if r.clicked() {
                *act = Some(TreeAction::Preview(p.clone()));
            }
            tree_menu(&r, &p, false, act);
        }
    }
}

fn trim_trailing(b: &mut Buffer) {
    let mut changes = Vec::new();
    for l in 0..b.rope.len_lines() {
        let t = b.line_text(l);
        let n = t.chars().count() - t.trim_end().chars().count();
        if n > 0 {
            let e = b.line_end(l);
            changes.push(sublimers_core::Change { start: e - n, end: e, text: String::new() });
        }
    }
    let sels = b.sels.clone();
    b.transact(changes, sels, None);
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.cumulative_pass_nr() < 2 {
            trace("frame");
        }
        if let Some(files) = self.files_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.files = files;
            self.files_rx = None;
            trace("file index ready");
        }
        let closing = ctx.input(|i| i.viewport().close_requested());
        if closing && !self.primary && !self.docs.iter().any(|d| d.buf.is_dirty()) {
            // A closed extra window with nothing unsaved is forgotten, like in Sublime.
            session::remove(&self.session_file);
        } else if closing || self.last_session_save.elapsed().as_secs() >= 5 {
            self.save_session();
            self.last_session_save = std::time::Instant::now();
        }
        if self.last_disk_check.elapsed().as_millis() >= 1000 {
            self.last_disk_check = std::time::Instant::now();
            self.check_disk();
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(1000));
        for d in &mut self.docs {
            if d.preview && d.buf.is_dirty() {
                d.preview = false;
            }
        }
        // Global keybindings (only when no text field other than the editor has focus).
        let typing_elsewhere = ctx.wants_keyboard_input() && !editor::editor_has_focus(ctx);
        let cmds = settings::match_keys(ctx, &self.settings.keymap, typing_elsewhere);
        for c in cmds {
            self.run_command(ctx, &c);
        }

        // Title
        let d = &self.docs[self.active];
        let title = format!("{}{} - Sublimers", d.buf.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| d.buf.name()), if d.buf.is_dirty() { " •" } else { "" });
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        egui::TopBottomPanel::top("menu").frame(egui::Frame::new().fill(ctx.style().visuals.panel_fill).inner_margin(egui::Margin::symmetric(8, 4))).show_separator_line(false).show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                let mut cmd = None;
                for (menu, items) in palette::MENUS {
                    ui.menu_button(*menu, |ui| {
                        ui.spacing_mut().button_padding = egui::vec2(14.0, 8.0);
                        ui.spacing_mut().item_spacing.y = 3.0;
                        for (label, c) in *items {
                            let keys = settings::shortcut_label(&self.settings.keymap, c);
                            if ui.add(egui::Button::new(*label).shortcut_text(keys).min_size(egui::vec2(260.0, 0.0))).clicked() {
                                cmd = Some(*c);
                                ui.close_menu();
                            }
                        }
                    });
                }
                if let Some(c) = cmd {
                    self.run_command(ctx, c);
                }
            });
        });

        egui::TopBottomPanel::bottom("status").frame(egui::Frame::new().fill(ctx.style().visuals.panel_fill).inner_margin(egui::Margin::symmetric(10, 4))).show(ctx, |ui| {
            ui.horizontal(|ui| {
                let d = &self.docs[self.active];
                let s = d.buf.sels.last().unwrap();
                let (l, c) = (d.buf.line_of(s.head), d.buf.col_of(s.head));
                let mut info = format!("Line {}, Column {}", l + 1, c + 1);
                if d.buf.sels.len() > 1 {
                    info = format!("{} selection regions  |  {info}", d.buf.sels.len());
                }
                // Copy what the status bar shows so the closures below don't borrow `self.docs`.
                let syntax = d.hl.syntax_name.clone();
                let indent = if d.buf.translate_tabs { format!("Spaces: {}", d.buf.tab_size) } else { format!("Tab Size: {}", d.buf.tab_size) };
                let (cur_le, cur_enc) = (d.buf.line_ending, d.buf.encoding.clone());
                ui.label(info);
                ui.label(&self.status);
                if let UpdateState::Available(r) = &self.update {
                    if ui.link(format!("⬆ Update {} available", r.version)).clicked() {
                        self.update_window = true;
                    }
                }
                let mut set_le = None;
                let mut enc_action: Option<(bool, &str)> = None;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(&syntax).clicked() {
                        self.palette.show(PaletteMode::Syntaxes, "");
                    }
                    ui.label(indent);
                    ui.menu_button(cur_le.name(), |ui| {
                        for le in sublimers_core::LineEnding::ALL {
                            if ui.radio(cur_le == le, le.name()).clicked() {
                                set_le = Some(le);
                                ui.close_menu();
                            }
                        }
                    });
                    ui.menu_button(&cur_enc, |ui| {
                        ui.menu_button("Reopen with Encoding", |ui| {
                            for e in sublimers_core::encoding::ENCODINGS {
                                if ui.button(*e).clicked() {
                                    enc_action = Some((true, e));
                                    ui.close_menu();
                                }
                            }
                        });
                        ui.menu_button("Save with Encoding", |ui| {
                            for e in sublimers_core::encoding::ENCODINGS {
                                if ui.radio(cur_enc == *e, *e).clicked() {
                                    enc_action = Some((false, e));
                                    ui.close_menu();
                                }
                            }
                        });
                    });
                    if ui.button(format!("🎨 {}", self.settings.color_scheme)).clicked() {
                        self.palette.show(PaletteMode::Themes, "");
                    }
                });
                let b = &mut self.docs[self.active].buf;
                if let Some(le) = set_le {
                    b.set_line_ending(le);
                }
                match enc_action {
                    Some((true, e)) if b.path.is_some() => {
                        if let Err(err) = b.reload(Some(e)) {
                            self.status = format!("Reopen failed: {err}");
                        }
                    }
                    Some((false, e)) => b.set_encoding(e),
                    _ => {}
                }
            });
        });

        if self.find.open {
            egui::TopBottomPanel::bottom("find").frame(egui::Frame::new().fill(ctx.style().visuals.panel_fill).inner_margin(egui::Margin::symmetric(12, 8))).show(ctx, |ui| self.find_panel(ui));
        }

        if self.show_sidebar {
            egui::SidePanel::left("sidebar").default_width(230.0).frame(egui::Frame::new().fill(ctx.style().visuals.panel_fill).inner_margin(egui::Margin::symmetric(10, 10))).show(ctx, |ui| self.sidebar(ui));
        }

        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
            let theme = self.syn.theme(&self.settings.color_scheme).clone();
            let rects = self.layout.rects(ui.available_rect_before_wrap());
            let sep = ctx.style().visuals.panel_fill;
            for (g, r) in rects.into_iter().enumerate() {
                let r = if self.layout.groups() > 1 { r.shrink(1.0) } else { r };
                let mut gui = ui.new_child(egui::UiBuilder::new().max_rect(r).id_salt(("group", g)));
                gui.set_clip_rect(r);
                gui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                self.tab_strip(&mut gui, g);
                let Some(di) = self.group_active.get(g).copied().flatten() else {
                    gui.painter().rect_filled(gui.available_rect_before_wrap(), 0.0, sep);
                    continue;
                };
                if self.docs[di].disk_conflict {
                    let bar = egui::Rect::from_min_size(gui.available_rect_before_wrap().min, egui::vec2(r.width(), 34.0));
                    gui.painter().rect_filled(bar, 0.0, egui::Color32::from_rgb(0x5a, 0x3a, 0x12));
                    let mut bui = gui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(egui::vec2(12.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
                    bui.label(egui::RichText::new(format!("{} changed on disk, and you have unsaved changes.", self.docs[di].buf.name())).color(egui::Color32::WHITE));
                    if bui.button("Reload").clicked() {
                        let d = &mut self.docs[di];
                        if let Err(e) = d.buf.reload(None) {
                            self.status = format!("Reload failed: {e}");
                        }
                        d.disk_conflict = false;
                    }
                    if bui.button("Keep My Changes").clicked() {
                        let d = &mut self.docs[di];
                        d.buf.ignore_disk_change();
                        d.disk_conflict = false;
                    }
                    gui.advance_cursor_after_rect(bar);
                }
                Self::update_git_marks(&mut self.docs[di]);
                let want_focus = g == self.focus && !self.palette.open && !self.find.focus;
                let vr = editor::show(&mut gui, &mut self.docs[di], editor::ViewArgs { syn: &self.syn, theme: &theme, st: &self.settings, group: g, want_focus });
                if vr.focused && self.active != di {
                    self.active = di;
                    self.sync_groups();
                }
                if g == self.focus && !vr.focused && editor::editor_has_focus(ctx) && !ctx.memory(|m| m.has_focus(editor::editor_id(g))) {
                    // Focus moved to another pane's editor via keyboard focus; follow it.
                }
                if let Some(cmd) = vr.context_menu {
                    self.active = di;
                    self.sync_groups();
                    self.run_command(ctx, cmd);
                }
            }
            if self.layout.groups() > 1 {
                // Focused pane indicator.
                let rects = self.layout.rects(ui.max_rect());
                if let Some(r) = rects.get(self.focus) {
                    ui.painter().rect_stroke(r.shrink(0.5), 0.0, egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(0x4f, 0x9c, 0xff).gamma_multiply(0.6)), egui::StrokeKind::Inside);
                }
            }
        });

        if self.palette.open {
            palette::show(ctx, self);
        }
        self.file_dialog(ctx);
        self.poll_dialog(ctx);
        self.poll_git();
        self.update_ui(ctx);

        if let Some(i) = self.pending_close {
            let mut done = false;
            egui::Window::new("Unsaved changes").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.label(format!("Save changes to {} before closing?", self.docs[i].buf.name()));
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        self.active = i;
                        self.save_doc(i, false, true);
                        done = true;
                    }
                    if ui.button("Don't Save").clicked() {
                        self.close(i, true);
                        done = true;
                    }
                    if ui.button("Cancel").clicked() {
                        done = true;
                    }
                });
            });
            if done {
                self.pending_close = None;
            }
        }

        // Drag & drop files
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        for p in dropped {
            if p.is_dir() {
                self.open_folder(&p);
            } else {
                self.open_file(&p);
            }
        }
    }
}

pub fn trace(what: &str) {
    if std::env::var_os("SUBLIMERS_TRACE").is_some() {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        eprintln!("[{:>6} ms] {what}", START.get_or_init(std::time::Instant::now).elapsed().as_millis());
    }
}

fn main() -> eframe::Result {
    trace("main");
    let mut args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let window = session::Window::from_args(&mut args);
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1200.0, 800.0]).with_title("Sublimers").with_app_id("sublimers").with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../../../packaging/sublimers-512.png")).unwrap_or_default()),
        ..Default::default()
    };
    eframe::run_native("Sublimers", opts, Box::new(|cc| Ok(Box::new(App::new(cc, args, window)))))
}


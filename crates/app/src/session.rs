//! Hot exit: the open files and their unsaved contents, restored on the next launch.
//!
//! Each window is its own process with its own session file. The main window uses
//! `Session.sublime_session`; extra windows (Ctrl+Shift+N) use `Session-<id>.sublime_session`.
//! A `.pid` file next to each session marks it as owned by a running window, so the main
//! window only reopens windows that aren't already running.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct SessionDoc {
    pub path: Option<PathBuf>,
    /// Present only when the buffer had unsaved changes (or was never saved).
    pub content: Option<String>,
    pub caret: usize,
    pub syntax: String,
    pub scroll_y: f32,
    #[serde(default)]
    pub group: usize,
    #[serde(default)]
    pub folds: Vec<(usize, usize)>,
    #[serde(default)]
    pub bookmarks: Vec<usize>,
    #[serde(default)]
    pub encoding: Option<String>,
    #[serde(default)]
    pub line_ending: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Session {
    pub docs: Vec<SessionDoc>,
    pub active: usize,
    pub root: Option<PathBuf>,
    pub show_sidebar: bool,
    #[serde(default)]
    pub layout: crate::Layout,
}

/// How this process's window starts.
pub enum Window {
    Main,
    /// Reopen a window from a previous run.
    Restore(PathBuf),
    /// A fresh empty window.
    New,
}

impl Window {
    pub fn from_args(args: &mut Vec<PathBuf>) -> Self {
        if let Some(i) = args.iter().position(|a| a.as_os_str() == "--new-window") {
            args.remove(i);
            return Window::New;
        }
        if let Some(i) = args.iter().position(|a| a.as_os_str() == "--session") {
            args.remove(i);
            if i < args.len() {
                return Window::Restore(args.remove(i));
            }
        }
        Window::Main
    }
}

pub fn main_path() -> PathBuf {
    crate::settings::config_dir().join("Session.sublime_session")
}

pub fn new_window_path() -> PathBuf {
    let id = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    crate::settings::config_dir().join(format!("Session-{id}.sublime_session"))
}

fn pid_path(session: &Path) -> PathBuf {
    session.with_extension("pid")
}

/// Mark `session` as owned by this process.
pub fn claim(session: &Path) {
    let _ = std::fs::create_dir_all(crate::settings::config_dir());
    let _ = std::fs::write(pid_path(session), std::process::id().to_string());
}

fn owned_by_live_process(session: &Path) -> bool {
    std::fs::read_to_string(pid_path(session))
        .ok()
        .and_then(|p| p.trim().parse::<u32>().ok())
        .is_some_and(|pid| pid != std::process::id() && Path::new(&format!("/proc/{pid}")).exists())
}

/// The program to launch for another window. Inside an AppImage the binary lives in a
/// temporary mount that disappears with this process, so launch the AppImage itself.
pub fn self_exe() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from).or_else(|| std::env::current_exe().ok())
}

pub fn spawn_window(args: &[&std::ffi::OsStr]) {
    if let Some(exe) = self_exe() {
        let _ = std::process::Command::new(exe).args(args).spawn();
    }
}

/// Reopen extra windows saved by a previous run that aren't open right now.
pub fn spawn_orphan_windows() {
    let Ok(rd) = std::fs::read_dir(crate::settings::config_dir()) else { return };
    for e in rd.filter_map(Result::ok) {
        let p = e.path();
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.starts_with("Session-") && name.ends_with(".sublime_session") && !owned_by_live_process(&p) {
            spawn_window(&["--session".as_ref(), p.as_os_str()]);
        }
    }
}

/// Forget a closed extra window.
pub fn remove(session: &Path) {
    let _ = std::fs::remove_file(session);
    let _ = std::fs::remove_file(pid_path(session));
}

pub fn save(path: &Path, s: &Session) {
    let _ = std::fs::create_dir_all(crate::settings::config_dir());
    let tmp = path.with_extension("tmp");
    // Write then rename so a crash mid-write never loses the previous session.
    if std::fs::write(&tmp, serde_json::to_vec(s).unwrap()).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

pub fn load(path: &Path) -> Option<Session> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

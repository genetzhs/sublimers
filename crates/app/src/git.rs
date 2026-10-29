//! Git integration: changed-line markers against HEAD, and file status for the sidebar.
//! Uses the `git` command, so it works with any repository the user can already use.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineChange {
    Added,
    Modified,
    /// Lines were deleted just above this line.
    DeletedAbove,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Untracked,
    Deleted,
    Conflict,
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then_some(out.stdout)
}

pub fn repo_root(path: &Path) -> Option<PathBuf> {
    let dir = if path.is_dir() { path } else { path.parent()? };
    let out = git(dir, &["rev-parse", "--show-toplevel"])?;
    Some(PathBuf::from(String::from_utf8_lossy(&out).trim()))
}

pub fn head_commit(root: &Path) -> Option<String> {
    git(root, &["rev-parse", "HEAD"]).map(|o| String::from_utf8_lossy(&o).trim().to_string())
}

/// The file's content at HEAD, or None when it isn't tracked (no markers then).
pub fn head_content(file: &Path) -> Option<String> {
    let root = repo_root(file)?;
    let rel = file.strip_prefix(&root).ok()?;
    let rel = rel.to_string_lossy().replace('\\', "/");
    let bytes = git(&root, &["show", &format!("HEAD:{rel}")])?;
    Some(String::from_utf8_lossy(&bytes).replace("\r\n", "\n"))
}

/// Per-line markers for `current` compared with `base` (index = line number).
pub fn line_changes(base: &str, current: &str) -> Vec<Option<LineChange>> {
    use similar::{DiffOp, TextDiff};
    let n = current.split('\n').count();
    let mut marks = vec![None; n];
    let diff = TextDiff::configure().timeout(std::time::Duration::from_millis(200)).diff_lines(base, current);
    for op in diff.ops() {
        match *op {
            DiffOp::Insert { new_index, new_len, .. } => {
                for m in marks.iter_mut().skip(new_index).take(new_len) {
                    *m = Some(LineChange::Added);
                }
            }
            DiffOp::Replace { new_index, new_len, .. } => {
                for m in marks.iter_mut().skip(new_index).take(new_len) {
                    *m = Some(LineChange::Modified);
                }
            }
            DiffOp::Delete { new_index, .. } => {
                let i = new_index.min(n.saturating_sub(1));
                if marks[i].is_none() {
                    marks[i] = Some(LineChange::DeletedAbove);
                }
            }
            DiffOp::Equal { .. } => {}
        }
    }
    marks
}

/// Status of changed files under `root` (absolute paths), from `git status`.
pub fn status(root: &Path) -> HashMap<PathBuf, FileStatus> {
    let mut map = HashMap::new();
    let Some(top) = repo_root(root) else { return map };
    let Some(out) = git(&top, &["status", "--porcelain=v1", "-z", "--untracked-files=all"]) else { return map };
    let mut entries = out.split(|b| *b == 0);
    while let Some(e) = entries.next() {
        if e.len() < 4 {
            continue;
        }
        let (x, y) = (e[0] as char, e[1] as char);
        let path = top.join(String::from_utf8_lossy(&e[3..]).as_ref());
        let st = match (x, y) {
            ('?', '?') => FileStatus::Untracked,
            ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => FileStatus::Conflict,
            ('A', _) => FileStatus::Added,
            ('D', _) | (_, 'D') => FileStatus::Deleted,
            _ => FileStatus::Modified,
        };
        if x == 'R' || x == 'C' {
            entries.next(); // the original path of a rename/copy
        }
        map.insert(path, st);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_added_modified_deleted() {
        let base = "a\nb\nc\nd\n";
        let cur = "a\nB\nc\nnew\n";
        let m = line_changes(base, cur);
        assert_eq!(m[0], None);
        assert_eq!(m[1], Some(LineChange::Modified));
        assert_eq!(m[3], Some(LineChange::Modified));
        let m = line_changes("a\nb\n", "a\nx\nb\n");
        assert_eq!(m[1], Some(LineChange::Added));
        let m = line_changes("a\nb\nc\n", "a\nc\n");
        assert_eq!(m[1], Some(LineChange::DeletedAbove));
    }

    #[test]
    fn reads_status_and_head_of_a_repo() {
        let dir = std::env::temp_dir().join(format!("sublimers-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap().status.success());
        run(&["init", "-q"]);
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "init"]);
        std::fs::write(dir.join("f.txt"), "one\n").unwrap();
        run(&["add", "f.txt"]);
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "f"]);
        std::fs::write(dir.join("f.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("new.txt"), "x").unwrap();
        let dir = dir.canonicalize().unwrap();
        assert_eq!(head_content(&dir.join("f.txt")).as_deref(), Some("one\n"));
        assert_eq!(head_content(&dir.join("new.txt")), None);
        let st = status(&dir);
        assert_eq!(st.get(&dir.join("f.txt")), Some(&FileStatus::Modified));
        assert_eq!(st.get(&dir.join("new.txt")), Some(&FileStatus::Untracked));
        std::fs::remove_dir_all(&dir).ok();
    }
}

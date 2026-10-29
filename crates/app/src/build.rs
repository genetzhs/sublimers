//! Build systems (Tools → Build, Ctrl+B): run a command for the current file or project and
//! show its output in a panel, with clickable `file:line:col` errors (F4 / Shift+F4).
//!
//! Build systems are detected from the project (Cargo.toml, package.json, Makefile, go.mod)
//! and the file's syntax, and can be added in a `.sublime-project` file under "build_systems"
//! with Sublime's keys: name, shell_cmd, working_dir, file_regex and variants.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BuildSystem {
    pub name: String,
    pub shell_cmd: String,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub file_regex: Option<String>,
    #[serde(default)]
    pub variants: Vec<BuildSystem>,
}

impl BuildSystem {
    fn new(name: &str, cmd: &str, dir: &str) -> Self {
        BuildSystem { name: name.into(), shell_cmd: cmd.into(), working_dir: Some(dir.into()), file_regex: None, variants: vec![] }
    }

    /// The system and its variants as a flat list, variants named "Parent - Variant".
    pub fn flatten(&self) -> Vec<BuildSystem> {
        let mut out = vec![BuildSystem { variants: vec![], ..self.clone() }];
        for v in &self.variants {
            let mut v = v.clone();
            v.name = format!("{} - {}", self.name, v.name);
            v.working_dir = v.working_dir.or_else(|| self.working_dir.clone());
            v.file_regex = v.file_regex.or_else(|| self.file_regex.clone());
            if v.shell_cmd.is_empty() {
                v.shell_cmd = self.shell_cmd.clone();
            }
            out.push(v);
        }
        out
    }
}

/// Sublime's build variables for a file and project folder.
pub struct Vars {
    pub file: Option<PathBuf>,
    pub folder: Option<PathBuf>,
}

impl Vars {
    pub fn expand(&self, s: &str) -> String {
        self.expand_with(s, true)
    }

    /// `quote`: shell-quote values (for commands), or insert them as-is (for paths).
    pub fn expand_with(&self, s: &str, quote: bool) -> String {
        let q = |v: &str| if quote { shell_quote(v) } else { v.to_string() };
        let f = self.file.as_deref();
        let path = |p: Option<&Path>| p.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let pairs = [
            ("$file_path", path(f.and_then(Path::parent))),
            ("$file_base_name", f.and_then(Path::file_stem).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            ("$file_extension", f.and_then(Path::extension).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            ("$file_name", f.and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            ("$file", path(f)),
            ("$project_path", path(self.folder.as_deref())),
            ("$folder", path(self.folder.as_deref())),
        ];
        let mut out = s.to_string();
        // Longest names first so "$file" doesn't eat "$file_path".
        for (k, v) in pairs {
            out = out.replace(&format!("${{{}}}", &k[1..]), &q(&v)).replace(k, &q(&v));
        }
        out
    }
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Build systems that apply to a file, project ones first.
pub fn detect(file: Option<&Path>, syntax: &str, folder: Option<&Path>, project: &[BuildSystem]) -> Vec<BuildSystem> {
    let mut out: Vec<BuildSystem> = project.iter().flat_map(BuildSystem::flatten).collect();
    let start = file.and_then(Path::parent).or(folder);
    let find_up = |marker: &str| start.and_then(|s| s.ancestors().find(|a| a.join(marker).exists()).map(Path::to_path_buf));
    if let Some(dir) = find_up("Cargo.toml") {
        let d = dir.to_string_lossy();
        for (n, c) in [("Cargo Build", "cargo build"), ("Cargo Run", "cargo run"), ("Cargo Test", "cargo test"), ("Cargo Clippy", "cargo clippy"), ("Cargo Check", "cargo check")] {
            out.push(BuildSystem::new(n, c, &d));
        }
    }
    if let Some(dir) = find_up("go.mod") {
        let d = dir.to_string_lossy();
        out.push(BuildSystem::new("Go Build", "go build ./...", &d));
        out.push(BuildSystem::new("Go Test", "go test ./...", &d));
    }
    if let Some(dir) = find_up("package.json") {
        let d = dir.to_string_lossy();
        let scripts = std::fs::read(dir.join("package.json")).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
        if let Some(obj) = scripts.as_ref().and_then(|v| v["scripts"].as_object()) {
            for k in obj.keys() {
                out.push(BuildSystem::new(&format!("npm run {k}"), &format!("npm run {k}"), &d));
            }
        }
    }
    if let Some(dir) = find_up("Makefile") {
        out.push(BuildSystem::new("Make", "make", &dir.to_string_lossy()));
    }
    if file.is_some() {
        let single: &[(&str, &str)] = match syntax {
            "Rust" => &[("Rust File", "rustc $file -o /tmp/$file_base_name && /tmp/$file_base_name")],
            "Python" => &[("Python", "python3 -u $file")],
            "Go" => &[("Go Run", "go run $file")],
            "C" => &[("C Run", "cc $file -o /tmp/$file_base_name && /tmp/$file_base_name")],
            "C++" => &[("C++ Run", "c++ $file -o /tmp/$file_base_name && /tmp/$file_base_name")],
            "JavaScript" | "JavaScript (Babel)" => &[("Node", "node $file")],
            "TypeScript" | "TypeScriptReact" => &[("TypeScript (tsx)", "npx --yes tsx $file")],
            "Bourne Again Shell (bash)" | "Shell-Unix-Generic" => &[("Bash", "bash $file")],
            "Ruby" => &[("Ruby", "ruby $file")],
            "PHP" => &[("PHP", "php $file")],
            "Perl" => &[("Perl", "perl $file")],
            "Java" => &[("Java", "java $file")],
            "Lua" => &[("Lua", "lua $file")],
            _ => &[],
        };
        for (n, c) in single {
            out.push(BuildSystem::new(n, c, "$file_path"));
        }
    }
    out
}

/// One error location found in the output.
#[derive(Clone, Debug, PartialEq)]
pub struct ErrorLoc {
    /// Output line it was found on.
    pub out_line: usize,
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
}

const DEFAULT_REGEX: &str = r#"^\s*(?:-->|at|File "|In file included from)?\s*"?((?:[A-Za-z]:)?[^\s:"(][^:"\n]*?)"?(?::|", line |\()(\d+)(?:[:,](\d+))?"#;

pub struct Run {
    pub name: String,
    pub cmd: String,
    pub dir: PathBuf,
    pub lines: Vec<String>,
    pub errors: Vec<ErrorLoc>,
    pub started: std::time::Instant,
    /// Exit code (None = killed) and elapsed seconds, once finished.
    pub finished: Option<(Option<i32>, f32)>,
    child: Option<Child>,
    rx: Receiver<String>,
    re: regex::Regex,
}

impl Run {
    pub fn start(bs: &BuildSystem, vars: &Vars) -> Result<Run, String> {
        let cmd = vars.expand(&bs.shell_cmd);
        let dir = PathBuf::from(vars.expand_with(bs.working_dir.as_deref().unwrap_or("$file_path"), false));
        let dir = if dir.as_os_str().is_empty() || !dir.is_dir() { vars.folder.clone().unwrap_or_else(|| PathBuf::from(".")) } else { dir };
        let re = regex::Regex::new(bs.file_regex.as_deref().unwrap_or(DEFAULT_REGEX)).map_err(|e| format!("Bad file_regex: {e}"))?;
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .current_dir(&dir)
            .env("CARGO_TERM_COLOR", "never")
            .env("TERM", "dumb")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("Couldn't run {cmd}: {e}"))?;
        let (tx, rx) = channel();
        for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)].into_iter().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).split(b'\n').map_while(Result::ok) {
                    if tx.send(strip_ansi(&String::from_utf8_lossy(&line))).is_err() {
                        return;
                    }
                }
            });
        }
        Ok(Run { name: bs.name.clone(), cmd, dir, lines: Vec::new(), errors: Vec::new(), started: std::time::Instant::now(), finished: None, child: Some(child), rx, re })
    }

    /// Collect new output; returns true if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(l) = self.rx.try_recv() {
            if let Some(e) = parse_error(&self.re, &l, &self.dir) {
                self.errors.push(ErrorLoc { out_line: self.lines.len(), ..e });
            }
            self.lines.push(l);
            changed = true;
        }
        if let Some(c) = self.child.as_mut() {
            if let Ok(Some(st)) = c.try_wait() {
                // Drain what the reader threads still hold before reporting.
                std::thread::sleep(std::time::Duration::from_millis(20));
                while let Ok(l) = self.rx.try_recv() {
                    if let Some(e) = parse_error(&self.re, &l, &self.dir) {
                        self.errors.push(ErrorLoc { out_line: self.lines.len(), ..e });
                    }
                    self.lines.push(l);
                }
                self.finished = Some((st.code(), self.started.elapsed().as_secs_f32()));
                self.child = None;
                changed = true;
            }
        }
        changed
    }

    pub fn running(&self) -> bool {
        self.child.is_some()
    }

    pub fn cancel(&mut self) {
        if let Some(c) = self.child.as_mut() {
            // Kill the whole process group (cargo spawns rustc, sh spawns the program...).
            let _ = Command::new("kill").arg("-TERM").arg(format!("-{}", c.id())).status();
            let _ = c.kill();
        }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.cancel();
    }
}

use std::os::unix::process::CommandExt;

pub fn parse_error(re: &regex::Regex, line: &str, dir: &Path) -> Option<ErrorLoc> {
    let c = re.captures(line)?;
    let file = c.get(1)?.as_str().trim();
    let line_no: usize = c.get(2)?.as_str().parse().ok()?;
    let col: usize = c.get(3).and_then(|m| m.as_str().parse().ok()).unwrap_or(1);
    let p = Path::new(file);
    let path = if p.is_absolute() { p.to_path_buf() } else { dir.join(p) };
    // Only real files: avoids matching things like "warning: 3 warnings".
    path.is_file().then_some(ErrorLoc { out_line: 0, path, line: line_no.max(1) - 1, col: col.max(1) - 1 })
}

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c != '\r' {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_vars() {
        let v = Vars { file: Some(PathBuf::from("/a b/c.rs")), folder: Some(PathBuf::from("/p")) };
        assert_eq!(v.expand("rustc $file -o $file_path/$file_base_name"), "rustc '/a b/c.rs' -o '/a b'/c");
        assert_eq!(v.expand("cd $folder"), "cd /p");
    }

    #[test]
    fn finds_errors() {
        let dir = std::env::temp_dir().join(format!("sublimers-build-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "").unwrap();
        let re = regex::Regex::new(DEFAULT_REGEX).unwrap();
        let e = parse_error(&re, "  --> src/main.rs:3:5", &dir).unwrap();
        assert_eq!((e.line, e.col), (2, 4));
        let e = parse_error(&re, "src/main.rs:7:2: error: x", &dir).unwrap();
        assert_eq!((e.line, e.col), (6, 1));
        let e = parse_error(&re, &format!("  File \"{}\", line 9, in <module>", dir.join("src/main.rs").display()), &dir).unwrap();
        assert_eq!(e.line, 8);
        assert!(parse_error(&re, "warning: unused variable: `x`", &dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strips_colors() {
        assert_eq!(strip_ansi("\x1b[1;31merror\x1b[0m: x\r"), "error: x");
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Runs a real `cargo build` on a broken crate; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn cargo_build_errors() {
        let dir = std::env::temp_dir().join(format!("sublimers-bp-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"bp\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        let file = dir.join("src/main.rs");
        std::fs::write(&file, "fn main() {\n    let x: u32 = \"no\";\n}\n").unwrap();
        let systems = detect(Some(&file), "Rust", None, &[]);
        assert_eq!(systems[0].name, "Cargo Build");
        let mut r = Run::start(&systems[0], &Vars { file: Some(file.clone()), folder: None }).unwrap();
        while r.finished.is_none() {
            r.poll();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_ne!(r.finished.unwrap().0, Some(0));
        assert!(r.errors.iter().any(|e| e.path.ends_with("src/main.rs") && e.line == 1), "{:?}\n{}", r.errors, r.lines.join("\n"));
    }
}

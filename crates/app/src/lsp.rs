//! Language Server Protocol client: completion, hover, go to definition, diagnostics and
//! formatting from language servers such as rust-analyzer, pylsp, gopls or clangd.
//!
//! Each server runs as a child process speaking JSON-RPC over stdio. A reader thread turns
//! its output into messages; the UI thread polls them every frame, so a slow or stuck
//! server never blocks editing.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};

use serde_json::{json, Value};

/// Which server handles which syntaxes (by syntect syntax name), and the LSP language id.
pub struct ServerSpec {
    pub syntaxes: &'static [&'static str],
    pub language_id: &'static str,
    /// Candidate commands, first one found on PATH wins.
    pub commands: &'static [&'static [&'static str]],
}

pub const SERVERS: &[ServerSpec] = &[
    ServerSpec { syntaxes: &["Rust"], language_id: "rust", commands: &[&["rust-analyzer"]] },
    ServerSpec { syntaxes: &["Python"], language_id: "python", commands: &[&["pylsp"], &["pyright-langserver", "--stdio"], &["basedpyright-langserver", "--stdio"]] },
    ServerSpec { syntaxes: &["JavaScript", "JavaScript (Babel)"], language_id: "javascript", commands: &[&["typescript-language-server", "--stdio"]] },
    ServerSpec { syntaxes: &["TypeScript", "TypeScriptReact"], language_id: "typescript", commands: &[&["typescript-language-server", "--stdio"]] },
    ServerSpec { syntaxes: &["Go"], language_id: "go", commands: &[&["gopls"]] },
    ServerSpec { syntaxes: &["C"], language_id: "c", commands: &[&["clangd"]] },
    ServerSpec { syntaxes: &["C++"], language_id: "cpp", commands: &[&["clangd"]] },
];

pub fn spec_for(syntax: &str) -> Option<&'static ServerSpec> {
    SERVERS.iter().find(|s| s.syntaxes.contains(&syntax))
}

fn on_path(cmd: &str) -> bool {
    // rustup installs a placeholder `rust-analyzer` that fails unless the component is added.
    Command::new(cmd).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

pub fn uri(path: &Path) -> String {
    let mut s = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => s.push(b as char),
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

pub fn path_from_uri(u: &str) -> Option<PathBuf> {
    let rest = u.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8_lossy(&out).into_owned()))
}

/// Position units the server counts columns in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    Utf16,
    Utf32,
    Utf8,
}

/// Char column -> server column for one line of text.
pub fn to_server_col(line: &str, col: usize, enc: Encoding) -> usize {
    match enc {
        Encoding::Utf32 => col,
        Encoding::Utf16 => line.chars().take(col).map(char::len_utf16).sum(),
        Encoding::Utf8 => line.chars().take(col).map(char::len_utf8).sum(),
    }
}

/// Server column -> char column.
pub fn from_server_col(line: &str, col: usize, enc: Encoding) -> usize {
    let mut n = 0;
    for (i, ch) in line.chars().enumerate() {
        if n >= col {
            return i;
        }
        n += match enc {
            Encoding::Utf32 => 1,
            Encoding::Utf16 => ch.len_utf16(),
            Encoding::Utf8 => ch.len_utf8(),
        };
    }
    line.chars().count()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// A diagnostic in char coordinates: (line, col) .. (line, col).
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub severity: Severity,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct CompletionItem {
    pub insert: String,
}

/// What a finished request should do; the UI matches on these.
pub enum Event {
    Completion { doc: u64, items: Vec<CompletionItem> },
    Hover { doc: u64, pos: usize, text: String },
    Definition { path: PathBuf, line: usize, col_server: usize, enc: Encoding },
    /// Raw server positions; converted against the buffer by the UI.
    Diagnostics { path: PathBuf, raw: Vec<Value>, enc: Encoding },
    Format { doc: u64, version: u64, edits: Vec<Value>, enc: Encoding },
    Status(String),
}

enum Pending {
    Initialize,
    Completion { doc: u64 },
    Hover { doc: u64, pos: usize },
    Definition,
    Format { doc: u64, version: u64 },
    Ignore,
}

pub struct Client {
    pub name: String,
    pub encoding: Encoding,
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next_id: i64,
    pending: HashMap<i64, Pending>,
    ready: bool,
    /// Messages sent before `initialized` is acknowledged.
    queued: Vec<Value>,
    /// Open documents: uri -> (LSP version, buffer version last sent).
    open: HashMap<String, (i64, u64)>,
}

impl Client {
    fn start(cmd: &[&str], root: &Path) -> std::io::Result<Self> {
        let mut child = Command::new(cmd[0]).args(&cmd[1..]).current_dir(root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                let mut len = None;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).ok().filter(|n| *n > 0).is_none() {
                        return;
                    }
                    let line = line.trim();
                    if line.is_empty() {
                        break;
                    }
                    if let Some(v) = line.strip_prefix("Content-Length:") {
                        len = v.trim().parse::<usize>().ok();
                    }
                }
                let Some(len) = len else { continue };
                let mut body = vec![0; len];
                if r.read_exact(&mut body).is_err() {
                    return;
                }
                if let Ok(v) = serde_json::from_slice::<Value>(&body) {
                    if tx.send(v).is_err() {
                        return;
                    }
                }
            }
        });
        let mut c = Client { name: cmd[0].to_string(), encoding: Encoding::Utf16, child, stdin, rx, next_id: 1, pending: HashMap::new(), ready: false, queued: Vec::new(), open: HashMap::new() };
        let params = json!({
            "processId": std::process::id(),
            "rootUri": uri(root),
            "workspaceFolders": [{"uri": uri(root), "name": root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()}],
            "capabilities": {
                "general": {"positionEncodings": ["utf-32", "utf-16"]},
                "textDocument": {
                    "synchronization": {"didSave": true},
                    "completion": {"completionItem": {"snippetSupport": false}},
                    "hover": {"contentFormat": ["plaintext", "markdown"]},
                    "definition": {},
                    "formatting": {},
                    "publishDiagnostics": {}
                },
                "workspace": {"workspaceFolders": true}
            }
        });
        c.request_raw("initialize", params, Pending::Initialize);
        Ok(c)
    }

    fn write(&mut self, msg: &Value) {
        let body = msg.to_string();
        let _ = write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body);
        let _ = self.stdin.flush();
    }

    fn send(&mut self, msg: Value) {
        if self.ready {
            self.write(&msg);
        } else {
            self.queued.push(msg);
        }
    }

    fn request_raw(&mut self, method: &str, params: Value, p: Pending) {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, p);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if method == "initialize" {
            self.write(&msg);
        } else {
            self.send(msg);
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Open or update a document (full-text sync).
    pub fn sync(&mut self, path: &Path, language_id: &str, text: impl FnOnce() -> String, buf_version: u64) {
        let u = uri(path);
        match self.open.get(&u).copied() {
            None => {
                self.open.insert(u.clone(), (1, buf_version));
                self.notify("textDocument/didOpen", json!({"textDocument": {"uri": u, "languageId": language_id, "version": 1, "text": text()}}));
            }
            Some((v, sent)) if sent != buf_version => {
                self.open.insert(u.clone(), (v + 1, buf_version));
                self.notify("textDocument/didChange", json!({"textDocument": {"uri": u, "version": v + 1}, "contentChanges": [{"text": text()}]}));
            }
            _ => {}
        }
    }

    pub fn did_save(&mut self, path: &Path) {
        let u = uri(path);
        if self.open.contains_key(&u) {
            self.notify("textDocument/didSave", json!({"textDocument": {"uri": u}}));
        }
    }

    pub fn did_close(&mut self, path: &Path) {
        let u = uri(path);
        if self.open.remove(&u).is_some() {
            self.notify("textDocument/didClose", json!({"textDocument": {"uri": u}}));
        }
    }

    fn pos_params(path: &Path, line: usize, col: usize) -> Value {
        json!({"textDocument": {"uri": uri(path)}, "position": {"line": line, "character": col}})
    }

    pub fn completion(&mut self, doc: u64, path: &Path, line: usize, col: usize) {
        self.request_raw("textDocument/completion", Self::pos_params(path, line, col), Pending::Completion { doc });
    }

    pub fn hover(&mut self, doc: u64, pos: usize, path: &Path, line: usize, col: usize) {
        self.request_raw("textDocument/hover", Self::pos_params(path, line, col), Pending::Hover { doc, pos });
    }

    pub fn definition(&mut self, path: &Path, line: usize, col: usize) {
        self.request_raw("textDocument/definition", Self::pos_params(path, line, col), Pending::Definition);
    }

    pub fn format(&mut self, doc: u64, version: u64, path: &Path, tab_size: usize, spaces: bool) {
        let p = json!({"textDocument": {"uri": uri(path)}, "options": {"tabSize": tab_size, "insertSpaces": spaces}});
        self.request_raw("textDocument/formatting", p, Pending::Format { doc, version });
    }

    /// Drain server messages into UI events.
    fn poll(&mut self, out: &mut Vec<Event>) {
        while let Ok(msg) = self.rx.try_recv() {
            if let (Some(id), Some(method)) = (msg.get("id"), msg.get("method").and_then(Value::as_str)) {
                // A request from the server: acknowledge what we can, so it doesn't wait on us.
                let result = match method {
                    "workspace/configuration" => json!(msg["params"]["items"].as_array().map(|a| vec![Value::Null; a.len()]).unwrap_or_default()),
                    _ => Value::Null,
                };
                let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
                self.write(&reply);
                continue;
            }
            if let Some(method) = msg.get("method").and_then(Value::as_str) {
                match method {
                    "textDocument/publishDiagnostics" => {
                        if let Some(p) = msg["params"]["uri"].as_str().and_then(path_from_uri) {
                            out.push(Event::Diagnostics { path: p, raw: msg["params"]["diagnostics"].as_array().cloned().unwrap_or_default(), enc: self.encoding });
                        }
                    }
                    "window/showMessage" => {
                        if let Some(m) = msg["params"]["message"].as_str() {
                            out.push(Event::Status(format!("{}: {m}", self.name)));
                        }
                    }
                    _ => {}
                }
                continue;
            }
            let Some(id) = msg.get("id").and_then(Value::as_i64) else { continue };
            let Some(p) = self.pending.remove(&id) else { continue };
            let result = &msg["result"];
            match p {
                Pending::Initialize => {
                    self.encoding = match result["capabilities"]["positionEncoding"].as_str() {
                        Some("utf-32") => Encoding::Utf32,
                        Some("utf-8") => Encoding::Utf8,
                        _ => Encoding::Utf16,
                    };
                    self.ready = true;
                    self.write(&json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}));
                    for m in std::mem::take(&mut self.queued) {
                        self.write(&m);
                    }
                    out.push(Event::Status(format!("{} ready", self.name)));
                }
                Pending::Completion { doc } => {
                    let list = result.get("items").unwrap_or(result);
                    let items = list
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .take(200)
                                .map(|it| {
                                    let label = it["label"].as_str().unwrap_or_default().trim().to_string();
                                    let insert = it["textEdit"]["newText"].as_str().or(it["insertText"].as_str()).unwrap_or(&label).to_string();
                                    CompletionItem { insert: strip_snippet(&insert) }
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    out.push(Event::Completion { doc, items });
                }
                Pending::Hover { doc, pos } => {
                    let text = hover_text(&result["contents"]);
                    if !text.trim().is_empty() {
                        out.push(Event::Hover { doc, pos, text });
                    }
                }
                Pending::Definition => {
                    let loc = if result.is_array() { result.get(0).cloned().unwrap_or(Value::Null) } else { result.clone() };
                    let u = loc["uri"].as_str().or(loc["targetUri"].as_str());
                    let range = if loc["range"].is_object() { &loc["range"] } else { &loc["targetSelectionRange"] };
                    if let Some(p) = u.and_then(path_from_uri) {
                        let line = range["start"]["line"].as_u64().unwrap_or(0) as usize;
                        let col = range["start"]["character"].as_u64().unwrap_or(0) as usize;
                        out.push(Event::Definition { path: p, line, col_server: col, enc: self.encoding });
                    } else {
                        out.push(Event::Status("No definition found".into()));
                    }
                }
                Pending::Format { doc, version } => {
                    out.push(Event::Format { doc, version, edits: result.as_array().cloned().unwrap_or_default(), enc: self.encoding });
                }
                Pending::Ignore => {}
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.request_raw("shutdown", Value::Null, Pending::Ignore);
        self.notify("exit", Value::Null);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Plain text from snippet syntax: `foo(${1:x}, $0)` -> `foo(x, )`.
pub fn strip_snippet(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '$' if chars.peek().is_some_and(|n| n.is_ascii_digit()) => {
                while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                    chars.next();
                }
            }
            '$' if chars.peek() == Some(&'{') => {
                chars.next();
                while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                    chars.next();
                }
                if chars.peek() == Some(&':') {
                    chars.next();
                }
                let mut depth = 1;
                for n in chars.by_ref() {
                    match n {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    out.push(n);
                }
            }
            c => out.push(c),
        }
    }
    out
}

fn hover_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(hover_text).collect::<Vec<_>>().join("\n\n"),
        Value::Object(o) => o.get("value").and_then(Value::as_str).unwrap_or_default().to_string(),
        _ => String::new(),
    }
    // Drop markdown code fences; the popup shows plain text.
    .lines()
    .filter(|l| !l.trim_start().starts_with("```"))
    .collect::<Vec<_>>()
    .join("\n")
    .trim()
    .to_string()
}

/// Running servers, keyed by (command, workspace root).
#[derive(Default)]
pub struct Manager {
    clients: HashMap<(String, PathBuf), Client>,
    failed: std::collections::HashSet<(String, PathBuf)>,
}

impl Manager {
    /// The client for a file's syntax, starting the server on first use.
    pub fn client_for(&mut self, syntax: &str, root: &Path) -> Option<(&mut Client, &'static str)> {
        let spec = spec_for(syntax)?;
        let cmd = spec.commands.iter().find(|c| self.clients.contains_key(&(c[0].to_string(), root.to_path_buf())))
            .or_else(|| spec.commands.iter().find(|c| !self.failed.contains(&(c[0].to_string(), root.to_path_buf())) && on_path(c[0])))?;
        let key = (cmd[0].to_string(), root.to_path_buf());
        if !self.clients.contains_key(&key) {
            match Client::start(cmd, root) {
                Ok(c) => {
                    self.clients.insert(key.clone(), c);
                }
                Err(_) => {
                    self.failed.insert(key);
                    return None;
                }
            }
        }
        self.clients.get_mut(&key).map(|c| (c, spec.language_id))
    }

    pub fn poll(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        for c in self.clients.values_mut() {
            c.poll(&mut out);
        }
        out
    }

    pub fn any_running(&self) -> bool {
        !self.clients.is_empty()
    }

    pub fn did_close(&mut self, path: &Path) {
        for c in self.clients.values_mut() {
            c.did_close(path);
        }
    }

    pub fn did_save(&mut self, path: &Path) {
        for c in self.clients.values_mut() {
            c.did_save(path);
        }
    }
}

/// Convert raw LSP diagnostics into char coordinates using the buffer's lines.
pub fn convert_diagnostics(raw: &[Value], enc: Encoding, line_text: impl Fn(usize) -> Option<String>) -> Vec<Diagnostic> {
    raw.iter()
        .filter_map(|d| {
            let r = &d["range"];
            let get = |p: &Value| -> Option<(usize, usize)> {
                let l = p["line"].as_u64()? as usize;
                let c = p["character"].as_u64()? as usize;
                let text = line_text(l)?;
                Some((l, from_server_col(&text, c, enc)))
            };
            let start = get(&r["start"])?;
            let end = get(&r["end"]).unwrap_or(start);
            let severity = match d["severity"].as_u64() {
                Some(1) => Severity::Error,
                Some(2) => Severity::Warning,
                _ => Severity::Info,
            };
            Some(Diagnostic { start, end, severity, message: d["message"].as_str().unwrap_or_default().to_string() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_columns() {
        let line = "a😀b";
        assert_eq!(to_server_col(line, 2, Encoding::Utf16), 3);
        assert_eq!(from_server_col(line, 3, Encoding::Utf16), 2);
        assert_eq!(to_server_col("αβγ", 2, Encoding::Utf8), 4);
    }

    #[test]
    fn uris_roundtrip() {
        let p = Path::new("/tmp/a b/κ.rs");
        assert_eq!(path_from_uri(&uri(p)).unwrap(), p);
    }

    #[test]
    fn strips_snippets() {
        assert_eq!(strip_snippet("foo(${1:x}, $0)"), "foo(x, )");
        assert_eq!(strip_snippet("vec![$1]"), "vec![]");
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Talks to a real rust-analyzer; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn rust_analyzer_roundtrip() {
        let dir = std::env::temp_dir().join(format!("sublimers-lsp-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        let src = "fn helper() -> u32 { 1 }\nfn main() {\n    let s = String::new();\n    helper();\n    let x: u32 = \"no\";\n    s.\n}\n";
        let file = dir.join("src/main.rs");
        std::fs::write(&file, src).unwrap();
        let mut m = Manager::default();
        let (c, lang) = m.client_for("Rust", &dir).expect("rust-analyzer on PATH");
        c.sync(&file, lang, || src.to_string(), 1);
        let (mut comp, mut hover, mut def, mut diag) = (false, false, false, false);
        let t = std::time::Instant::now();
        let mut asked = std::time::Instant::now() - std::time::Duration::from_secs(10);
        while t.elapsed().as_secs() < 90 && !(comp && hover && def && diag) {
            // Answers are empty while the server is still indexing, so keep asking.
            if asked.elapsed().as_secs() >= 3 {
                asked = std::time::Instant::now();
                let (c, _) = m.client_for("Rust", &dir).unwrap();
                c.completion(1, &file, 5, 6);
                c.hover(1, 0, &file, 3, 5);
                c.definition(&file, 3, 5);
            }
            for e in m.poll() {
                match e {
                    Event::Completion { items, .. } => comp |= items.iter().any(|i| i.insert == "push_str"),
                    Event::Hover { text, .. } => hover |= text.contains("fn helper"),
                    Event::Definition { line, .. } => def |= line == 0,
                    Event::Diagnostics { raw, .. } => diag |= !raw.is_empty(),
                    _ => {}
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(comp && hover && def && diag, "completion {comp} hover {hover} definition {def} diagnostics {diag}");
    }
}

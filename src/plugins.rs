//! Python plugins, run in a separate process, the plugin host
//! (`src/plugin_host/`), which gives plugins the `texteditor` module, and
//! for Sublime Text's plugins, `sublime` and `sublime_plugin`. The host and the editor exchange JSON
//! messages, one per line, over the host's stdin and stdout; while a plugin
//! runs, the host asks the editor for text and changes (`app::plugin_api`).

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use eframe::egui;
use serde_json::{Value, json};

/// The host's Python files, written out next to each other before it starts.
const HOST_FILES: [(&str, &str); 7] = [
    ("host.py", include_str!("plugin_host/host.py")),
    ("_host.py", include_str!("plugin_host/_host.py")),
    ("_plugins.py", include_str!("plugin_host/_plugins.py")),
    ("sublime.py", include_str!("plugin_host/sublime.py")),
    ("sublime_plugin.py", include_str!("plugin_host/sublime_plugin.py")),
    ("sublime_api.py", include_str!("plugin_host/sublime_api.py")),
    ("texteditor.py", include_str!("plugin_host/texteditor.py")),
];

pub struct PluginHost {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    next_id: u64,
    /// Plugin commands by name: "text", "window" or "application".
    pub commands: HashMap<String, String>,
    /// Events some plugin listens for, e.g. "on_post_save".
    pub events: HashSet<String>,
}

impl PluginHost {
    /// Starts the host with `python`, from files written to `dir`, and asks
    /// it to load plugins (`load` is the "load" message). Repaints `ctx`
    /// whenever the host says something.
    pub fn start(python: &Path, dir: &Path, load: Value, ctx: &egui::Context) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for (name, text) in HOST_FILES {
            std::fs::write(dir.join(name), text).map_err(|e| format!("{}: {e}", dir.join(name).display()))?;
        }
        // -I: ignore PYTHON* variables and user site-packages; -u: unbuffered.
        let mut child = Command::new(python)
            .arg("-I")
            .arg("-u")
            .arg(dir.join("host.py"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", python.display()))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let (send, messages) = mpsc::channel();

        let (out, out_ctx) = (send.clone(), ctx.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                let message = serde_json::from_str(&line).unwrap_or_else(|_| json!({"t": "log", "text": line}));
                if out.send(message).is_err() {
                    return;
                }
                out_ctx.request_repaint();
            }
            let _ = out.send(json!({"t": "exit"}));
            out_ctx.request_repaint();
        });
        // Python's own errors (say, a syntax error in the host) go to the console.
        let err_ctx = ctx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if send.send(json!({"t": "log", "text": line})).is_err() {
                    return;
                }
                err_ctx.request_repaint();
            }
        });

        let mut host = Self { child, stdin, messages, next_id: 0, commands: HashMap::new(), events: HashSet::new() };
        host.send(&load)?;
        Ok(host)
    }

    pub fn send(&mut self, message: &Value) -> Result<(), String> {
        let line = serde_json::to_string(message).map_err(|e| e.to_string())?;
        writeln!(self.stdin, "{line}").and_then(|_| self.stdin.flush()).map_err(|e| format!("the plugin host stopped: {e}"))
    }

    /// An id for a request, to match the host's "done" to it.
    pub fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn try_recv(&self) -> Option<Value> {
        self.messages.try_recv().ok()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<Value, RecvTimeoutError> {
        self.messages.recv_timeout(timeout)
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Python 3 to run plugins with: the `python_path` setting if given, else
/// the first one found in the usual places.
pub fn find_python(setting: &str) -> Option<PathBuf> {
    if !setting.is_empty() {
        let path = PathBuf::from(setting);
        return if path.components().count() > 1 { path.is_file().then_some(path) } else { on_path(setting) };
    }
    // On macOS, /usr/bin/python3 is a stub that offers to install the
    // developer tools, so the tools' own copy is checked instead.
    let known: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/opt/homebrew/bin/python3",
            "/usr/local/bin/python3",
            "/Library/Developer/CommandLineTools/usr/bin/python3",
            "/Applications/Xcode.app/Contents/Developer/usr/bin/python3",
        ]
    } else if cfg!(windows) {
        &[]
    } else {
        &["/usr/bin/python3", "/usr/local/bin/python3"]
    };
    known
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .or_else(|| if cfg!(windows) { on_path("python.exe").or_else(|| on_path("py.exe")) } else { on_path("python3") })
        .filter(|p| !(cfg!(target_os = "macos") && p == Path::new("/usr/bin/python3")))
}

/// A Python's version, like `3.12`.
pub fn python_version(python: &Path) -> Option<String> {
    let output = Command::new(python).args(["-I", "-c", "import sys; print('%d.%d' % sys.version_info[:2])"]).output().ok()?;
    let version = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !version.is_empty()).then_some(version)
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(name)).find(|p| p.is_file())
}

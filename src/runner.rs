use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::Duration;

/// Set on every agent the app starts, so the SessionStart hook does not add the lead prompt on top of the agent's own.
pub const AGENT_ENV: &str = "CLAUDE_FACTORY_AGENT";

#[derive(Clone)]
pub enum Entry {
    /// What the user typed in the app.
    User(String),
    /// A message from a teammate (`from`), or from some other session when the sender is unknown.
    Incoming { from: String, text: String },
    Assistant(String),
    Tool(String),
    Error(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Starting,
    Working,
    Idle,
    Exited,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Starting => "starting",
            Status::Working => "working",
            Status::Idle => "idle",
            Status::Exited => "stopped",
        }
    }
}

/// Turns the nested-desktop grabs on or off for every agent; mirrors the store's setting.
pub static DESKTOP_PREVIEW: AtomicBool = AtomicBool::new(true);

/// Next X display to hand out as NESTED_DISPLAY; the nested-desktop MCP defaults to :100.
static NEXT_DISPLAY: AtomicU32 = AtomicU32::new(100);

/// A small RGBA snapshot of an agent's nested desktop.
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The preview's width on screen, in world units.
pub const THUMB_WIDTH: u32 = 160;
/// Captured at this multiple of the shown size so it stays sharp when zoomed in and on HiDPI.
const THUMB_OVERSAMPLE: u32 = 2;

#[derive(Default)]
struct Shared {
    /// Between the agent's start_desktop and stop_desktop calls.
    desktop: bool,
    thumbnail: Option<Arc<Thumbnail>>,
    entries: Vec<Entry>,
    status: Option<Status>,
    /// SendMessage calls this agent made, as (to, message), until the app routes them.
    outbox: Vec<(String, String)>,
    /// Slash commands (skills included) the session accepts, as (name, description).
    commands: Vec<(String, String)>,
    /// Claude's id for this conversation, from the first event that carries it.
    session_id: Option<String>,
}

pub struct Launch<'a> {
    pub name: &'a str,
    pub cwd: &'a str,
    pub model: &'a str,
    pub permission_mode: &'a str,
    pub system_prompt: String,
    /// Shared by every agent of the team, so the lead's tasks are the ones teammates update.
    pub task_list_id: &'a str,
    /// Continue this past session (`--resume`) instead of starting a new one.
    pub resume: Option<&'a str>,
}

/// One teammate: a long-lived `claude -p` session the app talks to over stream-json.
pub struct Agent {
    child: Child,
    stdin: Option<ChildStdin>,
    shared: Arc<Mutex<Shared>>,
    /// The agent's NESTED_DISPLAY, to stop a Xephyr that outlives the agent.
    display: String,
}

impl Agent {
    pub fn start(launch: Launch, repaint: impl Fn() + Send + Clone + 'static) -> std::io::Result<Self> {
        let display = free_display();
        let mut cmd = Command::new("claude");
        cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
            .args(["--name", launch.name])
            .args(["--permission-mode", launch.permission_mode])
            // Nobody can answer a prompt in a headless session: deny instead of hanging the turn.
            .args(["--permission-prompts", "none"])
            // Teammates must receive each other's messages whatever the permission modes are.
            .args(["--settings", r#"{"crossSessionInbound":"accept"}"#])
            .args(["--append-system-prompt", &launch.system_prompt])
            .env(AGENT_ENV, "1")
            .env("NESTED_DISPLAY", &display)
            .env("CLAUDE_CODE_TASK_LIST_ID", launch.task_list_id)
            // Task tools are only on by default for some models.
            .env("CLAUDE_CODE_ENABLE_TODO_TOOLS", "1")
            .current_dir(launch.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(id) = launch.resume {
            cmd.args(["--resume", id]);
        }
        if !launch.model.trim().is_empty() {
            cmd.args(["--model", launch.model.trim()]);
        }
        // Own process group, so stopping the agent reaches its MCP servers and tool processes too.
        cmd.process_group(0);
        // SAFETY: prctl is async-signal-safe. The session dies with the app even if the app crashes.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let mut child = cmd.spawn()?;
        record(child.id(), &display);
        let shared = Arc::new(Mutex::new(Shared { status: Some(Status::Starting), ..Default::default() }));

        let stdout = child.stdout.take().expect("piped stdout");
        let (s, r) = (shared.clone(), repaint.clone());
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(event) = serde_json::from_str::<Value>(&line) {
                    apply(&mut s.lock().unwrap(), &event);
                    r();
                }
            }
            let mut s = s.lock().unwrap();
            s.status = Some(Status::Exited);
            s.desktop = false;
            r();
        });

        let weak = Arc::downgrade(&shared);
        let d = display.clone();
        thread::spawn(move || grab_loop(weak, d));

        let stderr = child.stderr.take().expect("piped stderr");
        let s = shared.clone();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    s.lock().unwrap().entries.push(Entry::Error(line));
                    repaint();
                }
            }
        });

        // `initialize` answers right away with the session's slash commands; the `init` event waits for the first turn.
        let mut stdin = child.stdin.take();
        if let Some(w) = stdin.as_mut() {
            let req = json!({ "type": "control_request", "request_id": "init", "request": { "subtype": "initialize" } });
            let _ = writeln!(w, "{req}").and_then(|_| w.flush());
        }
        Ok(Self { child, stdin, shared, display })
    }

    pub fn send(&mut self, text: &str, files: &[std::path::PathBuf]) {
        let (text, content) = crate::attach::message(text, files);
        let line = json!({ "type": "user", "message": { "role": "user", "content": content } }).to_string();
        let sent = self.stdin.as_mut().is_some_and(|w| writeln!(w, "{line}").and_then(|_| w.flush()).is_ok());
        let mut shared = self.shared.lock().unwrap();
        if sent {
            shared.entries.push(Entry::User(text));
            shared.status = Some(Status::Working);
        } else {
            shared.entries.push(Entry::Error("The session is not running; message not sent.".into()));
        }
    }

    pub fn status(&self) -> Status {
        self.shared.lock().unwrap().status.unwrap_or(Status::Starting)
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.shared.lock().unwrap().entries.clone()
    }

    /// The latest snapshot of the agent's nested desktop, while it has one and the grab works.
    pub fn thumbnail(&self) -> Option<Arc<Thumbnail>> {
        self.shared.lock().unwrap().thumbnail.clone()
    }

    pub fn session_id(&self) -> Option<String> {
        self.shared.lock().unwrap().session_id.clone()
    }

    /// The first message the agent got, typed or from a teammate, as (from, text); `from` is None for the user.
    pub fn first_message(&self) -> Option<(Option<String>, String)> {
        self.shared.lock().unwrap().entries.iter().find_map(|e| match e {
            Entry::User(t) => Some((None, t.clone())),
            Entry::Incoming { from, text } => Some((Some(from.clone()), text.clone())),
            _ => None,
        })
    }

    /// Puts a resumed session's earlier conversation above what the agent says from now on.
    pub fn preload(&self, earlier: Vec<Entry>) {
        let mut shared = self.shared.lock().unwrap();
        let later = std::mem::replace(&mut shared.entries, earlier);
        shared.entries.extend(later);
    }

    pub fn commands(&self) -> Vec<(String, String)> {
        self.shared.lock().unwrap().commands.clone()
    }

    /// Claude names a session's inbox socket after its process id, which is how a reply addresses an unnamed sender.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Takes the messages this agent sent to teammates since the last call.
    pub fn take_outbox(&self) -> Vec<(String, String)> {
        std::mem::take(&mut self.shared.lock().unwrap().outbox)
    }

    /// Shows a teammate's message in this agent's chat: the stream never echoes messages received from other sessions.
    pub fn note_incoming(&self, from: &str, text: &str) {
        self.shared.lock().unwrap().entries.push(Entry::Incoming { from: from.into(), text: text.into() });
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.stdin.take();
        // TERM the whole group now; the grace period and the KILL run off the UI thread.
        let (pid, display) = (self.child.id(), std::mem::take(&mut self.display));
        signal_group(pid, libc::SIGTERM);
        thread::spawn(move || {
            stop_group(pid, &display);
            unrecord(pid);
        });
    }
}

/// One agent the app started, in ~/.claude-factory/running.json: its process group and the leader's start time.
#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
    pgid: u32,
    start: u64,
    display: String,
}

/// Serializes read-modify-write of running.json between the UI and the stop threads.
static RUNNING_LOCK: Mutex<()> = Mutex::new(());

fn running_path() -> std::path::PathBuf {
    crate::team::data_dir().join("running.json")
}

fn edit_running(f: impl FnOnce(&mut Vec<Record>)) {
    let _guard = RUNNING_LOCK.lock().unwrap();
    let path = running_path();
    let mut records: Vec<Record> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    f(&mut records);
    let _ = std::fs::create_dir_all(crate::team::data_dir());
    let _ = std::fs::write(&path, serde_json::to_string(&records).unwrap_or_default());
}

fn record(pgid: u32, display: &str) {
    let Some(start) = start_time(pgid) else { return };
    edit_running(|r| r.push(Record { pgid, start, display: display.into() }));
}

fn unrecord(pgid: u32) {
    edit_running(|r| r.retain(|x| x.pgid != pgid));
}

/// Stops what a crashed or killed app left behind. A group is only touched if its leader still has the start
/// time recorded for it, so a reused PID is never killed.
pub fn reap_leftovers() {
    let mut records = Vec::new();
    edit_running(|r| records = std::mem::take(r));
    let handles: Vec<_> = records
        .into_iter()
        .map(|r| {
            thread::spawn(move || {
                if start_time(r.pgid) == Some(r.start) {
                    signal_group(r.pgid, libc::SIGTERM);
                    stop_group(r.pgid, &r.display);
                } else {
                    stop_xephyr(&r.display);
                }
            })
        })
        .collect();
    handles.into_iter().for_each(|h| drop(h.join()));
}

/// The leader's start time in clock ticks since boot (field 22 of /proc/<pid>/stat).
fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may contain spaces and parentheses: fields are counted after the last ')'.
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

fn signal_group(pgid: u32, signal: i32) {
    // SAFETY: plain syscall. A negative pid targets the whole group and nothing outside it.
    unsafe { libc::kill(-(pgid as i32), signal) };
}

/// After a TERM: gives the group about 1 s to exit, KILLs what is left, reaps the leader, then stops a Xephyr
/// that escaped the group on the agent's display.
fn stop_group(pgid: u32, display: &str) {
    for _ in 0..20 {
        // SAFETY: plain syscalls. WNOHANG reaps the leader if it is our child and has exited; kill 0 only probes.
        unsafe { libc::waitpid(pgid as i32, std::ptr::null_mut(), libc::WNOHANG) };
        if unsafe { libc::kill(-(pgid as i32), 0) } != 0 {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    signal_group(pgid, libc::SIGKILL);
    // SAFETY: as above; fails harmlessly when the leader is not our child (crash recovery).
    unsafe { libc::waitpid(pgid as i32, std::ptr::null_mut(), 0) };
    stop_xephyr(display);
}

/// TERMs a Xephyr serving `display` that one of the app's agents started (it carries AGENT_ENV in its environment).
fn stop_xephyr(display: &str) {
    let Ok(procs) = std::fs::read_dir("/proc") else { return };
    for p in procs.flatten() {
        let Some(pid) = p.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else { continue };
        let Ok(cmdline) = std::fs::read(p.path().join("cmdline")) else { continue };
        let mut args = cmdline.split(|b| *b == 0).map(String::from_utf8_lossy);
        let is_xephyr = args.next().is_some_and(|a| a.rsplit('/').next() == Some("Xephyr"));
        if !is_xephyr || !args.any(|a| a == display) {
            continue;
        }
        let ours = std::fs::read(p.path().join("environ"))
            .is_ok_and(|e| e.split(|b| *b == 0).any(|v| v == format!("{AGENT_ENV}=1").as_bytes()));
        if ours {
            // SAFETY: plain syscall on a process we identified above.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
}

fn apply(shared: &mut Shared, event: &Value) {
    // The first event (startup hooks, title) means the process is up; `init` itself only comes with the first turn.
    if shared.status == Some(Status::Starting) {
        shared.status = Some(Status::Idle);
    }
    if let Some(id) = event["session_id"].as_str() {
        shared.session_id = Some(id.into());
    }
    match (event["type"].as_str(), event["subtype"].as_str()) {
        (Some("command_lifecycle"), _) => match event["state"].as_str() {
            Some("started") => shared.status = Some(Status::Working),
            Some("completed") => shared.status = Some(Status::Idle),
            _ => {}
        },
        (Some("assistant"), _) => {
            for block in event["message"]["content"].as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("text") => shared.entries.push(Entry::Assistant(block["text"].as_str().unwrap_or("").into())),
                    Some("tool_use") => {
                        shared.entries.push(Entry::Tool(tool_summary(block)));
                        match block["name"].as_str() {
                            Some("mcp__nested-desktop__start_desktop") => shared.desktop = true,
                            Some("mcp__nested-desktop__stop_desktop") => shared.desktop = false,
                            _ => {}
                        }
                        let input = &block["input"];
                        if let (Some("SendMessage"), Some(to), Some(text)) =
                            (block["name"].as_str(), input["to"].as_str(), input["message"].as_str())
                        {
                            shared.outbox.push((to.into(), text.into()));
                        }
                    }
                    _ => {}
                }
            }
        }
        // Without --replay-user-messages a user event carrying text is something we did not type: a teammate's message.
        (Some("user"), _) => match &event["message"]["content"] {
            Value::String(text) => shared.entries.push(Entry::Incoming { from: "another session".into(), text: text.clone() }),
            Value::Array(blocks) => {
                for b in blocks.iter().filter(|b| b["type"] == "text") {
                    let text = b["text"].as_str().unwrap_or("").into();
                    shared.entries.push(Entry::Incoming { from: "another session".into(), text });
                }
            }
            _ => {}
        },
        (Some("control_response"), _) => {
            if let Some(commands) = event["response"]["response"]["commands"].as_array() {
                shared.commands = commands
                    .iter()
                    .filter_map(|c| Some((c["name"].as_str()?.into(), c["description"].as_str().unwrap_or("").into())))
                    .collect();
            }
        }
        (Some("result"), _) => {
            shared.status = Some(Status::Idle);
            if event["is_error"] == true {
                let why = event["result"].as_str().or(event["subtype"].as_str()).unwrap_or("error");
                shared.entries.push(Entry::Error(why.into()));
            }
        }
        _ => {}
    }
}

/// The next display from :100 up whose X socket and lock file are both absent.
fn free_display() -> String {
    loop {
        let n = NEXT_DISPLAY.fetch_add(1, Ordering::Relaxed);
        let used = std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists() || std::path::Path::new(&format!("/tmp/.X{n}-lock")).exists();
        if !used {
            return format!(":{n}");
        }
    }
}

/// Grabs the agent's display a few times per second while it has a desktop; ends when the agent is dropped.
fn grab_loop(shared: Weak<Mutex<Shared>>, display: String) {
    while let Some(shared) = shared.upgrade() {
        let wanted = shared.lock().unwrap().desktop && DESKTOP_PREVIEW.load(Ordering::Relaxed);
        let thumb = if wanted { grab(&display).map(Arc::new) } else { None };
        shared.lock().unwrap().thumbnail = thumb;
        drop(shared);
        thread::sleep(Duration::from_millis(300));
    }
}

fn grab(display: &str) -> Option<Thumbnail> {
    use x11rb::{connection::Connection, protocol::xproto::*};
    let (conn, screen) = x11rb::connect(Some(display)).ok()?;
    let root = &conn.setup().roots[screen];
    let (w, h) = (root.width_in_pixels as u32, root.height_in_pixels as u32);
    let img = conn.get_image(ImageFormat::Z_PIXMAP, root.root, 0, 0, w as u16, h as u16, !0).ok()?.reply().ok()?;
    // Xephyr's default 24-bit visual is 32 bits per pixel, BGRX little-endian.
    if w == 0 || img.data.len() != (w * h * 4) as usize {
        return None;
    }
    // BGRX to RGBA in place, then an area-averaging downscale (sharp text, no nearest-neighbour blocks).
    let mut data = img.data;
    for px in data.chunks_exact_mut(4) {
        px.swap(0, 2);
        px[3] = 255;
    }
    let full = image::RgbaImage::from_raw(w, h, data)?;
    let tw = (THUMB_WIDTH * THUMB_OVERSAMPLE).min(w);
    let th = (tw * h / w).max(1);
    let rgba = image::imageops::thumbnail(&full, tw, th).into_raw();
    Some(Thumbnail { width: tw, height: th, rgba })
}

pub fn tool_summary(block: &Value) -> String {
    let name = block["name"].as_str().unwrap_or("tool");
    let input = &block["input"];
    let detail = ["command", "file_path", "to", "pattern", "query", "description", "url"]
        .iter()
        .find_map(|k| input[*k].as_str())
        .unwrap_or("");
    let detail: String = detail.lines().next().unwrap_or("").chars().take(120).collect();
    if detail.is_empty() { name.into() } else { format!("{name}: {detail}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_turns_and_messages() {
        let mut s = Shared { status: Some(Status::Starting), ..Default::default() };
        apply(&mut s, &json!({"type":"system","subtype":"hook_started","session_id":"abc"}));
        assert!(s.status == Some(Status::Idle));
        assert_eq!(s.session_id.as_deref(), Some("abc"));
        apply(&mut s, &json!({"type":"control_response","response":{"subtype":"success","request_id":"init",
            "response":{"commands":[{"name":"simplify","description":"Clean up code","argumentHint":""}]}}}));
        assert_eq!(s.commands, [("simplify".to_string(), "Clean up code".to_string())]);
        apply(&mut s, &json!({"type":"command_lifecycle","state":"started"}));
        apply(&mut s, &json!({"type":"user","message":{"content":"ping from Bob"}}));
        apply(&mut s, &json!({"type":"user","message":{"content":[{"type":"tool_result","content":"x"}]}}));
        apply(&mut s, &json!({"type":"assistant","message":{"content":[
            {"type":"tool_use","name":"Bash","input":{"command":"cargo build\nmore"}},
            {"type":"tool_use","name":"SendMessage","input":{"to":"Bob","message":"please test"}},
            {"type":"text","text":"done"}]}}));
        assert!(s.status == Some(Status::Working));
        apply(&mut s, &json!({"type":"result","subtype":"success","is_error":false}));
        assert!(s.status == Some(Status::Idle));

        let shown: Vec<String> = s
            .entries
            .iter()
            .map(|e| match e {
                Entry::Incoming { text, .. } => format!("in:{text}"),
                Entry::Tool(t) => format!("tool:{t}"),
                Entry::Assistant(t) => format!("out:{t}"),
                _ => "other".into(),
            })
            .collect();
        assert_eq!(shown, ["in:ping from Bob", "tool:Bash: cargo build", "tool:SendMessage: Bob", "out:done"]);
        assert_eq!(s.outbox, [("Bob".to_string(), "please test".to_string())]);
    }
}

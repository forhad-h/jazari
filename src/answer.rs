//! Answering a session's permission prompts and questions from jazari's dashboard.
//!
//! The `PermissionRequest` hook (`jazari watch answer --hook`) sends the pending prompt to the
//! dashboard over the dashboard's own Unix socket and waits for an answer. Claude Code keeps its
//! terminal prompt open while the hook waits, so the first answer wins: the dashboard's, through
//! the hook, or the user's at the terminal. A waiting `PreToolUse` hook would hide the terminal
//! prompt, so only this hook waits. See
//! `docs/solutions/integration-issues/claude-code-permission-request-hook-races-the-terminal-prompt.md`.
//!
//! Every Claude Code session runs as the same user as the dashboard. So an answer is trusted only
//! from the socket's server process, checked through the kernel's peer credentials: the same user,
//! a jazari process, and no Claude Code process among its ancestors. The request and its answer
//! travel only over that socket, never through a file that a session could write.
//!
//! Rules: the feature is off unless turned on, and then the hook reads only the setting and prints
//! nothing; only the user answers; no dashboard, a failed check, a timeout or a terminal answer
//! makes the hook exit with no output, so the terminal prompt decides; a plan approval
//! (ExitPlanMode) is left to the terminal, because a plain `allow` does not approve it. Windows has
//! no socket here, so there the hook never answers.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

/// A prompt is waiting for an answer from jazari.
pub const OPENED_EVENT: &str = "JazariPromptOpened";
/// The prompt was answered from jazari, at the terminal, or not at all.
pub const CLOSED_EVENT: &str = "JazariPromptClosed";

/// How long the hook waits for an answer, unless the setting says otherwise.
pub const DEFAULT_WAIT_SECS: u64 = 120;
/// The longest wait the setting allows: each waiting hook is a running process.
pub const MAX_WAIT_SECS: u64 = 600;
const POLL: Duration = Duration::from_millis(500);
/// The most text of a command or input that a request keeps.
const MAX_PREVIEW_CHARS: usize = 2000;
const DENY_MESSAGE: &str = "Denied from jazari by the user.";

/// Whether the hook waits for answers from the dashboard (off unless turned on).
pub fn enabled() -> bool {
    crate::settings::get("answer_prompts").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// How long the hook waits, in seconds.
pub fn wait_secs() -> u64 {
    crate::settings::get("answer_wait_secs").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_WAIT_SECS).clamp(10, MAX_WAIT_SECS)
}

/// The dashboard's socket. Its folder is private to the user.
pub fn socket_path() -> PathBuf {
    crate::data_dir().join("answer").join("dashboard.sock")
}

/// One question of an AskUserQuestion prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub question: String,
    pub options: Vec<String>,
    pub multi: bool,
}

/// A prompt that waits for an answer, as the dashboard shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub session: String,
    pub tool: String,
    /// The command, file path or input, with secrets redacted.
    pub preview: String,
    /// The questions of an AskUserQuestion prompt; empty for a permission prompt.
    pub questions: Vec<Question>,
    /// Whether Claude Code offered an "always allow" rule for this prompt.
    pub always_allow: bool,
    pub opened: f64,
    pub until: f64,
}

/// The user's answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum Answer {
    Allow,
    /// Allow, and add the rule that Claude Code suggested.
    AlwaysAllow,
    Deny,
    /// The chosen option for each question, by question text. A multi-select answer joins its
    /// labels with ", ".
    Choose { answers: BTreeMap<String, String> },
}

/// Whether `answer` is a valid answer to `req`: a choice of offered options for a question, and
/// allow, deny or an offered "always allow" for a permission prompt.
pub fn check(req: &Request, answer: &Answer) -> anyhow::Result<()> {
    match answer {
        Answer::Allow | Answer::Deny => anyhow::ensure!(req.questions.is_empty(), "this prompt is a question: choose an option"),
        Answer::AlwaysAllow => anyhow::ensure!(req.questions.is_empty() && req.always_allow, "this prompt offers no \"always allow\" rule"),
        Answer::Choose { answers } => {
            anyhow::ensure!(!req.questions.is_empty(), "this prompt is not a question");
            anyhow::ensure!(answers.len() == req.questions.len(), "answer every question");
            for q in &req.questions {
                let chosen = answers.get(&q.question).ok_or_else(|| anyhow::anyhow!("no option chosen for {:?}", q.question))?;
                let fits = |label: &str| q.options.iter().any(|o| o == label);
                let ok = if q.multi { chosen.split(", ").all(fits) } else { fits(chosen) };
                anyhow::ensure!(ok, "{chosen:?} is not an option of {:?}", q.question);
            }
        }
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn cut(text: &str) -> String {
    match text.char_indices().nth(MAX_PREVIEW_CHARS) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// The request for the hook input `raw`, or `None` when jazari leaves the prompt to the terminal.
fn request_for(raw: &Value, now: f64, wait: u64) -> Option<Request> {
    let session = raw["session_id"].as_str().filter(|s| valid_id(s))?;
    let tool = raw["tool_name"].as_str()?;
    if tool == "ExitPlanMode" {
        return None;
    }
    let input = &raw["tool_input"];
    let questions: Vec<Question> = if tool == "AskUserQuestion" {
        input["questions"]
            .as_array()?
            .iter()
            .map(|q| {
                Some(Question {
                    question: q["question"].as_str()?.to_string(),
                    options: q["options"].as_array()?.iter().filter_map(|o| o["label"].as_str().map(String::from)).collect(),
                    multi: q["multiSelect"].as_bool().unwrap_or(false),
                })
            })
            .collect::<Option<Vec<_>>>()
            .filter(|qs| !qs.is_empty() && qs.iter().all(|q| !q.options.is_empty()))?
    } else {
        Vec::new()
    };
    let preview = if !questions.is_empty() {
        questions.iter().map(|q| q.question.as_str()).collect::<Vec<_>>().join("\n")
    } else if let Some(command) = input["command"].as_str() {
        command.to_string()
    } else if let Some(path) = input["file_path"].as_str().or(input["path"].as_str()).or(input["notebook_path"].as_str()) {
        path.to_string()
    } else {
        input.to_string()
    };
    Some(Request {
        session: session.to_string(),
        tool: tool.to_string(),
        preview: cut(&crate::jazari::telegram::redact(&preview)),
        always_allow: questions.is_empty() && raw["permission_suggestions"].as_array().is_some_and(|s| !s.is_empty()),
        questions,
        opened: now,
        until: now + wait as f64,
    })
}

/// The hook output that applies `answer` to the prompt in `raw`.
fn output(raw: &Value, answer: &Answer) -> Value {
    let decision = match answer {
        Answer::Allow => json!({ "behavior": "allow" }),
        Answer::AlwaysAllow => json!({ "behavior": "allow", "updatedPermissions": raw["permission_suggestions"] }),
        Answer::Deny => json!({ "behavior": "deny", "message": DENY_MESSAGE }),
        Answer::Choose { answers } => {
            let mut input = raw["tool_input"].clone();
            input["answers"] = json!(answers);
            json!({ "behavior": "allow", "updatedInput": input })
        }
    };
    json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } })
}

/// Whether a hook event shows that the prompt was answered at the terminal: any later event of the
/// session's main agent (a subagent's events carry `agent_id`). A permission request is left out,
/// because the event log records this same prompt too.
fn answered_at_desk(ev: &Value, session: &str) -> bool {
    ev["session_id"].as_str() == Some(session)
        && ev.get("agent_id").is_none()
        && matches!(
            ev["hook_event_name"].as_str(),
            Some("PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "Stop" | "StopFailure" | "UserPromptSubmit" | "SessionEnd")
        )
}

/// What the hook reads from the dashboard.
#[derive(Debug, PartialEq)]
enum Recv {
    Line(Vec<u8>),
    /// Nothing yet.
    Wait,
    /// The dashboard closed the connection.
    Closed,
}

/// The hook's connection to the dashboard.
trait Conn {
    fn send(&mut self, line: &[u8]) -> std::io::Result<()>;
    /// Waits up to [`POLL`] for a line.
    fn recv(&mut self) -> Recv;
}

/// The hook's surroundings, so that tests use no socket, a fixed clock and a scratch event log.
struct Env {
    connect: Box<dyn FnMut() -> Option<Box<dyn Conn>>>,
    events: PathBuf,
    wait: u64,
    now: Box<dyn FnMut() -> f64>,
    log: Box<dyn FnMut(Value)>,
}

/// How the prompt closed.
#[derive(Debug, PartialEq)]
enum Outcome {
    Dashboard(Answer),
    Desk,
    Timeout,
    /// The dashboard closed the connection or sent something that is not a valid answer.
    Dropped,
}

impl Outcome {
    fn key(&self) -> &'static str {
        match self {
            Outcome::Dashboard(_) => "dashboard",
            Outcome::Desk => "desk",
            Outcome::Timeout => "timeout",
            Outcome::Dropped => "dropped",
        }
    }
}

/// Sends the request to a trusted dashboard and waits for the first answer. Returns the hook
/// output, or `None` when the terminal prompt decides.
fn handle(env: &mut Env, raw: &Value) -> Option<Value> {
    let req = request_for(raw, (env.now)(), env.wait)?;
    let mut conn = (env.connect)()?;
    let mut line = serde_json::to_vec(&req).ok()?;
    line.push(b'\n');
    conn.send(&line).ok()?;
    (env.log)(json!({ "ts": req.opened, "session_id": req.session, "hook_event_name": OPENED_EVENT, "tool": req.tool }));
    let mut events = crate::jsonl::JsonlTail::at_end(env.events.clone());
    let outcome = loop {
        match conn.recv() {
            Recv::Line(bytes) => {
                break match serde_json::from_slice::<Answer>(&bytes) {
                    Ok(a) if check(&req, &a).is_ok() => Outcome::Dashboard(a),
                    _ => Outcome::Dropped,
                };
            }
            Recv::Closed => break Outcome::Dropped,
            Recv::Wait => {}
        }
        if events.read_new().iter().any(|ev| answered_at_desk(ev, &req.session)) {
            break Outcome::Desk;
        }
        if (env.now)() >= req.until {
            break Outcome::Timeout;
        }
    };
    let answer = match &outcome {
        Outcome::Dashboard(a) => serde_json::to_value(a).ok(),
        _ => None,
    };
    (env.log)(json!({
        "ts": (env.now)(), "session_id": req.session, "hook_event_name": CLOSED_EVENT,
        "tool": req.tool, "outcome": outcome.key(), "answer": answer, "preview": req.preview,
    }));
    match outcome {
        Outcome::Dashboard(a) => Some(output(raw, &a)),
        _ => None,
    }
}

#[derive(clap::Args)]
pub struct AnswerArgs {
    /// Hook entrypoint called by Claude Code on PermissionRequest.
    #[arg(long, hide = true)]
    pub hook: bool,
    /// Wait for answers from the dashboard (the terminal prompt stays open; the first answer wins).
    #[arg(long, conflicts_with_all = ["off", "hook"])]
    on: bool,
    /// Stop waiting for answers from the dashboard.
    #[arg(long, conflicts_with = "hook")]
    off: bool,
    /// How long a prompt waits for an answer from the dashboard, in seconds (10 to 600).
    #[arg(long, value_parser = clap::value_parser!(u64).range(10..=MAX_WAIT_SECS), conflicts_with = "hook")]
    wait: Option<u64>,
}

/// `jazari watch answer`: the hook, or turning the feature on or off.
pub fn run(a: AnswerArgs) -> anyhow::Result<()> {
    if a.hook {
        return run_hook(enabled, &mut std::io::stdin(), &mut std::io::stdout());
    }
    if a.on || a.off {
        crate::settings::set("answer_prompts", json!(a.on))?;
    }
    if let Some(secs) = a.wait {
        crate::settings::set("answer_wait_secs", json!(secs))?;
    }
    if !cfg!(unix) {
        println!("Answering from the dashboard works on macOS and Linux only.");
    } else if enabled() {
        println!("Answering from the dashboard is on: a prompt waits {} s for an answer in the dashboard (key r), and the terminal prompt stays open.", wait_secs());
    } else {
        println!("Answering from the dashboard is off. Turn it on with `jazari watch answer --on`.");
    }
    Ok(())
}

/// `jazari watch answer --hook`: Claude Code's `PermissionRequest` hook. When the feature is off
/// it reads only the setting and writes nothing, so the terminal prompt behaves as without jazari.
fn run_hook(on: fn() -> bool, stdin: &mut dyn std::io::Read, stdout: &mut dyn std::io::Write) -> anyhow::Result<()> {
    if !on() || !cfg!(unix) {
        return Ok(());
    }
    let mut input = String::new();
    stdin.read_to_string(&mut input)?;
    let Ok(raw) = serde_json::from_str::<Value>(&input) else { return Ok(()) };
    let mut env = Env {
        connect: Box::new(server::connect),
        events: crate::claude_data_dir().join("events.jsonl"),
        wait: wait_secs(),
        now: Box::new(crate::now),
        log: Box::new(|ev| {
            if let Ok(mut line) = serde_json::to_vec(&ev) {
                line.push(b'\n');
                let _ = crate::hook::append(&crate::claude_data_dir(), &line);
            }
        }),
    };
    if let Some(out) = handle(&mut env, &raw) {
        writeln!(stdout, "{out}")?;
    }
    Ok(())
}

/// The dashboard's side: the socket server, and the hook's side of the connection.
pub mod server {
    use super::{Answer, Conn, Recv, Request};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, Sender};

    /// A prompt waiting in the dashboard, with the way back to its hook.
    pub struct Pending {
        pub req: Request,
        reply: Sender<Answer>,
        /// Set when the hook has gone (answered at the terminal, timed out) or was answered.
        closed: Arc<AtomicBool>,
    }

    /// The prompts that wait for the dashboard's answer.
    pub struct Server {
        rx: Receiver<Pending>,
        pending: Vec<Pending>,
    }

    impl Server {
        /// Takes in new prompts and drops those whose hook has gone.
        pub fn poll(&mut self, now: f64) {
            while let Ok(p) = self.rx.try_recv() {
                self.pending.push(p);
            }
            self.pending.retain(|p| !p.closed.load(Ordering::SeqCst) && p.req.until > now);
        }

        /// The oldest prompt of `session` that waits for an answer.
        pub fn waiting(&self, session: &str) -> Option<&Request> {
            self.pending.iter().map(|p| &p.req).find(|r| r.session == session)
        }

        pub fn count(&self) -> usize {
            self.pending.len()
        }

        /// Sends `answer` to the oldest prompt of `session`.
        pub fn answer(&mut self, session: &str, answer: Answer) -> anyhow::Result<()> {
            let i = self.pending.iter().position(|p| p.req.session == session && !p.closed.load(Ordering::SeqCst));
            let i = i.ok_or_else(|| anyhow::anyhow!("that prompt was already answered or has closed"))?;
            super::check(&self.pending[i].req, &answer)?;
            let p = self.pending.remove(i);
            p.reply.send(answer).map_err(|_| anyhow::anyhow!("that prompt has closed"))
        }

        #[cfg(test)]
        pub fn for_tests(reqs: Vec<Request>) -> (Server, Vec<Receiver<Answer>>) {
            let (tx, rx) = std::sync::mpsc::channel();
            let mut answers = Vec::new();
            for req in reqs {
                let (reply, got) = std::sync::mpsc::channel();
                tx.send(Pending { req, reply, closed: Arc::default() }).unwrap();
                answers.push(got);
            }
            (Server { rx, pending: Vec::new() }, answers)
        }
    }

    /// Starts the dashboard's socket server, unless another dashboard already serves it or the
    /// platform has no Unix sockets.
    #[cfg(unix)]
    pub fn start() -> Option<Server> {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::{UnixListener, UnixStream};
        let path = super::socket_path();
        let dir = path.parent()?;
        std::fs::create_dir_all(dir).ok()?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).ok()?;
        if UnixStream::connect(&path).is_ok() {
            return None;
        }
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).ok()?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                // Only the user's own processes may send prompts.
                if peer(&stream).is_some_and(|(_, uid)| uid == me()) {
                    let tx = tx.clone();
                    std::thread::spawn(move || serve(stream, tx));
                }
            }
        });
        Some(Server { rx, pending: Vec::new() })
    }

    #[cfg(not(unix))]
    pub fn start() -> Option<Server> {
        None
    }

    /// One hook's connection: read its request, hand it to the dashboard, then write the answer,
    /// or notice that the hook has gone.
    #[cfg(unix)]
    fn serve(stream: std::os::unix::net::UnixStream, tx: Sender<Pending>) {
        use std::io::{BufRead, Read, Write};
        let Ok(read_half) = stream.try_clone() else { return };
        let _ = read_half.set_read_timeout(Some(std::time::Duration::from_secs(2)));
        let mut line = Vec::new();
        if std::io::BufReader::new(read_half).read_until(b'\n', &mut line).is_err() {
            return;
        }
        let Ok(req) = serde_json::from_slice::<Request>(&line) else { return };
        let (reply, answers) = std::sync::mpsc::channel();
        let closed = Arc::new(AtomicBool::new(false));
        if tx.send(Pending { req, reply, closed: closed.clone() }).is_err() {
            return;
        }
        let mut stream = stream;
        let _ = stream.set_nonblocking(true);
        loop {
            match answers.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(answer) => {
                    let _ = stream.set_nonblocking(false);
                    let mut out = serde_json::to_vec(&answer).unwrap_or_default();
                    out.push(b'\n');
                    let _ = stream.write_all(&out);
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => match stream.read(&mut [0u8; 64]) {
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    // The hook closed the connection (or sent more, which it never does).
                    _ => break,
                },
            }
        }
        closed.store(true, Ordering::SeqCst);
    }

    #[cfg(unix)]
    fn me() -> u32 {
        // SAFETY: getuid has no preconditions and cannot fail.
        unsafe { libc::getuid() }
    }

    /// The pid and uid of the process on the other end of `stream`, from the kernel.
    #[cfg(target_os = "linux")]
    fn peer(stream: &std::os::unix::net::UnixStream) -> Option<(u32, u32)> {
        use std::os::fd::AsRawFd;
        let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: cred and len are valid for writes of the sizes given.
        let rc = unsafe { libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len) };
        (rc == 0 && cred.pid > 0).then_some((cred.pid as u32, cred.uid))
    }

    #[cfg(target_os = "macos")]
    fn peer(stream: &std::os::unix::net::UnixStream) -> Option<(u32, u32)> {
        use std::os::fd::AsRawFd;
        let fd = stream.as_raw_fd();
        let mut pid: libc::pid_t = 0;
        let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
        // SAFETY: pid and len are valid for writes of the sizes given.
        let rc = unsafe { libc::getsockopt(fd, libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&mut pid as *mut libc::pid_t).cast(), &mut len) };
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: uid and gid are valid for writes.
        let rc2 = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
        (rc == 0 && rc2 == 0 && pid > 0).then_some((pid as u32, uid))
    }

    /// Other Unix systems: no peer check, so no answers.
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    fn peer(_: &std::os::unix::net::UnixStream) -> Option<(u32, u32)> {
        None
    }

    /// Whether `pid` is a jazari dashboard that the user started, not one that a Claude Code
    /// session started: a jazari process with no Claude Code process among its ancestors.
    #[cfg(unix)]
    fn trusted_dashboard(pid: u32) -> bool {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
        let mut sys = System::new();
        let mut at = Pid::from_u32(pid);
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[at]), true, ProcessRefreshKind::nothing());
        if !sys.process(at).is_some_and(|p| p.name().to_string_lossy().to_lowercase().contains("jazari")) {
            return false;
        }
        for _ in 0..64 {
            if crate::watch::is_claude_process(at.as_u32()) {
                return false;
            }
            sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[at]), true, ProcessRefreshKind::nothing());
            match sys.process(at).and_then(|p| p.parent()) {
                Some(parent) if parent != at => at = parent,
                _ => return true,
            }
        }
        false
    }

    /// The hook's side: connect to the dashboard and check that the server is a trusted dashboard.
    #[cfg(unix)]
    pub(super) fn connect() -> Option<Box<dyn Conn>> {
        let stream = std::os::unix::net::UnixStream::connect(super::socket_path()).ok()?;
        let (pid, uid) = peer(&stream)?;
        if uid != me() || !trusted_dashboard(pid) {
            return None;
        }
        stream.set_read_timeout(Some(super::POLL)).ok()?;
        let reader = std::io::BufReader::new(stream.try_clone().ok()?);
        Some(Box::new(UnixConn { reader, writer: stream, line: Vec::new() }))
    }

    #[cfg(not(unix))]
    pub(super) fn connect() -> Option<Box<dyn Conn>> {
        None
    }

    #[cfg(unix)]
    struct UnixConn {
        reader: std::io::BufReader<std::os::unix::net::UnixStream>,
        writer: std::os::unix::net::UnixStream,
        /// A line read in parts across timeouts.
        line: Vec<u8>,
    }

    #[cfg(unix)]
    impl Conn for UnixConn {
        fn send(&mut self, line: &[u8]) -> std::io::Result<()> {
            std::io::Write::write_all(&mut self.writer, line)
        }

        fn recv(&mut self) -> Recv {
            use std::io::BufRead;
            match self.reader.read_until(b'\n', &mut self.line) {
                Ok(0) => Recv::Closed,
                Ok(_) if self.line.ends_with(b"\n") => Recv::Line(std::mem::take(&mut self.line)),
                Ok(_) => Recv::Wait,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Recv::Wait,
                Err(_) => Recv::Closed,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jazari-answer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn bash(cmd: &str) -> Value {
        json!({ "session_id": "s1", "hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": { "command": cmd },
                "permission_suggestions": [{ "type": "addRules", "rules": [{ "toolName": "Bash", "ruleContent": "mkdir:*" }], "behavior": "allow", "destination": "localSettings" }] })
    }

    fn ask() -> Value {
        json!({ "session_id": "s1", "hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion",
                "tool_input": { "questions": [{ "question": "Pick a fruit", "header": "Fruit", "multiSelect": false,
                                                "options": [{ "label": "Apple", "description": "" }, { "label": "Pear", "description": "" }] }] } })
    }

    /// A fake dashboard connection: each `recv` takes the next scripted step and moves the clock
    /// one second; `sent` keeps what the hook sent.
    struct Fake {
        script: Vec<Recv>,
        sent: Rc<RefCell<Vec<u8>>>,
        clock: Rc<RefCell<f64>>,
        on_recv: Box<dyn FnMut(usize)>,
        n: usize,
    }

    impl Conn for Fake {
        fn send(&mut self, line: &[u8]) -> std::io::Result<()> {
            self.sent.borrow_mut().extend_from_slice(line);
            Ok(())
        }
        fn recv(&mut self) -> Recv {
            self.n += 1;
            *self.clock.borrow_mut() += 1.0;
            (self.on_recv)(self.n);
            if self.script.is_empty() { Recv::Wait } else { self.script.remove(0) }
        }
    }

    struct Run {
        env: Env,
        sent: Rc<RefCell<Vec<u8>>>,
        logged: Rc<RefCell<Vec<Value>>>,
    }

    fn run(dir: &std::path::Path, wait: u64, script: Vec<Recv>, on_recv: impl FnMut(usize) + 'static) -> Run {
        let clock = Rc::new(RefCell::new(1000.0));
        let sent = Rc::new(RefCell::new(Vec::new()));
        let logged = Rc::new(RefCell::new(Vec::new()));
        let mut fake = Some(Fake { script, sent: sent.clone(), clock: clock.clone(), on_recv: Box::new(on_recv), n: 0 });
        let l = logged.clone();
        let env = Env {
            connect: Box::new(move || fake.take().map(|f| Box::new(f) as Box<dyn Conn>)),
            events: dir.join("events.jsonl"),
            wait,
            now: Box::new(move || *clock.borrow()),
            log: Box::new(move |ev| l.borrow_mut().push(ev)),
        };
        Run { env, sent, logged }
    }

    fn line(a: &Answer) -> Recv {
        let mut l = serde_json::to_vec(a).unwrap();
        l.push(b'\n');
        Recv::Line(l)
    }

    #[test]
    fn a_dashboard_answer_allows_and_is_logged() {
        let dir = scratch("allow");
        let mut r = run(&dir, 120, vec![Recv::Wait, line(&Answer::Allow)], |_| {});
        let out = handle(&mut r.env, &bash("mkdir out")).unwrap();
        assert_eq!(out["hookSpecificOutput"]["decision"]["behavior"], "allow");
        let sent: Request = serde_json::from_slice(r.sent.borrow().strip_suffix(b"\n").unwrap()).unwrap();
        assert_eq!((sent.preview.as_str(), sent.always_allow), ("mkdir out", true));
        let closed = r.logged.borrow().last().cloned().unwrap();
        assert_eq!((closed["hook_event_name"].as_str(), closed["outcome"].as_str()), (Some(CLOSED_EVENT), Some("dashboard")));
    }

    #[test]
    fn a_terminal_answer_a_timeout_or_a_bad_reply_leaves_the_decision_to_the_terminal() {
        let dir = scratch("desk");
        let events = dir.join("events.jsonl");
        std::fs::write(&events, "{\"session_id\":\"s1\",\"hook_event_name\":\"PostToolUse\"}\n").unwrap();
        let ev = events.clone();
        let mut r = run(&dir, 120, vec![], move |n| {
            let line = match n {
                1 => "{\"session_id\":\"s1\",\"hook_event_name\":\"PermissionRequest\"}\n",
                2 => "{\"session_id\":\"s1\",\"hook_event_name\":\"PostToolUse\",\"agent_id\":\"a1\"}\n",
                3 => "{\"session_id\":\"s2\",\"hook_event_name\":\"PostToolUse\"}\n",
                4 => "{\"session_id\":\"s1\",\"hook_event_name\":\"PostToolUse\"}\n",
                _ => return,
            };
            std::io::Write::write_all(&mut std::fs::OpenOptions::new().append(true).open(&ev).unwrap(), line.as_bytes()).unwrap();
        });
        assert_eq!(handle(&mut r.env, &bash("rm -rf build")), None);
        assert_eq!(r.logged.borrow().last().unwrap()["outcome"], "desk", "only the main agent's later event counts");

        let mut r = run(&dir, 5, vec![], |_| {});
        assert_eq!(handle(&mut r.env, &bash("ls")), None);
        assert_eq!(r.logged.borrow().last().unwrap()["outcome"], "timeout");

        // A reply that does not fit the prompt, or a closed connection, never allows.
        let mut r = run(&dir, 120, vec![line(&Answer::AlwaysAllow)], |_| {});
        let no_rule = json!({ "session_id": "s1", "tool_name": "Write", "tool_input": { "file_path": "/w/a.rs" } });
        assert_eq!(handle(&mut r.env, &no_rule), None);
        let mut r = run(&dir, 120, vec![Recv::Line(b"{\"answer\":\"allow\"".to_vec()), Recv::Closed], |_| {});
        assert_eq!(handle(&mut r.env, &bash("ls")), None);
        let mut r = run(&dir, 120, vec![Recv::Closed], |_| {});
        assert_eq!(handle(&mut r.env, &bash("ls")), None);
    }

    #[test]
    fn without_a_trusted_dashboard_the_hook_does_nothing() {
        let dir = scratch("nodash");
        let mut r = run(&dir, 120, vec![], |_| {});
        r.env.connect = Box::new(|| None);
        assert_eq!(handle(&mut r.env, &bash("ls")), None);
        assert!(r.logged.borrow().is_empty());
    }

    #[test]
    fn when_off_the_hook_reads_nothing_and_prints_nothing() {
        let input: &[u8] = b"{\"session_id\":\"s1\",\"tool_name\":\"Bash\"}";
        let mut stdin = input;
        let mut out = Vec::new();
        run_hook(|| false, &mut stdin, &mut out).unwrap();
        assert!(out.is_empty());
        assert_eq!(stdin.len(), input.len(), "stdin is not even read");
    }

    #[test]
    fn a_question_is_answered_with_the_original_input_and_the_choice() {
        let dir = scratch("ask");
        let pear = Answer::Choose { answers: BTreeMap::from([("Pick a fruit".to_string(), "Pear".to_string())]) };
        let mut r = run(&dir, 120, vec![line(&pear)], |_| {});
        let out = handle(&mut r.env, &ask()).unwrap();
        let d = &out["hookSpecificOutput"]["decision"];
        assert_eq!(d["behavior"], "allow");
        assert_eq!(d["updatedInput"]["answers"]["Pick a fruit"], "Pear");
        assert_eq!(d["updatedInput"]["questions"][0]["header"], "Fruit", "the original questions are kept");
    }

    #[test]
    fn answers_must_fit_the_prompt() {
        let q = request_for(&ask(), 0.0, 120).unwrap();
        let choose = |label: &str| Answer::Choose { answers: BTreeMap::from([("Pick a fruit".to_string(), label.to_string())]) };
        assert!(check(&q, &choose("Pear")).is_ok());
        assert!(check(&q, &choose("Plum")).is_err(), "only an offered option");
        assert!(check(&q, &Answer::Allow).is_err(), "a question needs a choice");
        let p = request_for(&bash("ls"), 0.0, 120).unwrap();
        assert!(check(&p, &Answer::Deny).is_ok() && check(&p, &Answer::AlwaysAllow).is_ok());
        assert!(check(&p, &choose("Pear")).is_err());
    }

    #[test]
    fn the_dashboard_server_answers_the_oldest_open_prompt_of_a_session() {
        let a = request_for(&bash("ls"), 10.0, 120).unwrap();
        let mut b = request_for(&ask(), 20.0, 120).unwrap();
        b.session = "s2".into();
        let (mut server, answers) = server::Server::for_tests(vec![a, b]);
        server.poll(30.0);
        assert_eq!(server.count(), 2);
        assert_eq!(server.waiting("s2").unwrap().tool, "AskUserQuestion");
        assert!(server.answer("s2", Answer::Allow).is_err(), "a question needs a choice");
        server.answer("s1", Answer::Deny).unwrap();
        assert_eq!(answers[0].try_recv().unwrap(), Answer::Deny);
        assert!(server.answer("s1", Answer::Deny).is_err(), "answered once");
        server.poll(1000.0);
        assert_eq!(server.count(), 0, "an expired prompt is dropped");
    }

    #[test]
    fn outputs_match_what_claude_code_accepts() {
        let deny = output(&bash("x"), &Answer::Deny);
        assert_eq!(deny["hookSpecificOutput"]["decision"], json!({ "behavior": "deny", "message": DENY_MESSAGE }));
        let always = output(&bash("x"), &Answer::AlwaysAllow);
        assert_eq!(always["hookSpecificOutput"]["decision"]["updatedPermissions"], bash("x")["permission_suggestions"]);
        assert_eq!(always["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
    }

    #[test]
    fn plan_approval_and_bad_input_are_left_to_the_terminal() {
        let plan = json!({ "session_id": "s1", "tool_name": "ExitPlanMode", "tool_input": { "plan": "x" } });
        assert_eq!(request_for(&plan, 0.0, 120), None);
        let bad_id = json!({ "session_id": "../s1", "tool_name": "Bash", "tool_input": { "command": "ls" } });
        assert_eq!(request_for(&bad_id, 0.0, 120), None);
        let no_options = json!({ "session_id": "s1", "tool_name": "AskUserQuestion", "tool_input": { "questions": [{ "question": "Why?", "options": [] }] } });
        assert_eq!(request_for(&no_options, 0.0, 120), None);
        let write = json!({ "session_id": "s1", "tool_name": "Write", "tool_input": { "file_path": "/w/a.rs", "content": "x" } });
        let req = request_for(&write, 0.0, 120).unwrap();
        assert_eq!((req.preview.as_str(), req.always_allow), ("/w/a.rs", false));
    }

    #[test]
    fn previews_are_redacted_and_cut() {
        let secret = json!({ "session_id": "s1", "tool_name": "Bash", "tool_input": { "command": "curl -H 'Authorization: Bearer sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789'" } });
        let req = request_for(&secret, 0.0, 120).unwrap();
        assert!(!req.preview.contains("abcdefghijklmnopqrstuvwxyz0123456789"), "{}", req.preview);
        let long = json!({ "session_id": "s1", "tool_name": "Bash", "tool_input": { "command": "x".repeat(MAX_PREVIEW_CHARS + 10) } });
        assert_eq!(request_for(&long, 0.0, 120).unwrap().preview.chars().count(), MAX_PREVIEW_CHARS + 1);
    }
}

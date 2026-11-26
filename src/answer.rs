//! Answering a session's permission prompts and questions from jazari's dashboard.
//!
//! The `PermissionRequest` hook (`jazari watch answer --hook`) sends the pending prompt to the
//! dashboard over the dashboard's own Unix socket and waits for an answer. Claude Code keeps its
//! terminal prompt open while the hook waits, so the first answer wins: the dashboard's, through
//! the hook, or the user's at the terminal. A waiting `PreToolUse` hook would hide the terminal
//! prompt, so only this hook waits. See
//! `docs/solutions/integration-issues/claude-code-permission-request-hook-races-the-terminal-prompt.md`.
//!
//! Every Claude Code session runs as the same user as the dashboard, so no check on the socket can
//! prove that an answer came from the user. The check that matters runs in the hook itself: before
//! it allows anything, the hook asks for Touch ID or the Mac password, and the system dialog names
//! the session and the action from the hook's own input. A fake dashboard can send "allow", but the
//! user then sees a dialog for an action they did not choose. A deny needs no check, because a deny
//! is always safe. Systems without such a check (Linux, Windows) only take a deny.
//!
//! Defence in depth: the hook talks only to a server that the kernel's peer credentials show as the
//! same user, running the same jazari executable, with no Claude Code process among its parents.
//! The request and the answer never go through a file.
//!
//! Rules: the feature is off unless turned on, and then the hook reads only the setting and prints
//! nothing; only the user answers; no dashboard, a failed check, a cancelled confirmation, a timeout
//! or a terminal answer makes the hook exit with no output, so the terminal prompt decides; a plan
//! approval (ExitPlanMode) is left to the terminal, because a plain `allow` does not approve it.

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
/// How long the user has to confirm an answer with Touch ID or the password.
pub const CONFIRM_SECS: u64 = 120;
const POLL: Duration = Duration::from_millis(500);
/// The most text of a command, a file's content or a change that a request carries.
const MAX_DETAIL_CHARS: usize = 6000;
/// The most text of the one-line summary.
const MAX_SUMMARY_CHARS: usize = 120;
const DENY_MESSAGE: &str = "Denied from jazari by the user.";

/// jazari's data folder for this feature. A release build ignores `JAZARI_HOME`, so a project's
/// settings can't move the socket or the setting away from the user's own folder.
fn data_dir() -> PathBuf {
    if cfg!(debug_assertions) { crate::data_dir() } else { dirs::home_dir().unwrap_or_default().join(".jazari") }
}

fn setting(key: &str) -> Option<Value> {
    let text = std::fs::read_to_string(data_dir().join("settings.json")).ok()?;
    serde_json::from_str::<Value>(&text).ok()?.get(key).cloned()
}

/// Whether the hook waits for answers from the dashboard (off unless turned on).
pub fn enabled() -> bool {
    setting("answer_prompts").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// How long the hook waits, in seconds.
pub fn wait_secs() -> u64 {
    setting("answer_wait_secs").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_WAIT_SECS).clamp(10, MAX_WAIT_SECS)
}

/// The dashboard's socket. Its folder is private to the user.
pub fn socket_path() -> PathBuf {
    data_dir().join("answer").join("dashboard.sock")
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
    /// The session's project folder name, to name it in the confirmation.
    pub project: String,
    pub tool: String,
    /// One line: the command, or the file and what changes in it.
    pub summary: String,
    /// The full command, the file's new content or the change, with secrets redacted.
    pub detail: String,
    /// The questions of an AskUserQuestion prompt; empty for a permission prompt.
    pub questions: Vec<Question>,
    /// The permission rule that "always allow" adds, when Claude Code offered one.
    pub rule: Option<String>,
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
        Answer::AlwaysAllow => anyhow::ensure!(req.questions.is_empty() && req.rule.is_some(), "this prompt offers no \"always allow\" rule"),
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

/// What the user confirms with Touch ID or the password before `answer` applies. It is built from
/// the hook's own request, never from text the dashboard sent.
pub fn reason(req: &Request, answer: &Answer) -> String {
    let session = format!("session {} ({})", req.project, req.session.chars().take(8).collect::<String>());
    match answer {
        Answer::Choose { answers } => {
            let picked: Vec<String> = answers.iter().map(|(q, a)| format!("\"{}\": {}", cut(q, 80), cut(a, 80))).collect();
            format!("answer {} in {session}", picked.join("; "))
        }
        Answer::AlwaysAllow => format!("always allow {} in {session}", req.rule.as_deref().unwrap_or("?")),
        Answer::Allow | Answer::Deny => format!("allow {} {} in {session}", req.tool, req.summary),
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Text without control characters (terminal escapes among them); line breaks and tabs are kept.
pub fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect()
}

/// At most `max` characters, with "…" when cut.
fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

fn redacted(text: &str) -> String {
    clean(&crate::jazari::telegram::redact(text))
}

/// The one-line summary and the full detail of a permission prompt's input.
fn describe(tool: &str, input: &Value) -> (String, String) {
    let s = |k: &str| input[k].as_str().unwrap_or("");
    let lines = |t: &str| t.lines().count().max(1);
    let path = || [s("file_path"), s("notebook_path"), s("path")].into_iter().find(|p| !p.is_empty()).unwrap_or("").to_string();
    let (summary, detail) = match tool {
        "Bash" => (format!("`{}`", s("command").lines().next().unwrap_or("")), s("command").to_string()),
        "Write" => (format!("writing {} ({} lines)", path(), lines(s("content"))), s("content").to_string()),
        "Edit" => {
            let (old, new) = (s("old_string"), s("new_string"));
            let diff = old.lines().map(|l| format!("- {l}")).chain(new.lines().map(|l| format!("+ {l}"))).collect::<Vec<_>>().join("\n");
            (format!("editing {} ({} lines replaced by {})", path(), lines(old), lines(new)), diff)
        }
        "MultiEdit" => {
            let edits = input["edits"].as_array().cloned().unwrap_or_default();
            let diff = edits
                .iter()
                .flat_map(|e| {
                    let old = e["old_string"].as_str().unwrap_or("").lines().map(|l| format!("- {l}"));
                    let new = e["new_string"].as_str().unwrap_or("").lines().map(|l| format!("+ {l}"));
                    old.chain(new).chain(std::iter::once(String::new())).collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            (format!("editing {} ({} changes)", path(), edits.len()), diff)
        }
        "NotebookEdit" => (format!("editing {}", path()), s("new_source").to_string()),
        _ => (input.to_string(), serde_json::to_string_pretty(input).unwrap_or_default()),
    };
    let summary = redacted(&summary).replace(['\n', '\t'], " ");
    (cut(&summary, MAX_SUMMARY_CHARS), cut(&redacted(&detail), MAX_DETAIL_CHARS))
}

/// The rules that "always allow" would add, as text: `Bash(npm test:*) in localSettings`.
fn rule_text(suggestions: &Value) -> Option<String> {
    let list = suggestions.as_array().filter(|l| !l.is_empty())?;
    let parts: Vec<String> = list
        .iter()
        .map(|s| {
            let rules: Vec<String> = s["rules"]
                .as_array()
                .map(|rs| {
                    rs.iter()
                        .map(|r| match r["ruleContent"].as_str() {
                            Some(c) => format!("{}({c})", r["toolName"].as_str().unwrap_or("?")),
                            None => r["toolName"].as_str().unwrap_or("?").to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let what = if rules.is_empty() { s["type"].as_str().unwrap_or("a change").to_string() } else { rules.join(", ") };
            match s["destination"].as_str() {
                Some(d) => format!("{what} in {d}"),
                None => what,
            }
        })
        .collect();
    Some(clean(&parts.join("; ")))
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
                    question: clean(q["question"].as_str()?),
                    options: q["options"].as_array()?.iter().filter_map(|o| o["label"].as_str().map(clean)).collect(),
                    multi: q["multiSelect"].as_bool().unwrap_or(false),
                })
            })
            .collect::<Option<Vec<_>>>()
            .filter(|qs| !qs.is_empty() && qs.iter().all(|q| !q.options.is_empty()))?
    } else {
        Vec::new()
    };
    let (summary, detail) = if questions.is_empty() {
        describe(tool, input)
    } else {
        (cut(&questions[0].question, MAX_SUMMARY_CHARS), questions.iter().map(|q| q.question.as_str()).collect::<Vec<_>>().join("\n"))
    };
    let project = raw["cwd"].as_str().and_then(|c| std::path::Path::new(c).file_name()).map_or_else(|| "?".into(), |n| clean(&n.to_string_lossy()));
    Some(Request {
        session: session.to_string(),
        project,
        tool: clean(tool),
        summary,
        detail,
        rule: if questions.is_empty() { rule_text(&raw["permission_suggestions"]) } else { None },
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
    /// Whether the server is still the process that was checked when the connection opened.
    fn still_trusted(&self) -> bool;
}

/// The hook's surroundings, so that tests use no socket, no dialog, a fixed clock and a scratch
/// event log.
struct Env {
    connect: Box<dyn FnMut() -> Option<Box<dyn Conn>>>,
    /// Touch ID or the password, showing the reason; true only when the user confirmed.
    confirm: Box<dyn FnMut(&str) -> bool>,
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
    /// The user cancelled or failed the confirmation of an allow or an answer.
    NotConfirmed,
}

impl Outcome {
    fn key(&self) -> &'static str {
        match self {
            Outcome::Dashboard(_) => "dashboard",
            Outcome::Desk => "desk",
            Outcome::Timeout => "timeout",
            Outcome::Dropped => "dropped",
            Outcome::NotConfirmed => "not-confirmed",
        }
    }
}

/// Sends the request to the dashboard and waits for the first answer. An allow or an answer applies
/// only after the user confirms it with Touch ID or the password. Returns the hook output, or
/// `None` when the terminal prompt decides.
fn handle(env: &mut Env, raw: &Value) -> Option<Value> {
    let req = request_for(raw, (env.now)(), env.wait)?;
    let mut conn = (env.connect)()?;
    let mut line = serde_json::to_vec(&req).ok()?;
    line.push(b'\n');
    conn.send(&line).ok()?;
    (env.log)(json!({ "ts": req.opened, "session_id": req.session, "hook_event_name": OPENED_EVENT, "tool": req.tool }));
    let mut events = crate::jsonl::JsonlTail::at_end(env.events.clone());
    let mut outcome = loop {
        match conn.recv() {
            Recv::Line(bytes) => {
                break match serde_json::from_slice::<Answer>(&bytes) {
                    Ok(a) if check(&req, &a).is_ok() && conn.still_trusted() => Outcome::Dashboard(a),
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
    if let Outcome::Dashboard(a) = &outcome
        && *a != Answer::Deny
        && !(env.confirm)(&reason(&req, a))
    {
        outcome = Outcome::NotConfirmed;
    }
    let answer = match &outcome {
        Outcome::Dashboard(a) => serde_json::to_value(a).ok(),
        _ => None,
    };
    (env.log)(json!({
        "ts": (env.now)(), "session_id": req.session, "hook_event_name": CLOSED_EVENT,
        "tool": req.tool, "outcome": outcome.key(), "answer": answer, "summary": req.summary,
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

fn save_setting(key: &str, value: Value) -> anyhow::Result<()> {
    let path = data_dir().join("settings.json");
    let mut map = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<serde_json::Map<String, Value>>(&t).ok()).unwrap_or_default();
    map.insert(key.into(), value.clone());
    let a = crate::act::Action::new(crate::act::Kind::Setting, key).detail(value.to_string());
    crate::act::run(&a, || {
        std::fs::create_dir_all(data_dir())?;
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec_pretty(&Value::Object(map))?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    })
}

/// `jazari watch answer`: the hook, or turning the feature on or off. Only the user changes the
/// setting, from a terminal; a Claude Code session or a hook is refused.
pub fn run(a: AnswerArgs) -> anyhow::Result<()> {
    if a.hook {
        return run_hook(enabled, &mut std::io::stdin(), &mut std::io::stdout());
    }
    if a.on || a.off || a.wait.is_some() {
        anyhow::ensure!(
            !matches!(crate::act::origin(), crate::act::Origin::Session(_) | crate::act::Origin::Hook),
            "only the user can change this, from a terminal outside Claude Code"
        );
        if a.on || a.off {
            save_setting("answer_prompts", json!(a.on))?;
        }
        if let Some(secs) = a.wait {
            save_setting("answer_wait_secs", json!(secs))?;
        }
    }
    if !cfg!(unix) {
        println!("Answering from the dashboard works on macOS and Linux only.");
    } else if enabled() {
        let how = if crate::presence::available() { "an allow or an answer asks for Touch ID or your password" } else { "only a deny can be sent from here" };
        println!("Answering from the dashboard is on: a prompt waits {} s for you in the dashboard (key r), {how}, and the terminal prompt stays open.", wait_secs());
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
        confirm: Box::new(crate::presence::confirm),
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::{Receiver, Sender};

    /// The most hook connections the dashboard serves at once.
    const MAX_CONNECTIONS: usize = 32;

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
        /// The socket file this server bound, to notice when another process replaces it.
        bound: Option<(u64, u64)>,
    }

    impl Server {
        /// Takes in new prompts and drops those whose hook has gone. A prompt may wait no longer
        /// than the longest wait the setting allows, whatever its sender claims.
        pub fn poll(&mut self, now: f64) {
            while let Ok(mut p) = self.rx.try_recv() {
                p.req.until = p.req.until.min(now + super::MAX_WAIT_SECS as f64);
                self.pending.push(p);
            }
            self.pending.retain(|p| !p.closed.load(Ordering::SeqCst) && p.req.until > now);
        }

        /// Every prompt of `session` that waits for an answer, oldest first.
        pub fn waiting(&self, session: &str) -> Vec<&Request> {
            self.pending.iter().map(|p| &p.req).filter(|r| r.session == session).collect()
        }

        pub fn count(&self) -> usize {
            self.pending.len()
        }

        /// Whether the socket file is still the one this server bound. Another process of the user
        /// can remove it and bind its own, and hooks would then reach that process instead.
        pub fn intact(&self) -> bool {
            self.bound.is_none_or(|b| file_id(&super::socket_path()) == Some(b))
        }

        /// Sends `answer` to the prompt `req`.
        pub fn answer(&mut self, req: &Request, answer: Answer) -> anyhow::Result<()> {
            let i = self.pending.iter().position(|p| p.req == *req && !p.closed.load(Ordering::SeqCst));
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
            (Server { rx, pending: Vec::new(), bound: None }, answers)
        }
    }

    #[cfg(unix)]
    fn file_id(path: &std::path::Path) -> Option<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }

    #[cfg(not(unix))]
    fn file_id(_: &std::path::Path) -> Option<(u64, u64)> {
        None
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
        let bound = file_id(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        let open = Arc::new(AtomicUsize::new(0));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                // Only the user's own processes may send prompts, and only so many at once.
                if open.load(Ordering::SeqCst) >= MAX_CONNECTIONS || !peer(&stream).is_some_and(|(_, uid)| uid == me()) {
                    continue;
                }
                open.fetch_add(1, Ordering::SeqCst);
                let (tx, open) = (tx.clone(), open.clone());
                std::thread::spawn(move || {
                    serve(stream, tx);
                    open.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        Some(Server { rx, pending: Vec::new(), bound })
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
        if std::io::BufReader::new(read_half.take(64 * 1024)).read_until(b'\n', &mut line).is_err() {
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

    /// The executable and start time of `pid`.
    #[cfg(unix)]
    fn identity(pid: u32) -> Option<(std::path::PathBuf, u64)> {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
        let mut sys = System::new();
        let pid = Pid::from_u32(pid);
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing().with_exe(UpdateKind::Always));
        let p = sys.process(pid)?;
        Some((std::fs::canonicalize(p.exe()?).ok()?, p.start_time()))
    }

    /// Defence in depth, not proof: whether `pid` runs this same jazari executable, with no Claude
    /// Code process among its parents. A missing parent before the system's first process fails
    /// the check.
    #[cfg(unix)]
    fn trusted_dashboard(pid: u32) -> Option<(u32, u64)> {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
        let mine = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
        let (exe, started) = identity(pid)?;
        if exe != mine {
            return None;
        }
        let mut sys = System::new();
        let mut at = Pid::from_u32(pid);
        for _ in 0..64 {
            if crate::watch::is_claude_process(at.as_u32()) {
                return None;
            }
            if at.as_u32() <= 1 {
                return Some((pid, started));
            }
            sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[at]), true, ProcessRefreshKind::nothing());
            at = sys.process(at)?.parent()?;
        }
        None
    }

    /// The hook's side: connect to the dashboard and check its server process.
    #[cfg(unix)]
    pub(super) fn connect() -> Option<Box<dyn Conn>> {
        let stream = std::os::unix::net::UnixStream::connect(super::socket_path()).ok()?;
        let (pid, uid) = peer(&stream)?;
        if uid != me() {
            return None;
        }
        let server = trusted_dashboard(pid)?;
        stream.set_read_timeout(Some(super::POLL)).ok()?;
        let reader = std::io::BufReader::new(stream.try_clone().ok()?);
        Some(Box::new(UnixConn { reader, writer: stream, line: Vec::new(), server }))
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
        /// The server's pid and start time when the connection opened.
        server: (u32, u64),
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
                Ok(_) if self.line.len() > 64 * 1024 => Recv::Closed,
                Ok(_) => Recv::Wait,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Recv::Wait,
                Err(_) => Recv::Closed,
            }
        }

        /// The same pid with the same start time: a pid reused by a newer process fails.
        fn still_trusted(&self) -> bool {
            identity(self.server.0).is_some_and(|(_, started)| started == self.server.1)
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
        json!({ "session_id": "s1", "cwd": "/w/marsad", "hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": { "command": cmd },
                "permission_suggestions": [{ "type": "addRules", "rules": [{ "toolName": "Bash", "ruleContent": "mkdir:*" }], "behavior": "allow", "destination": "localSettings" }] })
    }

    fn ask() -> Value {
        json!({ "session_id": "s1", "cwd": "/w/marsad", "hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion",
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
        trusted: bool,
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
        fn still_trusted(&self) -> bool {
            self.trusted
        }
    }

    struct Run {
        env: Env,
        sent: Rc<RefCell<Vec<u8>>>,
        logged: Rc<RefCell<Vec<Value>>>,
        /// The reasons the user was asked to confirm.
        asked: Rc<RefCell<Vec<String>>>,
    }

    fn run(dir: &std::path::Path, wait: u64, script: Vec<Recv>, on_recv: impl FnMut(usize) + 'static) -> Run {
        run_with(dir, wait, script, on_recv, true, true)
    }

    fn run_with(dir: &std::path::Path, wait: u64, script: Vec<Recv>, on_recv: impl FnMut(usize) + 'static, confirmed: bool, trusted: bool) -> Run {
        let clock = Rc::new(RefCell::new(1000.0));
        let sent = Rc::new(RefCell::new(Vec::new()));
        let logged = Rc::new(RefCell::new(Vec::new()));
        let asked = Rc::new(RefCell::new(Vec::new()));
        let mut fake = Some(Fake { script, sent: sent.clone(), clock: clock.clone(), on_recv: Box::new(on_recv), n: 0, trusted });
        let (l, a) = (logged.clone(), asked.clone());
        let env = Env {
            connect: Box::new(move || fake.take().map(|f| Box::new(f) as Box<dyn Conn>)),
            confirm: Box::new(move |reason| {
                a.borrow_mut().push(reason.to_string());
                confirmed
            }),
            events: dir.join("events.jsonl"),
            wait,
            now: Box::new(move || *clock.borrow()),
            log: Box::new(move |ev| l.borrow_mut().push(ev)),
        };
        Run { env, sent, logged, asked }
    }

    fn line(a: &Answer) -> Recv {
        let mut l = serde_json::to_vec(a).unwrap();
        l.push(b'\n');
        Recv::Line(l)
    }

    #[test]
    fn an_allow_applies_only_after_the_user_confirms_the_hooks_own_action() {
        let dir = scratch("allow");
        let mut r = run(&dir, 120, vec![Recv::Wait, line(&Answer::Allow)], |_| {});
        let out = handle(&mut r.env, &bash("mkdir out")).unwrap();
        assert_eq!(out["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert_eq!(*r.asked.borrow(), ["allow Bash `mkdir out` in session marsad (s1)"]);
        let sent: Request = serde_json::from_slice(r.sent.borrow().strip_suffix(b"\n").unwrap()).unwrap();
        assert_eq!((sent.summary.as_str(), sent.rule.as_deref()), ("`mkdir out`", Some("Bash(mkdir:*) in localSettings")));
        assert_eq!(r.logged.borrow().last().unwrap()["outcome"], "dashboard");

        // A cancelled or failed confirmation (or a fake dashboard's allow) allows nothing.
        let mut r = run_with(&dir, 120, vec![line(&Answer::AlwaysAllow)], |_| {}, false, true);
        assert_eq!(handle(&mut r.env, &bash("rm -rf build")), None);
        assert_eq!(*r.asked.borrow(), ["always allow Bash(mkdir:*) in localSettings in session marsad (s1)"]);
        assert_eq!(r.logged.borrow().last().unwrap()["outcome"], "not-confirmed");
    }

    #[test]
    fn a_deny_needs_no_confirmation() {
        let dir = scratch("deny");
        let mut r = run_with(&dir, 120, vec![line(&Answer::Deny)], |_| {}, false, true);
        let out = handle(&mut r.env, &bash("rm -rf build")).unwrap();
        assert_eq!(out["hookSpecificOutput"]["decision"], json!({ "behavior": "deny", "message": DENY_MESSAGE }));
        assert!(r.asked.borrow().is_empty());
    }

    #[test]
    fn a_terminal_answer_a_timeout_a_bad_reply_or_a_changed_server_leaves_the_decision_to_the_terminal() {
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

        // A reply that does not fit the prompt, a closed connection, or a server process that
        // changed never allows, and no confirmation is asked.
        let no_rule = json!({ "session_id": "s1", "tool_name": "Write", "tool_input": { "file_path": "/w/a.rs", "content": "x" } });
        let mut r = run(&dir, 120, vec![line(&Answer::AlwaysAllow)], |_| {});
        assert_eq!(handle(&mut r.env, &no_rule), None);
        let mut r = run(&dir, 120, vec![Recv::Line(b"{\"answer\":\"allow\"".to_vec()), Recv::Closed], |_| {});
        assert_eq!(handle(&mut r.env, &bash("ls")), None);
        let mut r = run_with(&dir, 120, vec![line(&Answer::Allow)], |_| {}, true, false);
        assert_eq!(handle(&mut r.env, &bash("ls")), None);
        assert!(r.asked.borrow().is_empty());
    }

    #[test]
    fn without_a_dashboard_the_hook_does_nothing() {
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
        assert_eq!(*r.asked.borrow(), ["answer \"Pick a fruit\": Pear in session marsad (s1)"]);
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
    fn the_server_clamps_the_wait_and_lists_every_prompt_of_a_session() {
        let a = request_for(&bash("ls"), 10.0, 120).unwrap();
        let mut far = request_for(&bash("pwd"), 20.0, 120).unwrap();
        far.until = 1e12;
        let mut b = request_for(&ask(), 20.0, 120).unwrap();
        b.session = "s2".into();
        let (mut server, answers) = server::Server::for_tests(vec![a.clone(), far, b.clone()]);
        server.poll(30.0);
        assert_eq!(server.count(), 3);
        assert_eq!(server.waiting("s1").len(), 2);
        assert!(server.answer(&b, Answer::Allow).is_err(), "a question needs a choice");
        server.answer(&a, Answer::Deny).unwrap();
        assert_eq!(answers[0].try_recv().unwrap(), Answer::Deny);
        assert!(server.answer(&a, Answer::Deny).is_err(), "answered once");
        server.poll(30.0 + MAX_WAIT_SECS as f64 + 1.0);
        assert_eq!(server.count(), 0, "a claimed far deadline is cut to the longest wait");
    }

    #[test]
    fn writes_and_edits_show_their_content_and_text_is_cleaned() {
        let write = json!({ "session_id": "s1", "tool_name": "Write", "tool_input": { "file_path": "/w/a.rs", "content": "fn main() {}\n\u{1b}[2Jgone" } });
        let r = request_for(&write, 0.0, 120).unwrap();
        assert_eq!(r.summary, "writing /w/a.rs (2 lines)");
        assert!(r.detail.contains("fn main() {}") && !r.detail.contains('\u{1b}'), "{:?}", r.detail);
        let edit = json!({ "session_id": "s1", "tool_name": "Edit", "tool_input": { "file_path": "/w/a.rs", "old_string": "a\nb", "new_string": "c" } });
        let r = request_for(&edit, 0.0, 120).unwrap();
        assert_eq!((r.summary.as_str(), r.detail.as_str()), ("editing /w/a.rs (2 lines replaced by 1)", "- a\n- b\n+ c"));
        let plan = json!({ "session_id": "s1", "tool_name": "ExitPlanMode", "tool_input": { "plan": "x" } });
        assert_eq!(request_for(&plan, 0.0, 120), None);
        let bad_id = json!({ "session_id": "../s1", "tool_name": "Bash", "tool_input": { "command": "ls" } });
        assert_eq!(request_for(&bad_id, 0.0, 120), None);
    }

    #[test]
    fn previews_are_redacted_and_cut() {
        let secret = json!({ "session_id": "s1", "tool_name": "Bash", "tool_input": { "command": "curl -H 'Authorization: Bearer sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789'" } });
        let req = request_for(&secret, 0.0, 120).unwrap();
        assert!(!req.detail.contains("abcdefghijklmnopqrstuvwxyz0123456789") && !req.summary.contains("abcdefghijklmnopqrstuvwxyz0123456789"));
        let long = json!({ "session_id": "s1", "tool_name": "Bash", "tool_input": { "command": "x".repeat(MAX_DETAIL_CHARS + 10) } });
        let req = request_for(&long, 0.0, 120).unwrap();
        assert_eq!(req.detail.chars().count(), MAX_DETAIL_CHARS + 1);
        assert_eq!(req.summary.chars().count(), MAX_SUMMARY_CHARS + 1);
    }
}

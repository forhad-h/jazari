//! `jazari watch context`: how full a session's context is, so a session can decide to hand off before
//! auto-compact. With `--hook` it is also the UserPromptSubmit and PostToolUse hook that tells a
//! session, once per crossing, that its context passed the warning threshold and how to hand off.

use crate::model::{ContextInfo, Session, auto_compact_window, context_info, fmt_tokens};
use crate::report::Format;
use clap::Args;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct ContextArgs {
    /// Session id or id prefix (default: the session this command runs in).
    #[arg(long)]
    session: Option<String>,
    /// Set the automatic handoff warning: a percent of the auto-compact point, or `off`.
    #[arg(long)]
    warn: Option<String>,
    /// Set when a session must hand off: tool calls other than reading, searching and the handoff
    /// are refused from this percent of the auto-compact point. A percent, or `off`.
    #[arg(long)]
    block: Option<String>,
    /// Let this session (id or id prefix) keep working past the block until its next compact.
    /// Only for when the user asked the session to keep going.
    #[arg(long, value_name = "SESSION")]
    allow: Option<String>,
    #[arg(long, value_enum, default_value = "table")]
    format: Format,
    /// Hook entrypoint called by Claude Code on UserPromptSubmit and PostToolUse (reads hook JSON on stdin).
    #[arg(long, hide = true)]
    hook: bool,
}

pub fn run(a: ContextArgs) -> anyhow::Result<()> {
    if a.hook {
        // Never fail or print anything but the note: a hook must not disturb the session.
        if let Some(out) = hook() {
            println!("{out}");
        }
        return Ok(());
    }
    if let Some(warn) = a.warn {
        let pct = parse_pct(&warn)?;
        crate::settings::set_context_warning(pct)?;
        println!("{}", warning_label(pct));
        return Ok(());
    }
    if let Some(block) = a.block {
        let pct = parse_pct(&block)?;
        crate::settings::set_context_block(pct)?;
        println!("{}", block_label(pct));
        return Ok(());
    }
    if let Some(prefix) = a.allow {
        let sessions = crate::live::load_sessions(true);
        let s = find_session(&sessions, Some(&prefix), None, &[])?;
        let action = crate::act::Action::new(crate::act::Kind::Setting, "context block").detail(format!("allow {}", s.id));
        crate::act::run(&action, || allow(&allowed_dir(), &s.id))?;
        println!("Session {} can keep working past the context block until its next compact.", &s.id[..s.id.len().min(8)]);
        return Ok(());
    }

    let sessions = crate::live::load_sessions(true);
    let env_id = std::env::var("CLAUDE_CODE_SESSION_ID").ok();
    let session = find_session(&sessions, a.session.as_deref(), env_id.as_deref(), &ancestors())?;
    let ctx = session.context(auto_compact_window());
    let warn = crate::settings::context_warning();
    match a.format {
        Format::Json => {
            let out = json!({
                "session_id": session.id,
                "project": session.project(),
                "tokens_total": session.total_tokens().total(),
                "context_used": ctx.map(|c| c.used),
                "context_limit": ctx.map(|c| c.limit),
                "context_left": ctx.map(|c| c.left()),
                "context_pct": ctx.map(|c| (c.used_ratio() * 1000.0).round() / 10.0),
                "context_estimated": ctx.map(|c| c.estimated),
                "warn_pct": warn,
                "block_pct": crate::settings::context_block(),
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        _ => {
            let id: String = session.id.chars().take(8).collect();
            println!("Session {id} ({})", session.project());
            match ctx {
                Some(c) => println!(
                    "Context: {} of {} before auto-compact ({:.0}% full), {} left{}",
                    fmt_tokens(c.used),
                    fmt_tokens(c.limit),
                    c.used_ratio() * 100.0,
                    fmt_tokens(c.left()),
                    if c.estimated { " (window estimated)" } else { "" }
                ),
                None => println!("Context: unknown (no API call recorded yet)"),
            }
            println!("Tokens used: {} (main agent and subagents)", fmt_tokens(session.total_tokens().total()));
            println!("{}", warning_label(warn));
            println!("{}", block_label(crate::settings::context_block()));
        }
    }
    Ok(())
}

fn parse_pct(text: &str) -> anyhow::Result<Option<u32>> {
    Ok(match text {
        "off" | "0" => None,
        n => Some(n.trim_end_matches('%').parse::<u32>().ok().filter(|p| (1..=100).contains(p)).ok_or_else(|| {
            anyhow::anyhow!("expected a percent from 1 to 100, or `off`, got {n:?}")
        })?),
    })
}

fn block_label(pct: Option<u32>) -> String {
    match pct {
        Some(p) => format!("Handoff required at {p}%: other tool calls are refused (`jazari watch context --block <pct|off>`)."),
        None => "Handoff block is off (`jazari watch context --block 90` turns it on).".into(),
    }
}

fn warning_label(pct: Option<u32>) -> String {
    match pct {
        Some(p) => format!("Handoff warning at {p}% of the auto-compact point (`jazari watch context --warn <pct|off>`)."),
        None => "Handoff warning is off (`jazari watch context --warn 80` turns it on).".into(),
    }
}

/// This process and its ancestors, nearest first.
pub(crate) fn ancestors() -> Vec<u32> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    let mut out = vec![std::process::id()];
    let mut pid = Pid::from_u32(std::process::id());
    for _ in 0..12 {
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing());
        let Some(parent) = sys.process(pid).and_then(|p| p.parent()) else { break };
        out.push(parent.as_u32());
        pid = parent;
    }
    if let Some(claude) = std::env::var("CLAUDE_PID").ok().and_then(|p| p.parse().ok()) {
        out.push(claude);
    }
    out
}

/// The session to report: an explicit id, then Claude Code's session id variable, then the
/// session whose Claude Code process is an ancestor of this one.
pub(crate) fn find_session<'a>(
    sessions: &'a HashMap<String, Session>,
    explicit: Option<&str>,
    env_id: Option<&str>,
    ancestors: &[u32],
) -> anyhow::Result<&'a Session> {
    let latest = |hits: &mut dyn Iterator<Item = &'a Session>| hits.max_by(|a, b| a.last_event.total_cmp(&b.last_event));
    let by_prefix = |id: &str| latest(&mut sessions.values().filter(|s| s.id.starts_with(id)));
    if let Some(id) = explicit {
        return by_prefix(id).ok_or_else(|| anyhow::anyhow!("no session matches {id:?}"));
    }
    if let Some(s) = env_id.filter(|id| !id.is_empty()).and_then(by_prefix) {
        return Ok(s);
    }
    // The nearest ancestor wins, so a nested `claude` reports itself rather than its parent.
    for pid in ancestors {
        if let Some(s) = latest(&mut sessions.values().filter(|s| s.pid == Some(*pid))) {
            return Ok(s);
        }
    }
    anyhow::bail!("not running inside a Claude Code session jazari knows; pass --session <id>")
}

// ---------- the UserPromptSubmit hook ----------

/// Markers for sessions already warned since they last crossed the threshold.
fn warned_dir() -> PathBuf {
    crate::claude_data_dir().join("context-warned")
}

/// Markers older than this belong to sessions long gone.
const MARKER_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30 * 86_400);

/// Reads the hook input and returns the JSON to print when the session newly crossed the threshold.
fn hook() -> Option<String> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).ok()?;
    let raw: Value = serde_json::from_str(&input).ok()?;
    if from_subagent(&raw) {
        return None;
    }
    let threshold = crate::settings::context_warning()?;
    // Only these events let a hook add text to the session's context.
    let event = raw["hook_event_name"].as_str().filter(|e| matches!(*e, "UserPromptSubmit" | "PostToolUse")).unwrap_or("UserPromptSubmit");
    let id = raw["session_id"].as_str().filter(|id| !id.is_empty() && !id.contains(['/', '\\', '.']))?;
    let pct = session_pct(&raw, id)?;

    let dir = warned_dir();
    let marker = dir.join(id);
    let was_warned = marker.exists();
    let (emit, warned) = decide(pct, threshold, was_warned);
    if warned && !was_warned {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(&marker, b"").ok()?;
        clean_markers(&dir);
    } else if was_warned && !warned {
        let _ = std::fs::remove_file(&marker);
    }
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "jazari".into());
    emit.then(|| {
        json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "additionalContext": crate::handoff::context_note(pct, &exe, crate::handoff::auto_close(), crate::settings::context_block()),
            }
        })
        .to_string()
    })
}

/// How full the session's context is, in percent of the auto-compact point, from the status line
/// report or the transcript, whichever is newer.
fn session_pct(raw: &Value, id: &str) -> Option<f64> {
    let snapshot = crate::statusline::load(&crate::statusline::status_dir().join(format!("{id}.json")));
    let tail = raw["transcript_path"].as_str().and_then(|p| last_context(Path::new(p)));
    Some(fresher_context(snapshot.as_ref(), tail.as_ref())?.used_ratio() * 100.0)
}

// ---------- the block before auto-compact ----------

/// Sessions the user let keep working past the block. A marker is removed once the session's
/// context drops below the block again, which happens at its next compact or `/clear`.
fn allowed_dir() -> PathBuf {
    crate::claude_data_dir().join("context-allowed")
}

fn allow(dir: &Path, id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!id.is_empty() && !id.contains(['/', '\\', '.']), "not a session id: {id:?}");
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(id), b"")?;
    Ok(())
}

/// Tools that only read or ask, so a session can still gather what its handoff needs.
const READ_ONLY_TOOLS: &[&str] = &["Read", "Grep", "Glob", "LS", "NotebookRead", "TodoWrite", "ToolSearch", "AskUserQuestion", "Skill"];

/// Whether this tool call may run in a session that is past the block: a read-only tool, the
/// handoff itself, or a jazari command that only reads (`jazari watch context` and the like).
fn allowed_past_block(raw: &Value) -> bool {
    let Some(tool) = raw["tool_name"].as_str() else { return false };
    if READ_ONLY_TOOLS.contains(&tool) {
        return true;
    }
    if tool != "Bash" {
        return false;
    }
    let Some(cmd) = raw["tool_input"]["command"].as_str() else { return false };
    is_handoff(cmd) || is_jazari_read(cmd) || is_read_only_git(cmd)
}

/// `jazari build handoff [flags] <<'EOF'` followed by the prompt and the closing `EOF`. The
/// delimiter must be quoted, so the shell runs nothing inside the prompt, and nothing may follow
/// the closing line.
pub(crate) fn is_handoff(cmd: &str) -> bool {
    let cmd = cmd.trim_end();
    let (first, rest) = cmd.split_once('\n').unwrap_or((cmd, ""));
    let Some((head, delim)) = first.split_once("<<") else { return false };
    let delim = delim.trim();
    let quoted = ['\'', '"'].iter().find_map(|q| delim.strip_prefix(*q).and_then(|d| d.strip_suffix(*q)));
    let Some(word) = quoted.filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) else { return false };
    handoff_head(head) && rest.lines().last().is_some_and(|l| l == word) && rest.lines().filter(|l| *l == word).count() == 1
}

/// `<jazari> build handoff` with only its own flags, and no other shell syntax.
fn handoff_head(head: &str) -> bool {
    if head.contains([';', '&', '|', '`', '$', '<', '>', '(', ')']) {
        return false;
    }
    let Some((_, mut words)) = jazari_words(head) else { return false };
    words.next() == Some("build") && words.next() == Some("handoff") && words.all(|w| matches!(w, "--close" | "--keep" | "--no-launch"))
}

/// The words after the program, when `cmd` starts with jazari: a bare name or path, or a path in
/// quotes (`'/opt/my tools/jazari'`, as the notes print it).
fn jazari_words(cmd: &str) -> Option<(&str, std::str::SplitWhitespace<'_>)> {
    let cmd = cmd.trim_start();
    let (exe, rest) = match cmd.chars().next()? {
        q @ ('\'' | '"') => {
            let end = cmd[1..].find(q)? + 1;
            (&cmd[1..end], &cmd[end + 1..])
        }
        _ => cmd.split_once(char::is_whitespace).unwrap_or((cmd, "")),
    };
    Path::new(exe).file_stem().is_some_and(|n| n == "jazari").then(|| (exe, rest.split_whitespace()))
}

/// A jazari command that only reads, or `jazari watch context --allow` (which asks the user
/// first): `jazari watch context|status|system|say`, with no other shell syntax.
fn is_jazari_read(cmd: &str) -> bool {
    if cmd.contains([';', '&', '|', '`', '$', '<', '>', '(', ')', '\n']) {
        return false;
    }
    let Some((_, mut words)) = jazari_words(cmd) else { return false };
    let sub = match words.next() {
        Some("watch") => words.next(),
        other => other,
    };
    match sub {
        Some("context") => !cmd.contains("--warn") && !cmd.contains("--block"),
        Some("status" | "system" | "say") => true,
        _ => false,
    }
}

/// `git status`, `git log` or `git diff` with no shell syntax, so the handoff can list the
/// changed files.
fn is_read_only_git(cmd: &str) -> bool {
    if cmd.contains([';', '&', '|', '`', '$', '<', '>', '(', ')', '\n']) || cmd.contains("--output") {
        return false;
    }
    let mut words = cmd.split_whitespace();
    words.next() == Some("git") && matches!(words.next(), Some("status" | "log" | "diff"))
}

/// The guard hook's context check before a tool call: the reason to refuse it, when the
/// session's context is past the block and the call isn't one a handoff needs. A subagent's call
/// is never refused, because its context is its own.
pub fn block_check(raw: &Value, id: &str) -> Option<String> {
    if from_subagent(raw) {
        return None;
    }
    let block = crate::settings::context_block()?;
    let pct = session_pct(raw, id)?;
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "jazari".into());
    block_decision(raw, id, pct, block, &allowed_dir(), &exe)
}

fn block_decision(raw: &Value, id: &str, pct: f64, block: u32, dir: &Path, exe: &str) -> Option<String> {
    let marker = dir.join(id);
    if pct < block as f64 {
        // Below the block again (a compact or `/clear`): a later crossing blocks again.
        let _ = std::fs::remove_file(&marker);
        return None;
    }
    if marker.exists() || allowed_past_block(raw) {
        return None;
    }
    let jazari = crate::terminal::sh_quote(exe);
    Some(format!(
        "[jazari] Refused: this session's context is {pct:.0}% full, and jazari requires a handoff from \
         {block}% so no work is lost at auto-compact. Don't retry this call. Do this next:\n\
         1. Write a handoff prompt that a fresh session can start from with no other context: the goal; \
         what is done; what is left, with the next step first; decisions and why; files changed; dead \
         ends; how to check the work.\n\
         2. Run it as one Bash call: `{jazari} build handoff <<'EOF'`, then the prompt, then a line with \
         only `EOF`. jazari opens a new session from it.\n\
         Reading and searching files, `git status`, `git diff`, `git log` and `jazari watch context` \
         still work. Only if the user told you to keep going in this session, run \
         `{jazari} watch context --allow {id}`. It asks the user first, and it lasts until the next compact."
    ))
}

/// A subagent's tool calls carry the parent's session id and their own `agent_id`. Only the main
/// agent may take the once-per-crossing warning: a subagent would use it up, and the handoff it is
/// told to run would end the main session.
fn from_subagent(raw: &Value) -> bool {
    raw.get("agent_id").is_some_and(|a| !a.is_null())
}

/// (whether to warn now, whether the session counts as warned afterwards). Dropping below the
/// threshold, for example after a compact or `/clear`, re-arms the warning.
fn decide(pct: f64, threshold: u32, warned: bool) -> (bool, bool) {
    let over = pct >= threshold as f64;
    (over && !warned, over)
}

fn clean_markers(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > MARKER_MAX_AGE);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Context fill from the latest main-agent API call in the transcript, found by reading only its tail.
struct TailContext {
    used: u64,
    at: f64,
    model: Option<String>,
}

const TAIL_BYTES: u64 = 512 * 1024;

fn last_context(path: &Path) -> Option<TailContext> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    // The first line may be cut in half when reading from the middle of the file.
    let text = if start > 0 { text.split_once('\n').map_or("", |(_, rest)| rest) } else { &text };
    text.lines().rev().find_map(|line| {
        // Skips parsing the many lines that can't carry usage.
        if !line.contains("\"usage\"") {
            return None;
        }
        let e: Value = serde_json::from_str(line).ok()?;
        if e["type"] != "assistant" || e["isSidechain"].as_bool().unwrap_or(false) {
            return None;
        }
        let u = crate::transcript::parse_usage(&e["message"]["usage"]);
        let used = u.input + u.cache_read + u.cache_write;
        (used > 0).then(|| TailContext {
            used,
            at: crate::transcript::unix_ts(&e).unwrap_or(0.0),
            model: e["message"]["model"].as_str().map(str::to_string),
        })
    })
}

/// Whichever of the status line snapshot and the transcript reported more recently, as the
/// dashboard decides.
fn fresher_context(snap: Option<&crate::model::StatusSnapshot>, tail: Option<&TailContext>) -> Option<ContextInfo> {
    let used = crate::model::fresher_used(tail.map(|t| (t.used, t.at)), snap)?;
    let model = snap.and_then(|s| s.model.clone()).or_else(|| tail.and_then(|t| t.model.clone()));
    Some(context_info(used, model.as_deref(), snap.and_then(|s| s.window), auto_compact_window()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StatusSnapshot;

    #[test]
    fn subagent_calls_never_take_the_warning() {
        assert!(from_subagent(&json!({ "session_id": "s", "agent_id": "a1", "hook_event_name": "PostToolUse" })));
        assert!(!from_subagent(&json!({ "session_id": "s", "hook_event_name": "PostToolUse" })));
        assert!(!from_subagent(&json!({ "session_id": "s", "agent_id": null })));
    }

    #[test]
    fn warns_once_per_crossing() {
        // Covers AE4.
        let mut warned = false;
        let mut notes = Vec::new();
        for pct in [78.0, 83.0, 90.0, 20.0, 85.0] {
            let (emit, now) = decide(pct, 80, warned);
            warned = now;
            if emit {
                notes.push(pct);
            }
        }
        assert_eq!(notes, [83.0, 85.0]);
    }

    fn session(id: &str, pid: Option<u32>, last: f64) -> Session {
        let mut s = Session::new(id, 0.0);
        s.pid = pid;
        s.last_event = last;
        s
    }

    #[test]
    fn finds_the_session_it_runs_in() {
        let sessions: HashMap<String, Session> = [
            session("aaaa1111", Some(42), 1.0),
            session("bbbb2222", Some(7), 2.0),
            session("bbbb3333", None, 3.0),
        ]
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();
        let find = |explicit, env, pids: &[u32]| find_session(&sessions, explicit, env, pids).map(|s| s.id.clone());
        assert_eq!(find(None, None, &[100, 42, 1]).unwrap(), "aaaa1111", "by an ancestor pid");
        assert_eq!(find(Some("bbbb2"), None, &[42]).unwrap(), "bbbb2222", "an explicit id wins");
        assert_eq!(find(None, Some("bbbb3333"), &[42]).unwrap(), "bbbb3333", "then Claude Code's session id");
        assert_eq!(find(Some("bbbb"), None, &[]).unwrap(), "bbbb3333", "a prefix picks the latest");
        let err = find(Some("zzz"), None, &[]).unwrap_err().to_string();
        assert!(err.contains("zzz"), "{err}");
        assert!(find(None, None, &[1, 2]).is_err());
    }

    #[test]
    fn hook_reads_the_fresher_context_source() {
        let snap = |used, ts| StatusSnapshot { ts, used: Some(used), window: Some(200_000), ..Default::default() };
        let tail = |used, at| TailContext { used, at, model: None };
        let pct = |c: Option<ContextInfo>| (c.unwrap().used_ratio() * 100.0).round();
        // An older snapshot above the threshold loses to a newer, post-compact transcript entry.
        assert_eq!(pct(fresher_context(Some(&snap(170_000, 10.0)), Some(&tail(40_000, 20.0)))), 20.0);
        assert_eq!(pct(fresher_context(Some(&snap(170_000, 30.0)), Some(&tail(40_000, 20.0)))), 85.0);
        assert!(fresher_context(None, None).is_none());
    }

    #[test]
    fn transcript_tail_finds_the_latest_main_agent_call() {
        let dir = std::env::temp_dir().join(format!("jazari-context-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.jsonl");
        let call = |used: u64, side: bool| {
            format!(
                r#"{{"type":"assistant","isSidechain":{side},"timestamp":"2026-10-23T10:00:00Z","message":{{"model":"claude-opus-5-5","usage":{{"input_tokens":{used}}}}}}}"#
            )
        };
        // Padding pushes the first call out of the tail window.
        let padding = format!("{{\"type\":\"user\",\"message\":{{\"content\":\"{}\"}}}}", "x".repeat(TAIL_BYTES as usize));
        let text = [call(1, false), padding, call(150_000, false), call(9, true), "{\"type\":".into()].join("\n");
        std::fs::write(&file, text).unwrap();
        let got = last_context(&file).unwrap();
        assert_eq!((got.used, got.model.as_deref()), (150_000, Some("claude-opus-5-5")));
        assert!(last_context(&dir.join("missing.jsonl")).is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    fn bash(cmd: &str) -> Value {
        json!({ "session_id": "s1", "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": { "command": cmd } })
    }

    #[test]
    fn only_a_quoted_heredoc_handoff_counts_as_the_handoff() {
        assert!(is_handoff("jazari build handoff <<'EOF'\nGoal: x\nrun `rm -rf /` later\nEOF"));
        assert!(is_handoff("'/opt/my tools/jazari' build handoff --keep <<\"END_1\"\nhi\nEND_1\n"));
        for bad in [
            "jazari build handoff <<EOF\n$(touch /tmp/x)\nEOF",           // unquoted: the shell expands it
            "jazari build handoff <<'EOF'\nhi\nEOF\nrm -rf ~",           // a command after the prompt
            "jazari build handoff <<'EOF'\nEOF\nmore\nEOF",              // the delimiter twice
            "jazari build handoff; rm x <<'EOF'\nhi\nEOF",               // shell syntax in the first line
            "jazari build handoff --session other <<'EOF'\nhi\nEOF",     // another session's handoff
            "evil build handoff <<'EOF'\nhi\nEOF",
            "jazari build handoff --file p.md",
        ] {
            assert!(!is_handoff(bad), "{bad}");
        }
    }

    #[test]
    fn past_the_block_only_reading_and_the_handoff_run() {
        let dir = std::env::temp_dir().join(format!("jazari-allowed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let edit = json!({ "tool_name": "Edit", "tool_input": { "file_path": "/w/a.rs" } });
        let read = json!({ "tool_name": "Read", "tool_input": { "file_path": "/w/a.rs" } });
        let reason = block_decision(&edit, "s1", 91.0, 90, &dir, "/opt/jazari").unwrap();
        assert!(reason.contains("91% full") && reason.contains("Don't retry this call"), "{reason}");
        assert!(reason.contains("`/opt/jazari build handoff <<'EOF'`"), "the next step is spelled out: {reason}");
        assert!(reason.contains("`/opt/jazari watch context --allow s1`"), "{reason}");
        assert_eq!(block_decision(&edit, "s1", 89.9, 90, &dir, "/opt/jazari"), None, "below the block");
        assert_eq!(block_decision(&read, "s1", 99.0, 90, &dir, "/opt/jazari"), None);
        for ok in ["jazari build handoff <<'EOF'\nhi\nEOF", "jazari watch context", "git status", "git diff --stat"] {
            assert_eq!(block_decision(&bash(ok), "s1", 95.0, 90, &dir, "jazari"), None, "{ok}");
        }
        assert_eq!(block_decision(&bash("'/opt/my tools/jazari' watch context --allow s1"), "s1", 95.0, 90, &dir, "jazari"), None);
        for no in ["cargo build", "jazari watch context --block off", "git commit -m x", "git diff --output=x", "git status; rm x", "jazari watch clean --run"] {
            assert!(block_decision(&bash(no), "s1", 95.0, 90, &dir, "jazari").is_some(), "{no}");
        }
    }

    #[test]
    fn allow_lasts_until_the_context_drops_below_the_block() {
        let dir = std::env::temp_dir().join(format!("jazari-allow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let edit = json!({ "tool_name": "Edit", "tool_input": { "file_path": "/w/a.rs" } });
        allow(&dir, "s1").unwrap();
        assert_eq!(block_decision(&edit, "s1", 93.0, 90, &dir, "jazari"), None, "allowed");
        assert!(block_decision(&edit, "s2", 93.0, 90, &dir, "jazari").is_some(), "only that session");
        assert_eq!(block_decision(&edit, "s1", 20.0, 90, &dir, "jazari"), None, "after a compact");
        assert!(block_decision(&edit, "s1", 93.0, 90, &dir, "jazari").is_some(), "the next crossing blocks again");
        assert!(allow(&dir, "../x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_subagent_is_never_blocked() {
        let raw = json!({ "session_id": "s1", "agent_id": "a1", "tool_name": "Edit" });
        assert_eq!(block_check(&raw, "s1"), None);
    }
}

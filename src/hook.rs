//! `jazari hook`: called by Claude Code for every registered event.
//! Keeps only the fields the dashboard needs and appends one JSON line.

use serde_json::{Map, Value, json};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::process::Stdio;

/// Rotate the log once it grows past this size.
const MAX_LOG_BYTES: u64 = 20 * 1024 * 1024;

const KEEP: &[&str] = &[
    "session_id",
    "hook_event_name",
    "cwd",
    "transcript_path",
    "permission_mode",
    "agent_id",
    "agent_type",
    "source",
    "reason",
    "model",
    "session_title",
    "tool_name",
    "notification_type",
];

pub fn run() -> anyhow::Result<()> {
    // jazari's own `claude -p` run that writes a prompt: not a session to show or count.
    if std::env::var_os(crate::prompt_gen::MARKER_ENV).is_some() {
        return Ok(());
    }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let raw: Value = serde_json::from_str(&input)?;
    let event = trim(&raw);

    let mut line = serde_json::to_vec(&event)?;
    line.push(b'\n');
    append(&crate::claude_data_dir(), &line)?;

    // Record a finished session in the usage history right away, so it is kept even if
    // the dashboard isn't opened before Claude Code deletes the transcript. Detached, so
    // the hook returns at once and the sync survives Claude Code exiting.
    match raw.get("hook_event_name").and_then(Value::as_str) {
        Some("SessionEnd") => {
            let _ = detached(std::process::Command::new(std::env::current_exe()?).args(["watch", "sync"])).spawn();
            after_task(&raw);
        }
        Some("Stop") => after_task(&raw),
        Some("SessionStart") => {
            // The disk watcher runs while any session does.
            crate::watch::ensure_running();
            // Install a ticked skill that is missing and bring installed copies up to date. This
            // hook runs async; Claude Code picks up a skill folder that appears after it started.
            let cwd = raw.get("cwd").and_then(Value::as_str).map(std::path::Path::new);
            let _ = crate::skills::sync_on_start(cwd, &crate::settings::rules());
            // Add the hooks a newer release registers, for users who never run `jazari update`.
            // Nothing changes when they are already there, or when jazari isn't installed.
            let a = crate::act::Action::new(crate::act::Kind::Install, "~/.claude/settings.json").detail("refresh hooks").on_its_own(true);
            // Only a change is recorded: most session starts find the hooks already current.
            if crate::act::check(&a).is_ok() {
                match crate::install::refresh(None) {
                    Ok(crate::install::Refresh::Updated) => crate::act::record(&a, "done", ""),
                    // A development build or a scratch run: say why the real settings stayed as they are.
                    Ok(crate::install::Refresh::Skipped(why)) => crate::act::record(&a, "kept", &why),
                    Err(e) => crate::act::record(&a, "failed", &format!("{e:#}")),
                    Ok(_) => {}
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// When a turn or session ends, the work it did may have filled the disk: start the automatic
/// cleanup if the disk is at the warning level (rate-limited inside `maybe_start`).
fn after_task(raw: &Value) {
    let cfg = crate::disk::Config::load();
    let home = dirs::home_dir();
    let cwd = raw.get("cwd").and_then(Value::as_str).map(std::path::Path::new);
    if let Some((level, _)) = crate::disk::worst(cwd.into_iter().chain(home.as_deref()), &cfg.thresholds, crate::disk::measure) {
        crate::clean::maybe_start(level);
    }
}

/// Appends one event line to the log in `dir`, rotating it once it is too big.
pub(crate) fn append(dir: &std::path::Path, line: &[u8]) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let log = dir.join("events.jsonl");
    if std::fs::metadata(&log).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&log, dir.join("events.1.jsonl"));
    }
    // Single write on an O_APPEND handle so concurrent hooks don't interleave.
    OpenOptions::new().create(true).append(true).open(log)?.write_all(line)?;
    Ok(())
}

/// No stdio and its own process group, so Claude Code cleaning up its hooks on exit
/// doesn't take the child with it (and on Windows, no console window).
pub(crate) fn detached(cmd: &mut std::process::Command) -> &mut std::process::Command {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(cmd, 0);
    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        std::os::windows::process::CommandExt::creation_flags(cmd, DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd
}

fn trim(raw: &Value) -> Value {
    let mut out = Map::new();
    out.insert("ts".into(), json!(crate::now()));
    for key in KEEP {
        if let Some(v) = raw.get(*key).filter(|v| !v.is_null()) {
            out.insert((*key).into(), v.clone());
        }
    }
    for key in ["message", "prompt", "error"] {
        if let Some(s) = raw.get(key).and_then(Value::as_str) {
            out.insert(key.into(), json!(truncate(s, 160)));
        }
    }
    if let Some(tool) = raw.get("tool_name").and_then(Value::as_str) {
        let summary = tool_summary(tool, raw.get("tool_input").unwrap_or(&Value::Null));
        if !summary.is_empty() {
            out.insert("summary".into(), json!(summary));
        }
    }
    // Claude Code passes its pid to hooks; walking the process tree is the fallback.
    let event = raw.get("hook_event_name").and_then(Value::as_str).unwrap_or("");
    let pid = std::env::var("CLAUDE_PID").ok().and_then(|p| p.parse::<u32>().ok()).or_else(|| {
        matches!(event, "SessionStart" | "UserPromptSubmit").then(claude_pid).flatten()
    });
    if let Some(pid) = pid {
        out.insert("pid".into(), json!(pid));
    }
    Value::Object(out)
}

/// A short human description of what a tool call is doing.
pub fn tool_summary(tool: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("");
    let text = match tool {
        "Bash" => s("command").lines().next().unwrap_or("").to_string(),
        "Read" | "Edit" | "Write" | "MultiEdit" => file_name(s("file_path")),
        "NotebookEdit" => file_name(s("notebook_path")),
        "Grep" | "Glob" => s("pattern").to_string(),
        "Skill" => s("skill").to_string(),
        "Agent" | "Task" => format!("{} {}", s("subagent_type"), s("description")).trim().to_string(),
        "WebFetch" => s("url").to_string(),
        "WebSearch" => s("query").to_string(),
        "AskUserQuestion" => input
            .pointer("/questions/0/question")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    };
    truncate(&text, 120)
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// The Claude Code process that ran this hook: the first ancestor that isn't a shell.
fn claude_pid() -> Option<u32> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    let mut pid = Pid::from_u32(std::process::id());
    for _ in 0..6 {
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing());
        let parent = sys.process(pid)?.parent()?;
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[parent]), true, ProcessRefreshKind::nothing());
        let name = sys.process(parent)?.name().to_string_lossy().to_lowercase();
        let name = name.trim_start_matches('-').trim_end_matches(".exe");
        if !matches!(name, "sh" | "bash" | "zsh" | "dash" | "fish" | "cmd" | "powershell" | "pwsh") {
            return Some(parent.as_u32());
        }
        pid = parent;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_carries_the_claude_pid() {
        // SAFETY: no other test reads or writes CLAUDE_PID.
        unsafe { std::env::set_var("CLAUDE_PID", "4242") };
        let ev = trim(&json!({"session_id": "s", "hook_event_name": "PreToolUse", "tool_name": "Bash"}));
        assert_eq!(ev["pid"], 4242);
        unsafe { std::env::set_var("CLAUDE_PID", "not a pid") };
        let ev = trim(&json!({"session_id": "s", "hook_event_name": "PreToolUse"}));
        assert!(ev.get("pid").is_none());
        unsafe { std::env::remove_var("CLAUDE_PID") };
    }
}

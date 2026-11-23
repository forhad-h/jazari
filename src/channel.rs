//! Talking to running sessions, and hearing from them.
//!
//! marsad → session: `marsad tell` (or `t` in the dashboard) queues a message in the session's
//! inbox; the synchronous guard hook hands it to Claude at the session's next tool call or prompt,
//! as context it reads before going on. A stop request (`--stop`, `x`) refuses the next tool call
//! and ends the turn, with the session saved for `claude --resume`; `--interrupt` (`X`) sends
//! SIGINT now, as Esc does. Every step is logged, so the dashboard shows what is waiting.
//!
//! session → marsad: `marsad say` posts what the session is doing; with `--attention` it also
//! marks the session as needing the user, which sounds and notifies like a question.

use clap::Args;
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;

/// A message or stop request was queued for a session.
pub const TOLD_EVENT: &str = "MarsadTold";
/// The hook handed a session its queued messages.
pub const DELIVERED_EVENT: &str = "MarsadDelivered";
/// A session posted a status line.
pub const SAID_EVENT: &str = "MarsadSaid";

fn inbox_dir() -> PathBuf {
    crate::claude_data_dir().join("inbox")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && !id.contains(['/', '\\', '.'])
}

fn log(ev: Value) {
    if let Ok(mut line) = serde_json::to_vec(&ev) {
        line.push(b'\n');
        let _ = crate::hook::append(&crate::claude_data_dir(), &line);
    }
}

/// Queues `text` (a message) or a stop request for the session.
fn queue(session: &str, kind: &str, text: &str) -> anyhow::Result<()> {
    anyhow::ensure!(valid_id(session), "not a session id: {session:?}");
    std::fs::create_dir_all(inbox_dir())?;
    let mut line = serde_json::to_vec(&json!({ "ts": crate::now(), "kind": kind, "text": text }))?;
    line.push(b'\n');
    std::fs::OpenOptions::new().create(true).append(true).open(inbox_dir().join(format!("{session}.jsonl")))?.write_all(&line)?;
    log(json!({ "ts": crate::now(), "session_id": session, "hook_event_name": TOLD_EVENT, "kind": kind, "message": text }));
    Ok(())
}

pub fn tell(session: &str, text: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!text.trim().is_empty(), "the message is empty");
    queue(session, "message", text.trim())
}

pub fn request_stop(session: &str) -> anyhow::Result<()> {
    queue(session, "stop", "")
}

/// Sends SIGINT to the session's Claude Code process now. False when the pid is unknown or no
/// longer that session's process.
pub fn interrupt(s: &crate::model::Session) -> bool {
    let Some(pid) = s.pid else { return false };
    let sent = crate::watch::interrupt_session(pid, s.run_started.max(s.started));
    if sent {
        log(json!({
            "ts": crate::now(),
            "session_id": s.id,
            "hook_event_name": crate::disk::HALTED_EVENT,
            "reason": "interrupted from marsad",
        }));
    }
    sent
}

/// What the hook hands a session: the queued messages, and whether to stop.
#[derive(Debug, Default, PartialEq)]
pub struct Mail {
    pub notes: Vec<String>,
    pub stop: bool,
    /// A stop that was still waiting when the user sent a prompt: they are there and chose to go
    /// on, so it is dropped rather than cutting the new turn short.
    pub cancelled_stop: bool,
}

/// What this event delivers: messages at either event; a stop at a tool call, while a prompt
/// drops it (the user is there and chose to go on).
fn sort(lines: &str, event: &str) -> Mail {
    let mut mail = Mail::default();
    for line in lines.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        match v["kind"].as_str() {
            Some("stop") if event == "PreToolUse" => mail.stop = true,
            Some("stop") => mail.cancelled_stop = true,
            Some("message") => mail.notes.extend(v["text"].as_str().map(str::to_string)),
            _ => {}
        }
    }
    mail
}

/// Takes the session's queued mail for this event. The inbox is renamed away first, so a message
/// queued meanwhile lands in a fresh file and is never lost.
pub fn take(session: &str, event: &str) -> Mail {
    if !valid_id(session) || !matches!(event, "PreToolUse" | "UserPromptSubmit") {
        return Mail::default();
    }
    let file = inbox_dir().join(format!("{session}.jsonl"));
    let taking = inbox_dir().join(format!("{session}.{}.taking", std::process::id()));
    if std::fs::rename(&file, &taking).is_err() {
        return Mail::default();
    }
    let text = std::fs::read_to_string(&taking).unwrap_or_default();
    let _ = std::fs::remove_file(&taking);
    let mail = sort(&text, event);
    if !mail.notes.is_empty() || mail.stop || mail.cancelled_stop {
        // `stop` clears the dashboard's pending stop either way.
        log(json!({
            "ts": crate::now(),
            "session_id": session,
            "hook_event_name": DELIVERED_EVENT,
            "messages": mail.notes.len(),
            "stop": mail.stop || mail.cancelled_stop,
        }));
    }
    mail
}

/// The context Claude reads for delivered messages.
pub fn note(messages: &[String]) -> Option<String> {
    (!messages.is_empty()).then(|| {
        let list: Vec<String> = messages.iter().map(|m| format!("- {m}")).collect();
        format!(
            "[marsad] The user sent this session a message through marsad, the dashboard that watches all their Claude Code \
             sessions. Read it and act on it before going on:\n{}\nIf it needs an answer and the user may not be watching this \
             session, also post a short reply with `marsad say \"...\"`.",
            list.join("\n")
        )
    })
}

/// Marks the session halted, so the dashboard shows it stopped and how to resume it.
pub fn log_stopped(session: &str) {
    log(json!({ "ts": crate::now(), "session_id": session, "hook_event_name": crate::disk::HALTED_EVENT, "reason": "stopped from marsad" }));
}

/// What the user and Claude see when a stop request ends the turn.
pub fn stop_texts(session: &str) -> (String, String) {
    (
        format!("Stopped from marsad before the next tool call. Resume with `claude --resume {session}`."),
        "[marsad] The user stopped this session from marsad. Stop now: don't start anything new, and give a short summary of \
         where the work stands."
            .to_string(),
    )
}

// ---------- marsad tell / marsad say ----------

#[derive(Args)]
pub struct TellArgs {
    /// Session id or id prefix (`marsad status` lists them).
    session: String,
    /// The message for Claude in that session.
    message: Vec<String>,
    /// Stop the session before its next tool call (saved; `claude --resume` continues it).
    #[arg(long, conflicts_with_all = ["message", "interrupt"])]
    stop: bool,
    /// Interrupt the session now, as Esc does.
    #[arg(long, conflicts_with = "message")]
    interrupt: bool,
}

pub fn run_tell(a: TellArgs) -> anyhow::Result<()> {
    let sessions = crate::live::load_sessions(false);
    let mut hits: Vec<&crate::model::Session> = sessions.values().filter(|s| s.id.starts_with(&a.session)).collect();
    hits.sort_by(|x, y| y.last_event.total_cmp(&x.last_event));
    let s = match hits.as_slice() {
        [] => anyhow::bail!("no running session matches {:?}", a.session),
        [one] => *one,
        _ => anyhow::bail!("{:?} matches {} sessions; give more of the id", a.session, hits.len()),
    };
    let name = s.project();
    if a.interrupt {
        anyhow::ensure!(interrupt(s), "couldn't interrupt {name}: its Claude Code process isn't known or has changed");
        println!("Interrupted {name} ({}).", s.id);
    } else if a.stop {
        request_stop(&s.id)?;
        println!("{name} stops before its next tool call; `claude --resume {}` continues it.", s.id);
    } else {
        tell(&s.id, &a.message.join(" "))?;
        println!("Queued for {name}: Claude reads it at the session's next tool call or prompt.");
    }
    Ok(())
}

#[derive(Args)]
pub struct SayArgs {
    /// What this session is doing or needs, shown on it in the dashboard.
    message: Vec<String>,
    /// Also mark the session as needing the user (sound and notification).
    #[arg(long)]
    attention: bool,
    /// Session id or prefix (default: the session this runs in).
    #[arg(long)]
    session: Option<String>,
}

pub fn run_say(a: SayArgs) -> anyhow::Result<()> {
    let text = a.message.join(" ");
    anyhow::ensure!(!text.trim().is_empty(), "say what the session is doing");
    let sessions = crate::live::load_sessions(true);
    let env_id = std::env::var("CLAUDE_CODE_SESSION_ID").ok();
    let s = crate::context::find_session(&sessions, a.session.as_deref(), env_id.as_deref(), &crate::context::ancestors())?;
    log(json!({
        "ts": crate::now(),
        "session_id": s.id,
        "hook_event_name": SAID_EVENT,
        "message": crate::hook::truncate(&text, 300),
        "attention": a.attention,
    }));
    println!("Posted to marsad for {}.", s.project());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_ends_the_turn_at_a_tool_call_and_a_prompt_drops_it() {
        let lines = [
            r#"{"kind":"message","text":"use the staging db"}"#,
            r#"{"kind":"stop","text":""}"#,
            "not json",
            r#"{"kind":"message","text":"then push"}"#,
        ]
        .join("\n");
        let mail = sort(&lines, "UserPromptSubmit");
        assert_eq!(mail, Mail { notes: vec!["use the staging db".into(), "then push".into()], stop: false, cancelled_stop: true });
        let mail = sort(&lines, "PreToolUse");
        assert!(mail.stop && !mail.cancelled_stop);
    }

    #[test]
    fn the_note_lists_each_message_and_how_to_answer() {
        assert_eq!(note(&[]), None);
        let n = note(&["check the logs".into()]).unwrap();
        assert!(n.contains("- check the logs") && n.contains("marsad say"), "{n}");
        assert!(!valid_id("../etc") && valid_id("1fe6ab"));
    }
}

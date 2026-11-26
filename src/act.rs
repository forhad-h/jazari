//! The gate every action jazari takes outside its own reports passes through: messages, stops and
//! interrupts sent to sessions, ending a session after a handoff, cleanups, setting changes and
//! updates.
//!
//! Each action is checked against the pause switch (`jazari watch pause`), dry runs (`JAZARI_DRY_RUN=1`)
//! and its repeat limit, then recorded in `actions.jsonl` with who asked for it, whether it ran,
//! failed or was refused. `jazari watch actions` shows the record.
//!
//! The jazari commands a Claude Code session runs are checked before they run too, by the
//! PreToolUse hook ([`hook_check`]): those that stop or end sessions, delete data, change a guard
//! or edit jazari's own files wait for the user's yes in Claude Code's permission prompt.

use clap::{Args, ValueEnum};
use serde_json::{Value, json};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Who asked for an action.
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    /// The user, in the dashboard.
    Dashboard,
    /// The user, running jazari in a terminal.
    Terminal,
    /// A Claude Code session ran jazari (its id, when known).
    Session(Option<String>),
    /// The background watcher, on its own.
    Watcher,
    /// A Claude Code hook.
    Hook,
    /// The founder, from the paired Telegram chat.
    Telegram,
}

impl Origin {
    pub fn key(&self) -> &'static str {
        match self {
            Origin::Dashboard => "dashboard",
            Origin::Terminal => "terminal",
            Origin::Session(_) => "session",
            Origin::Watcher => "watcher",
            Origin::Hook => "hook",
            Origin::Telegram => "telegram",
        }
    }

    /// The session that ran jazari, when a session did.
    pub fn session(&self) -> Option<&str> {
        match self {
            Origin::Session(id) => id.as_deref(),
            _ => None,
        }
    }
}

static ORIGIN: OnceLock<Origin> = OnceLock::new();

/// Fixes who is acting for this process; set once, before the first action.
pub fn set_origin(o: Origin) {
    let _ = ORIGIN.set(o);
}

/// Who is acting: set by `main` for the dashboard, watcher and hooks, otherwise worked out on first
/// use from Claude Code's variables and the process tree.
pub fn origin() -> &'static Origin {
    ORIGIN.get_or_init(detect)
}

fn detect() -> Origin {
    let env_id = std::env::var("CLAUDE_CODE_SESSION_ID").ok().filter(|s| !s.is_empty());
    // Variables can be dropped with `env -u`; a Claude Code process above this one can't.
    let ancestors = crate::context::ancestors();
    let inside = env_id.is_some()
        || std::env::var_os("CLAUDECODE").is_some()
        || ancestors.iter().skip(1).any(|pid| crate::watch::is_claude_process(*pid));
    if !inside {
        return Origin::Terminal;
    }
    let id = env_id.or_else(|| {
        let sessions = crate::live::load_sessions(true);
        crate::context::find_session(&sessions, None, None, &ancestors).ok().map(|s| s.id.clone())
    });
    Origin::Session(id)
}

/// What kind of action.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Message,
    Stop,
    Interrupt,
    EndSession,
    Clean,
    Setting,
    Update,
    Install,
}

impl Kind {
    pub fn key(self) -> &'static str {
        match self {
            Kind::Message => "message",
            Kind::Stop => "stop",
            Kind::Interrupt => "interrupt",
            Kind::EndSession => "end-session",
            Kind::Clean => "clean",
            Kind::Setting => "setting",
            Kind::Update => "update",
            Kind::Install => "install",
        }
    }

    /// What doing it is called, for a dry run's "would …".
    fn verb(self) -> &'static str {
        match self {
            Kind::Message => "message",
            Kind::Stop => "stop",
            Kind::Interrupt => "interrupt",
            Kind::EndSession => "end",
            Kind::Clean => "remove",
            Kind::Setting => "change the setting",
            Kind::Update => "install",
            Kind::Install => "edit",
        }
    }

    /// Seconds before the same action may run again on the same target: a second Ctrl-C quits
    /// Claude Code, and two dashboards must not both interrupt.
    fn repeat_after(self) -> f64 {
        match self {
            Kind::Interrupt => 30.0,
            Kind::EndSession => 60.0,
            Kind::Stop => 5.0,
            _ => 0.0,
        }
    }
}

/// One action about to be taken.
#[derive(Clone, Debug)]
pub struct Action {
    pub kind: Kind,
    /// A session id, cleanup item id, setting key or file.
    pub target: String,
    pub detail: String,
    /// Started by jazari itself rather than asked for just now; `jazari watch pause` holds these.
    pub on_its_own: bool,
}

impl Action {
    pub fn new(kind: Kind, target: impl Into<String>) -> Self {
        Action { kind, target: target.into(), detail: String::new(), on_its_own: false }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = crate::hook::truncate(&detail.into(), 200);
        self
    }

    pub fn on_its_own(mut self, yes: bool) -> Self {
        self.on_its_own = yes;
        self
    }
}

fn dir() -> PathBuf {
    crate::data_dir()
}

fn dry_run() -> bool {
    std::env::var_os("JAZARI_DRY_RUN").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Checks the action, runs it, and records what happened.
pub fn run<T>(a: &Action, f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    Gate::real().run(a, f)
}

/// Checks the action without running it; a refusal is recorded and returned as its reason.
pub fn check(a: &Action) -> Result<(), String> {
    Gate::real().check(a)
}

/// Records an action's outcome (`done`, `kept`, `failed`) after a [`check`].
pub fn record(a: &Action, outcome: &str, reason: &str) {
    Gate::real().record(a, outcome, reason);
}

/// The gate's surroundings, so tests use a scratch folder and a fixed clock.
struct Gate {
    dir: PathBuf,
    origin: Origin,
    now: f64,
    dry: bool,
}

impl Gate {
    fn real() -> Self {
        Gate { dir: dir(), origin: origin().clone(), now: crate::now(), dry: dry_run() }
    }

    fn run<T>(&self, a: &Action, f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
        self.check(a).map_err(anyhow::Error::msg)?;
        match f() {
            Ok(v) => {
                self.record(a, "done", "");
                Ok(v)
            }
            Err(e) => {
                self.record(a, "failed", &e.to_string());
                Err(e)
            }
        }
    }

    fn check(&self, a: &Action) -> Result<(), String> {
        let refuse = |outcome: &str, why: String| {
            self.record(a, outcome, &why);
            Err(why)
        };
        if a.on_its_own && let Some(p) = paused_at(&self.dir, self.now) {
            return refuse("refused", format!("jazari is paused{} (`jazari watch resume` turns it back on)", p.until_label()));
        }
        if let Some(ago) = last_done(&self.dir, a, self.now).filter(|ago| *ago < a.kind.repeat_after()) {
            return refuse("refused", format!("already did this {ago:.0}s ago; wait {:.0}s", a.kind.repeat_after() - ago));
        }
        if self.dry {
            return refuse("dry-run", format!("dry run: would {} {}", a.kind.verb(), a.target));
        }
        Ok(())
    }

    fn record(&self, a: &Action, outcome: &str, reason: &str) {
        let mut entry = json!({
            "ts": self.now,
            "action": a.kind.key(),
            "target": a.target,
            "origin": self.origin.key(),
            "outcome": outcome,
        });
        if let Some(by) = self.origin.session() {
            entry["by"] = json!(by);
        }
        if a.on_its_own {
            entry["on_its_own"] = json!(true);
        }
        if !a.detail.is_empty() {
            entry["detail"] = json!(a.detail);
        }
        if !reason.is_empty() {
            entry["reason"] = json!(crate::hook::truncate(reason, 300));
        }
        let _ = append(&self.dir, &entry);
    }
}

// ---------- the record ----------

const LOG: &str = "actions.jsonl";
const MAX_LOG_BYTES: u64 = 2 << 20;
/// How much of the record's end is read to find recent actions.
const TAIL_BYTES: u64 = 256 << 10;

fn append(dir: &Path, entry: &Value) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let log = dir.join(LOG);
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::rename(&log, dir.join("actions.1.jsonl"));
    }
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');
    // One write on an O_APPEND handle, so two jazari processes never interleave.
    std::fs::OpenOptions::new().create(true).append(true).open(log)?.write_all(&line)?;
    Ok(())
}

/// The last entries of the record, oldest first.
fn tail(dir: &Path) -> Vec<Value> {
    let Ok(mut file) = std::fs::File::open(dir.join(LOG)) else { return Vec::new() };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut text = String::new();
    let _ = file.read_to_string(&mut text);
    let lines = text.lines();
    // A read that starts mid-file starts mid-line.
    lines.skip(usize::from(start > 0)).filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Seconds since the same action last ran on the same target.
fn last_done(dir: &Path, a: &Action, now: f64) -> Option<f64> {
    if a.kind.repeat_after() <= 0.0 {
        return None;
    }
    tail(dir)
        .iter()
        .rev()
        .find(|e| e["action"] == a.kind.key() && e["target"] == a.target.as_str() && e["outcome"] == "done")
        .and_then(|e| e["ts"].as_f64())
        .map(|ts| (now - ts).max(0.0))
}

// ---------- pause ----------

#[derive(Debug, PartialEq)]
struct Pause {
    since: f64,
    until: Option<f64>,
}

impl Pause {
    fn until_label(&self) -> String {
        match self.until {
            Some(t) => format!(" until {}", fmt_time(t)),
            None => String::new(),
        }
    }
}

fn pause_path(dir: &Path) -> PathBuf {
    dir.join("paused.json")
}

fn paused_at(dir: &Path, now: f64) -> Option<Pause> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(pause_path(dir)).ok()?).ok()?;
    let p = Pause { since: v["since"].as_f64().unwrap_or(0.0), until: v["until"].as_f64() };
    p.until.is_none_or(|t| t > now).then_some(p)
}

/// Whether jazari holds everything it does on its own right now.
pub fn paused() -> bool {
    paused_at(&dir(), crate::now()).is_some()
}

/// `30m`, `2h`, `1d` or a bare number of minutes, in seconds.
fn parse_duration(s: &str) -> anyhow::Result<f64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(s.len()));
    let n: f64 = num.parse().map_err(|_| anyhow::anyhow!("expected a duration such as 30m, 2h or 1d, got {s:?}"))?;
    let secs = match unit {
        "s" => n,
        "" | "m" | "min" => n * 60.0,
        "h" => n * 3600.0,
        "d" => n * 86_400.0,
        _ => anyhow::bail!("expected a duration such as 30m, 2h or 1d, got {s:?}"),
    };
    anyhow::ensure!(secs > 0.0, "the duration must be more than zero");
    Ok(secs)
}

fn fmt_time(ts: f64) -> String {
    chrono::DateTime::from_timestamp(ts as i64, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

#[derive(Args)]
pub struct PauseArgs {
    /// How long to pause: 30m, 2h, 1d (default: until `jazari watch resume`).
    #[arg(long = "for", value_name = "DURATION")]
    duration: Option<String>,
}

/// Holds what jazari does on its own, for `duration` (30m, 2h, 1d) or until resumed; returns
/// " until <time>" or "".
pub fn pause(duration: Option<&str>) -> anyhow::Result<String> {
    let now = crate::now();
    let until = duration.map(parse_duration).transpose()?.map(|d| now + d);
    let action = Action::new(Kind::Setting, "paused").detail(until.map_or("until resumed".into(), fmt_time));
    run(&action, || {
        std::fs::create_dir_all(dir())?;
        std::fs::write(pause_path(&dir()), serde_json::to_vec(&json!({ "since": now, "until": until }))?)?;
        Ok(())
    })?;
    Ok(Pause { since: now, until }.until_label())
}

/// Lifts a pause; false when jazari wasn't paused.
pub fn resume() -> anyhow::Result<bool> {
    if !paused() {
        return Ok(false);
    }
    run(&Action::new(Kind::Setting, "paused").detail("resumed"), || {
        std::fs::remove_file(pause_path(&dir()))?;
        Ok(())
    })?;
    Ok(true)
}

/// `jazari watch pause`: hold what jazari does on its own.
pub fn run_pause(a: PauseArgs) -> anyhow::Result<()> {
    let label = pause(a.duration.as_deref())?;
    println!("Paused{label}: jazari won't interrupt or end sessions, clean up or update on its own.");
    println!("Warnings, the command check and anything you ask for still work. `jazari watch resume` turns it back on.");
    Ok(())
}

/// `jazari watch resume`: lift a pause.
pub fn run_resume() -> anyhow::Result<()> {
    if resume()? {
        println!("Resumed: jazari acts on its own again.");
    } else {
        println!("jazari isn't paused.");
    }
    Ok(())
}

// ---------- jazari watch actions ----------

#[derive(Clone, Copy, ValueEnum)]
pub enum Format {
    Table,
    Json,
}

#[derive(Args)]
pub struct ActionsArgs {
    /// How many of the latest actions to show.
    #[arg(short = 'n', long, default_value_t = 20)]
    limit: usize,
    #[arg(long, value_enum, default_value = "table")]
    format: Format,
}

/// `jazari watch actions`: what jazari did, refused or was paused for, newest last.
pub fn run_actions(a: ActionsArgs) -> anyhow::Result<()> {
    let entries = tail(&dir());
    let shown = &entries[entries.len().saturating_sub(a.limit)..];
    let pause = paused_at(&dir(), crate::now());
    match a.format {
        Format::Json => {
            let out = json!({
                "paused": pause.as_ref().map(|p| json!({ "since": p.since, "until": p.until })),
                "actions": shown,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Format::Table => {
            if let Some(p) = &pause {
                println!("Paused since {}{}. `jazari watch resume` turns it back on.\n", fmt_time(p.since), p.until_label());
            }
            if shown.is_empty() {
                println!("No actions recorded yet.");
            }
            for e in shown {
                let s = |k: &str| e[k].as_str().unwrap_or("").to_string();
                let who = match e["by"].as_str() {
                    Some(id) => format!("session {}", &id[..id.len().min(8)]),
                    None => s("origin"),
                };
                let why = [s("detail"), s("reason")].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" · ");
                println!(
                    "{}  {:<16} {:<11} {:<8} {}  {why}",
                    fmt_time(e["ts"].as_f64().unwrap_or(0.0)),
                    who,
                    s("action"),
                    s("outcome"),
                    crate::hook::truncate(&s("target"), 40),
                );
            }
        }
    }
    Ok(())
}

// ---------- the hook: jazari commands a session runs ----------

/// Why the user should approve this tool call first, when it is a jazari command that acts, or an
/// edit to jazari's own files. Read-only commands, `say`, `tell` with a message, and tier A
/// cleanups (which a session stopped for disk space needs) run without asking.
pub fn hook_check(raw: &Value) -> Option<String> {
    let input = &raw["tool_input"];
    let data = [crate::data_dir()];
    match raw["tool_name"].as_str()? {
        "Bash" => command_check(input["command"].as_str()?, &data),
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            let path = input["file_path"].as_str().or(input["notebook_path"].as_str())?;
            data.iter()
                .any(|d| !d.as_os_str().is_empty() && Path::new(path).starts_with(d))
                .then(|| format!("[jazari] This edits jazari's own data ({path}), which only jazari writes. jazari asks you first."))
        }
        _ => None,
    }
}

/// The reason to ask about a shell command, if any.
fn command_check(cmd: &str, data: &[PathBuf]) -> Option<String> {
    for segment in cmd.split([';', '&', '|', '\n', '(', ')', '`']) {
        let words: Vec<&str> = segment.split_whitespace().map(|w| w.trim_matches(['"', '\''])).collect();
        let Some(mut at) = words.iter().position(|w| Path::new(w).file_stem().is_some_and(|n| n == "jazari") && !w.ends_with('/')) else { continue };
        // `jazari watch <cmd>` and `jazari build <cmd>`: the command is after the group.
        if matches!(words.get(at + 1), Some(&"watch") | Some(&"build")) {
            at += 1;
        }
        let Some(sub) = words.get(at + 1) else { continue };
        if let Some(what) = acting(sub, &words[at + 2..]) {
            return Some(format!("[jazari] `jazari {sub}` here {what}. jazari asks you before a session does that."));
        }
    }
    let writes = [">", "rm ", "mv ", "cp ", "tee ", "sed -i", "truncate ", "ln ", "chmod ", "dd "];
    let names_data = cmd.contains(".jazari") || data.iter().any(|d| !d.as_os_str().is_empty() && cmd.contains(&*d.to_string_lossy()));
    (names_data && writes.iter().any(|w| cmd.contains(w)))
        .then(|| "[jazari] This command writes to jazari's own data folder, which only jazari writes. jazari asks you first.".to_string())
}

/// What a jazari subcommand with these arguments does that needs the user's yes.
fn acting(sub: &str, args: &[&str]) -> Option<&'static str> {
    let has = |flag: &str| args.iter().any(|a| *a == flag || a.starts_with(&format!("{flag}=")));
    match sub {
        "tell" if has("--interrupt") => Some("interrupts another session"),
        "tell" if has("--stop") => Some("stops another session"),
        "clean" if has("--run") => Some("removes cleanup items (tier B needs the user's agreement)"),
        "clean" if has("--auto-clean") => Some("changes automatic cleanup"),
        "disk" if has("--guard") => Some("turns the disk guard on or off"),
        "risk" if has("--guard") || has("--osv") || has("--scanner") => Some("changes the command check"),
        "context" if has("--warn") => Some("changes the handoff warning"),
        "retention" if args.iter().any(|a| !a.starts_with('-')) => Some("changes how long usage history is kept"),
        "handoff" if has("--close") || has("--auto-close") => Some("ends a session after the handoff"),
        "handoff" if has("--session") => Some("hands off a session other than this one"),
        "handoff" if args.first() == Some(&"close") => Some("ends a session"),
        "update" if !has("--check") => Some("installs or changes jazari updates"),
        "prompt" if matches!(args.first(), Some(&("rm" | "restore"))) => Some("deletes or restores a saved prompt"),
        "pause" | "resume" => Some("pauses or resumes what jazari does on its own"),
        "install" | "uninstall" => Some("edits ~/.claude/settings.json"),
        "import" => Some("merges data into jazari's history"),
        _ => None,
    }
}

/// The hook output that asks the user about a tool call.
pub fn ask_json(reason: &str, note: Option<&str>) -> Value {
    let mut out = json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "ask", "permissionDecisionReason": reason } });
    if let Some(n) = note {
        out["hookSpecificOutput"]["additionalContext"] = json!(n);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jazari-act-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn gate(dir: &Path, now: f64) -> Gate {
        Gate { dir: dir.to_path_buf(), origin: Origin::Session(Some("abc".into())), now, dry: false }
    }

    #[test]
    fn every_action_is_recorded_with_who_asked_and_how_it_went() {
        let dir = temp("record");
        let a = Action::new(Kind::Message, "s1").detail("use staging");
        gate(&dir, 10.0).run(&a, || Ok(())).unwrap();
        let err = gate(&dir, 11.0).run(&a, || -> anyhow::Result<()> { anyhow::bail!("inbox full") }).unwrap_err();
        assert_eq!(err.to_string(), "inbox full");
        let log = tail(&dir);
        assert_eq!(log.len(), 2);
        assert_eq!((log[0]["outcome"].as_str(), log[0]["by"].as_str(), log[0]["detail"].as_str()), (Some("done"), Some("abc"), Some("use staging")));
        assert_eq!((log[1]["outcome"].as_str(), log[1]["reason"].as_str()), (Some("failed"), Some("inbox full")));
    }

    #[test]
    fn a_repeat_interrupt_is_refused_until_its_window_passes() {
        let dir = temp("repeat");
        let a = Action::new(Kind::Interrupt, "s1");
        let ran = std::cell::Cell::new(0);
        let go = |now| gate(&dir, now).run(&a, || {
            ran.set(ran.get() + 1);
            Ok(())
        });
        assert!(go(100.0).is_ok());
        assert!(go(110.0).unwrap_err().to_string().contains("already did this 10s ago"));
        assert!(gate(&dir, 110.0).run(&Action::new(Kind::Interrupt, "s2"), || Ok(())).is_ok());
        assert!(go(131.0).is_ok());
        assert_eq!(ran.get(), 2);
        // Messages have no window.
        let m = Action::new(Kind::Message, "s1");
        assert!(gate(&dir, 131.0).run(&m, || Ok(())).is_ok() && gate(&dir, 131.0).run(&m, || Ok(())).is_ok());
    }

    #[test]
    fn a_pause_holds_only_what_jazari_does_on_its_own_and_ends_on_time() {
        let dir = temp("pause");
        std::fs::write(pause_path(&dir), r#"{"since":0,"until":500}"#).unwrap();
        let auto = Action::new(Kind::Clean, "npm-cache").on_its_own(true);
        let err = gate(&dir, 100.0).run(&auto, || Ok(())).unwrap_err();
        assert!(err.to_string().starts_with("jazari is paused until"));
        assert!(gate(&dir, 100.0).run(&Action::new(Kind::Clean, "npm-cache"), || Ok(())).is_ok());
        assert!(gate(&dir, 600.0).run(&auto, || Ok(())).is_ok());
        assert_eq!(tail(&dir)[0]["outcome"], "refused");
    }

    #[test]
    fn a_dry_run_records_and_does_nothing() {
        let dir = temp("dry");
        let g = Gate { dry: true, ..gate(&dir, 1.0) };
        let err = g.run(&Action::new(Kind::Stop, "s1"), || -> anyhow::Result<()> { panic!("ran") }).unwrap_err();
        assert_eq!(err.to_string(), "dry run: would stop s1");
        assert_eq!(tail(&dir)[0]["outcome"], "dry-run");
    }

    #[test]
    fn the_record_rolls_over_and_a_partial_first_line_is_skipped() {
        let dir = temp("roll");
        let big = "x".repeat(300 << 10);
        std::fs::write(dir.join(LOG), format!("{{\"pad\":\"{big}\"}}\n{{\"action\":\"stop\"}}\n")).unwrap();
        assert_eq!(tail(&dir).len(), 1);
        std::fs::write(dir.join(LOG), vec![b' '; (MAX_LOG_BYTES + 1) as usize]).unwrap();
        append(&dir, &json!({ "action": "message" })).unwrap();
        assert!(dir.join("actions.1.jsonl").exists());
        assert_eq!(tail(&dir).len(), 1);
    }

    #[test]
    fn durations_read_as_minutes_hours_and_days() {
        assert_eq!(parse_duration("30m").unwrap(), 1800.0);
        assert_eq!(parse_duration("2h").unwrap(), 7200.0);
        assert_eq!(parse_duration("1d").unwrap(), 86_400.0);
        assert_eq!(parse_duration("15").unwrap(), 900.0);
        assert!(parse_duration("soon").is_err() && parse_duration("0m").is_err() && parse_duration("3w").is_err());
    }

    fn asks(cmd: &str) -> bool {
        command_check(cmd, &[PathBuf::from("/home/me/.jazari")]).is_some()
    }

    #[test]
    fn a_session_is_asked_before_jazari_commands_that_act() {
        for cmd in [
            "jazari watch tell 3f2a --stop",
            "jazari watch tell 3f2a --interrupt",
            "/usr/local/bin/jazari watch disk --guard off",
            "jazari watch risk --guard=off",
            "jazari watch clean --run docker-volume-x",
            "jazari watch clean --auto-clean on",
            "jazari build handoff --close < note.md",
            "jazari build handoff --session other",
            "jazari build handoff close --session 1",
            "jazari watch retention forever",
            "jazari watch context --warn off",
            "jazari watch pause --for 1d",
            "jazari update",
            "jazari uninstall",
            "jazari build prompt rm mine",
            "cd /tmp && jazari watch disk --guard off",
            "bash -c 'jazari watch tell x --stop'",
            "echo ok; \"jazari\" watch risk --scanner off",
            "echo '{}' > ~/.jazari/settings.json",
            "rm -rf /home/me/.jazari/claude/inbox",
            "sed -i s/true/false/ ~/.jazari/build/jazari.db",
        ] {
            assert!(asks(cmd), "{cmd}");
        }
    }

    #[test]
    fn reading_reporting_and_tier_a_cleanups_run_without_asking() {
        for cmd in [
            "jazari watch status --format json",
            "jazari watch disk --format json",
            "jazari watch clean --tier-a",
            "jazari watch clean --format json",
            "jazari watch say \"building\"",
            "jazari watch tell 3f2a please rebase first",
            "jazari build handoff < note.md",
            "jazari update --check",
            "jazari watch actions",
            "jazari watch risk --check 'rm -rf /'",
            "cat ~/.jazari/actions.jsonl",
            "cd ~/Projects/jazari && cargo test",
            "ls ~/Projects/jazari/ > files.txt",
        ] {
            assert!(!asks(cmd), "{cmd}");
        }
    }

    #[test]
    fn edits_to_jazari_files_ask_and_others_dont() {
        let edit = |p: &str| json!({ "tool_name": "Write", "tool_input": { "file_path": p } });
        let home = crate::data_dir();
        assert!(hook_check(&edit(&home.join("settings.json").to_string_lossy())).is_some());
        assert!(hook_check(&edit("/tmp/notes.md")).is_none());
        assert!(hook_check(&json!({ "tool_name": "Read", "tool_input": { "file_path": home.join("x").to_string_lossy() } })).is_none());
    }
}

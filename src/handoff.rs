//! `jazari build handoff`: save a handoff prompt from a session that is running out of context, and open
//! a new terminal where a fresh Claude Code session starts from it. Optionally ends the old
//! session once the new one is open.

use crate::terminal::{self, Env, Job, Kind, Opened, Platform};
use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct HandoffArgs {
    #[command(subcommand)]
    cmd: Option<HandoffCmd>,
    /// Read the handoff prompt from this file instead of stdin.
    #[arg(long)]
    file: Option<PathBuf>,
    /// The session handing off (default: the session this command runs in).
    #[arg(long)]
    session: Option<String>,
    /// Open the new session in this terminal only, instead of trying the current terminal, the
    /// default terminal, then tmux.
    #[arg(long, value_enum)]
    terminal: Option<Kind>,
    /// End this session a few seconds after the new one opens.
    #[arg(long, conflicts_with = "keep")]
    close: bool,
    /// Keep this session running, even when closing is on by default.
    #[arg(long)]
    keep: bool,
    /// Save the handoff without opening a terminal.
    #[arg(long)]
    no_launch: bool,
    /// Whether a handoff ends the old session by default: on or off.
    #[arg(long, value_name = "on|off")]
    auto_close: Option<String>,
}

#[derive(Subcommand)]
enum HandoffCmd {
    /// Start a Claude Code session from a saved handoff, in the folder it was written for.
    Resume { id: String },
    /// List saved handoffs, newest first.
    List,
    /// Ends the old session after a handoff (started detached by `jazari build handoff --close`).
    #[command(hide = true)]
    Close {
        #[arg(long)]
        session: String,
        #[arg(long)]
        tmux_pane: Option<String>,
    },
}

/// One saved handoff.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Record {
    id: String,
    created: f64,
    cwd: String,
    from_session: Option<String>,
    prompt: String,
}

/// Seconds the old session gets to finish the tool call that ran `jazari build handoff`.
const CLOSE_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

const CLOSE_SETTING: &str = "handoff_close";

fn dir() -> PathBuf {
    crate::claude_data_dir().join("handoffs")
}

/// Whether a handoff ends the old session by default (on unless turned off), so the session
/// that ran out of context doesn't keep working next to the new one.
pub fn auto_close() -> bool {
    crate::settings::get(CLOSE_SETTING).and_then(|v| v.as_bool()).unwrap_or(true)
}

pub fn run(a: HandoffArgs) -> anyhow::Result<()> {
    match a.cmd {
        Some(HandoffCmd::Resume { id }) => return resume(&id),
        Some(HandoffCmd::List) => return list(),
        Some(HandoffCmd::Close { session, tmux_pane }) => {
            std::thread::sleep(CLOSE_DELAY);
            let a = crate::act::Action::new(crate::act::Kind::EndSession, session.as_str()).detail("after a handoff").on_its_own(true);
            return crate::act::run(&a, || close(&session, tmux_pane.as_deref()));
        }
        None => {}
    }
    if let Some(v) = a.auto_close {
        let on = parse_on_off(&v)?;
        crate::settings::set(CLOSE_SETTING, serde_json::json!(on))?;
        println!("{}", if on { "A handoff now ends the old session once the new one opens (`--keep` keeps it)." } else { "A handoff now keeps the old session running (`--close` ends it)." });
        return Ok(());
    }

    let prompt = read_prompt(a.file.as_deref())?;
    let sessions = crate::live::load_sessions(true);
    let env_id = std::env::var("CLAUDE_CODE_SESSION_ID").ok();
    let session = crate::context::find_session(&sessions, a.session.as_deref(), env_id.as_deref(), &crate::context::ancestors()).ok();
    if a.session.is_some() && session.is_none() {
        bail!("no session matches {:?}", a.session.unwrap_or_default());
    }
    let here = std::env::current_dir().unwrap_or_default();
    let cwd = session.map(|s| PathBuf::from(&s.cwd)).filter(|p| p.is_dir()).unwrap_or(here);
    let from = session.map(|s| s.id.clone());
    let record = Record { id: new_id(chrono::Local::now(), from.as_deref()), created: crate::now(), cwd: cwd.to_string_lossy().into_owned(), from_session: from, prompt };
    let path = save(&dir(), &record)?;
    println!("Saved the handoff to {}", path.display());

    let exe = std::env::current_exe()?;
    let resume_cmd = format!("{} build handoff resume {}", terminal::sh_quote(&exe.to_string_lossy()), record.id);
    if a.no_launch {
        println!("Start it in a new terminal with: {resume_cmd}");
        return Ok(());
    }
    let job = Job { argv: vec![exe.to_string_lossy().into_owned(), "build".into(), "handoff".into(), "resume".into(), record.id.clone()], cwd, title: format!("jazari-{}", record.id) };
    let env = Env::current();
    let platform = Platform::current();
    let plans = terminal::plans(&env, platform, &job, a.terminal, &terminal::warp_dir(platform));
    let opened = match terminal::open_now(&plans, &env) {
        Ok(o) => o,
        Err(errors) => {
            for e in &errors {
                eprintln!("  {e}");
            }
            bail!("couldn't open a terminal for the new session; start it yourself with: {resume_cmd}");
        }
    };
    match &opened {
        Opened::In(kind) => println!("Opened the new session in {}.", kind.label()),
        Opened::TmuxDetached(name) => println!("Started the new session in tmux; attach with: tmux attach -t {name}"),
    }

    let close_now = a.close || (auto_close() && !a.keep);
    if !close_now {
        return Ok(());
    }
    // The user must attach to a detached tmux session first, so this one stays as the way back.
    if matches!(opened, Opened::TmuxDetached(_)) {
        println!("Kept this session running, since the new one isn't on screen yet.");
        return Ok(());
    }
    let Some(s) = session else {
        println!("Kept this session running: jazari couldn't tell which session this is.");
        return Ok(());
    };
    if s.pid.is_none() {
        println!("Kept this session running: its process isn't known yet.");
        return Ok(());
    }
    // From inside a session, only that session can be ended this way.
    if let Some(me) = crate::act::origin().session().filter(|me| *me != s.id) {
        println!("Kept {} running: session {} can only end itself after a handoff.", s.project(), &me[..me.len().min(8)]);
        return Ok(());
    }
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["build", "handoff", "close", "--session", &s.id]);
    if env.tmux
        && let Some(pane) = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty())
    {
        cmd.args(["--tmux-pane", &pane]);
    }
    crate::hook::detached(&mut cmd).spawn().context("couldn't schedule closing this session")?;
    println!("This session ends in {} seconds. Stop here.", CLOSE_DELAY.as_secs());
    Ok(())
}

fn parse_on_off(v: &str) -> anyhow::Result<bool> {
    match v.to_lowercase().as_str() {
        "on" | "true" | "yes" => Ok(true),
        "off" | "false" | "no" => Ok(false),
        _ => bail!("expected on or off, got {v:?}"),
    }
}

fn read_prompt(file: Option<&Path>) -> anyhow::Result<String> {
    let text = match file {
        Some(f) => std::fs::read_to_string(f).with_context(|| format!("couldn't read {}", f.display()))?,
        None => {
            if std::io::stdin().is_terminal() {
                bail!("pass the handoff prompt on stdin (`jazari build handoff <<'EOF' ... EOF`) or with --file");
            }
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        }
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        bail!("the handoff prompt is empty");
    }
    Ok(text)
}

/// A sortable, readable id: the local time, then the start of the session's id.
fn new_id(at: chrono::DateTime<chrono::Local>, session: Option<&str>) -> String {
    let stamp = at.format("%Y%m%d-%H%M%S").to_string();
    match session {
        Some(s) => format!("{stamp}-{}", s.chars().filter(char::is_ascii_alphanumeric).take(8).collect::<String>()),
        None => stamp,
    }
}

fn save(dir: &Path, r: &Record) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", r.id));
    std::fs::write(&path, serde_json::to_vec_pretty(r)?)?;
    Ok(path)
}

fn load(dir: &Path, id: &str) -> anyhow::Result<Record> {
    if id.is_empty() || id.contains(['/', '\\', '.']) {
        bail!("not a handoff id: {id:?}");
    }
    let path = dir.join(format!("{id}.json"));
    let bytes = std::fs::read(&path).with_context(|| format!("no handoff {id} (`jazari build handoff list` shows them)"))?;
    serde_json::from_slice(&bytes).with_context(|| format!("{} is not a handoff", path.display()))
}

fn all(dir: &Path) -> Vec<Record> {
    let mut out: Vec<Record> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect();
    out.sort_by(|a, b| b.created.total_cmp(&a.created));
    out
}

fn list() -> anyhow::Result<()> {
    let records = all(&dir());
    if records.is_empty() {
        println!("No handoffs yet.");
        return Ok(());
    }
    for r in records {
        let first = r.prompt.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        let first: String = first.chars().take(70).collect();
        println!("{}  {}  {}", r.id, r.cwd, first);
    }
    Ok(())
}

/// The `claude` program; `JAZARI_CLAUDE` overrides it.
fn claude() -> std::ffi::OsString {
    std::env::var_os("JAZARI_CLAUDE").filter(|p| !p.is_empty()).unwrap_or_else(|| "claude".into())
}

fn resume(id: &str) -> anyhow::Result<()> {
    let r = load(&dir(), id)?;
    let cwd = PathBuf::from(&r.cwd);
    if !cwd.is_dir() {
        bail!("the handoff's folder {} no longer exists", r.cwd);
    }
    let mut cmd = std::process::Command::new(claude());
    cmd.arg(&r.prompt).current_dir(&cwd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = cmd.exec();
        bail!("couldn't start claude: {e}");
    }
    #[cfg(not(unix))]
    {
        let status = match cmd.status() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::process::Command::new("claude.cmd").arg(&r.prompt).current_dir(&cwd).status()
            }
            other => other,
        }
        .context("couldn't start claude")?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

/// Ends the old session once it has finished its tool call: the tmux pane when it runs in tmux and
/// the pane holds its Claude Code process, otherwise that process. The process and its start are
/// read from jazari's own records now, not from arguments.
fn close(session: &str, tmux_pane: Option<&str>) -> anyhow::Result<()> {
    let sessions = crate::live::load_sessions(false);
    let s = sessions.get(session).ok_or_else(|| anyhow::anyhow!("the session has already ended"))?;
    let pid = s.pid.ok_or_else(|| anyhow::anyhow!("its process isn't known"))?;
    if let Some(pane) = tmux_pane.filter(|p| valid_pane(p))
        && pane_holds(pane, pid)
    {
        let ok = std::process::Command::new("tmux")
            .args(["kill-pane", "-t", pane])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return Ok(());
        }
    }
    anyhow::ensure!(crate::watch::end_session(pid, s.run_started.max(s.started)), "its Claude Code process has changed or ended");
    Ok(())
}

/// A tmux pane id, such as `%12`.
fn valid_pane(p: &str) -> bool {
    p.strip_prefix('%').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether the pane's shell is `pid` or one of its ancestors.
fn pane_holds(pane: &str, pid: u32) -> bool {
    let Ok(out) = std::process::Command::new("tmux").args(["display-message", "-p", "-t", pane, "#{pane_pid}"]).stderr(std::process::Stdio::null()).output() else { return false };
    let Some(shell) = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().ok() else { return false };
    let mut sys = sysinfo::System::new();
    let mut at = sysinfo::Pid::from_u32(pid);
    for _ in 0..12 {
        if at.as_u32() == shell {
            return true;
        }
        sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[at]), true, sysinfo::ProcessRefreshKind::nothing());
        match sys.process(at).and_then(|p| p.parent()) {
            Some(parent) => at = parent,
            None => return false,
        }
    }
    false
}

/// The note the context hook adds once a session passes the warning threshold.
pub fn context_note(pct: f64, exe: &str, close: bool, block: Option<u32>) -> String {
    let jazari = terminal::sh_quote(exe);
    let after = if close {
        "jazari opens a new terminal running a fresh session from it and ends this session a few seconds later, so stop once the command reports success."
    } else {
        "jazari opens a new terminal running a fresh session from it. Then tell the user the new session is running and stop working in this one."
    };
    let blocks = match block {
        Some(b) => format!(
            " At {b}% jazari refuses every tool call except reading, searching and the handoff, so hand off \
             before that. If the user asked you to keep going here, mention the handoff option once and continue."
        ),
        None => " If the user asked you to keep going here, mention the handoff option once and continue.".into(),
    };
    format!(
        "[jazari] This session's context is {pct:.0}% full; Claude Code auto-compacts at 100%. \
         Finish or pause the step you are on, then hand off: write a handoff prompt a fresh session \
         can start from with no other context (the goal; what is done; what is left, with the next \
         step first; decisions and why; files changed; dead ends; how to check the work) and run \
         `{jazari} build handoff <<'EOF'` with the prompt, ending with `EOF`. {after}{blocks}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_tmux_pane_id_is_closed() {
        assert!(valid_pane("%12") && valid_pane("%0"));
        for p in ["%", "12", "%1;kill-server", "-t %1", "%1 "] {
            assert!(!valid_pane(p), "{p}");
        }
    }
    use chrono::TimeZone;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jazari-handoff-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn ids_sort_by_time_and_name_the_session() {
        let at = chrono::Local.with_ymd_and_hms(2026, 11, 23, 9, 5, 7).unwrap();
        assert_eq!(new_id(at, Some("ab12cd34-ffff")), "20261123-090507-ab12cd34");
        assert_eq!(new_id(at, Some("../x")), "20261123-090507-x", "only safe characters reach the file name");
        assert_eq!(new_id(at, None), "20261123-090507");
    }

    #[test]
    fn saves_loads_and_lists_newest_first() {
        let d = temp("store");
        let rec = |id: &str, created| Record { id: id.into(), created, cwd: "/w".into(), from_session: None, prompt: "Goal: x".into() };
        save(&d, &rec("a", 1.0)).unwrap();
        save(&d, &rec("b", 2.0)).unwrap();
        std::fs::write(d.join("junk.json"), "not json").unwrap();
        assert_eq!(load(&d, "a").unwrap(), rec("a", 1.0));
        assert!(load(&d, "../a").is_err(), "an id can't leave the folder");
        assert!(load(&d, "zzz").unwrap_err().to_string().contains("handoff list"));
        assert_eq!(all(&d).iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn note_tells_the_session_how_to_hand_off() {
        let keep = context_note(82.4, "/opt/my tools/jazari", false, None);
        assert!(keep.starts_with("[jazari] This session's context is 82% full"), "{keep}");
        assert!(keep.contains("`'/opt/my tools/jazari' build handoff <<'EOF'`"), "{keep}");
        assert!(keep.contains("stop working in this one"), "{keep}");
        assert!(context_note(90.0, "jazari", true, None).contains("ends this session"));
        assert!(context_note(80.0, "jazari", true, Some(90)).contains("At 90% jazari refuses every tool call"));
    }

    #[test]
    fn on_off_values() {
        assert!(parse_on_off("ON").unwrap());
        assert!(!parse_on_off("off").unwrap());
        assert!(parse_on_off("maybe").is_err());
    }
}

//! Background services: the Telegram bridge and the supervisor as user-level LaunchAgents on
//! macOS, so they start at login and come back after a crash or a reboot. Nothing is installed
//! unless the founder runs `jazari build service install`.

use std::path::{Path, PathBuf};

/// What can run as a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Part {
    Bridge,
    Supervisor,
}

impl Part {
    pub const ALL: [Part; 2] = [Part::Bridge, Part::Supervisor];

    fn name(self) -> &'static str {
        match self {
            Part::Bridge => "bridge",
            Part::Supervisor => "supervisor",
        }
    }

    pub fn label(self) -> String {
        format!("jazari.{}", self.name())
    }

    /// The `jazari` arguments that run it.
    fn args(self) -> [&'static str; 2] {
        match self {
            Part::Bridge => ["build", "bridge"],
            Part::Supervisor => ["build", "run"],
        }
    }
}

/// Where the LaunchAgent files live: `~/Library/LaunchAgents`.
pub fn agents_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("Library").join("LaunchAgents")
}

pub fn plist_path(agents: &Path, part: Part) -> PathBuf {
    agents.join(format!("{}.plist", part.label()))
}

/// Where a service's output goes: `<data dir>/logs/<name>.log`.
pub fn log_path(data: &Path, part: Part) -> PathBuf {
    data.join("logs").join(format!("{}.log", part.name()))
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// How long launchd waits after SIGTERM before it kills a service: enough for the supervisor to
/// save the audit head and leave its task to be queued again. A worker run can take 45 minutes,
/// and isn't waited for.
pub const EXIT_TIMEOUT_SECS: u32 = 120;

/// The exit code the supervisor uses when its own binary was replaced by an update: launchd starts
/// it again on the new binary, and `status` treats it as normal.
pub const RESTART_CODE: i32 = 75;

/// The LaunchAgent for `part`. It starts at login, and launchd starts it again 60 seconds after it
/// stops with an error. A clean exit stays stopped. `env` is copied in, because launchd gives a
/// service almost no environment: without PATH, the supervisor can't find `docker` or `git`.
pub fn plist(part: Part, exe: &Path, log: &Path, env: &[(String, String)]) -> String {
    let mut args = format!("        <string>{}</string>\n", xml(&exe.to_string_lossy()));
    for a in part.args() {
        args.push_str(&format!("        <string>{a}</string>\n"));
    }
    let mut vars = String::new();
    for (k, v) in env {
        vars.push_str(&format!("        <key>{}</key>\n        <string>{}</string>\n", xml(k), xml(v)));
    }
    let log = xml(&log.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
{args}    </array>
    <key>EnvironmentVariables</key>
    <dict>
{vars}    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>60</integer>
    <key>ExitTimeOut</key>
    <integer>{EXIT_TIMEOUT_SECS}</integer>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        label = part.label()
    )
}

/// The environment a service gets: PATH, HOME and JAZARI_HOME (when set) from the shell that
/// installs it.
pub fn service_env() -> Vec<(String, String)> {
    ["PATH", "HOME", "JAZARI_HOME"].iter().filter_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()).map(|v| (k.to_string(), v))).collect()
}

/// What `launchctl print` says about a loaded service.
#[derive(Debug, Default, PartialEq)]
pub struct State {
    pub running: bool,
    pub pid: Option<u32>,
    pub last_exit: Option<String>,
}

/// Only the service's own lines count: they are indented once. Deeper lines describe its parts
/// (sockets, endpoints), which have their own `state = active`.
pub fn parse_print(out: &str) -> State {
    let mut s = State::default();
    let mut state = None;
    for line in out.lines().filter(|l| l.starts_with('\t') && !l.starts_with("\t\t")).map(str::trim) {
        if let Some(v) = line.strip_prefix("state = ") {
            state.get_or_insert(v == "running");
        } else if let Some(v) = line.strip_prefix("pid = ") {
            s.pid = s.pid.or(v.parse().ok());
        } else if let Some(v) = line.strip_prefix("last exit code = ") {
            s.last_exit.get_or_insert(v.to_string());
        }
    }
    s.running = state.unwrap_or(false);
    s
}

/// Runs `launchctl` with these arguments: whether it succeeded, and what it printed.
pub type Launchctl<'a> = &'a dyn Fn(&[String]) -> (bool, String);

pub fn launchctl(args: &[String]) -> (bool, String) {
    match std::process::Command::new("launchctl").args(args).output() {
        Ok(o) => (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(e) => (false, e.to_string()),
    }
}

fn domain() -> String {
    #[cfg(unix)]
    let uid = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let uid = 0;
    format!("gui/{uid}")
}

/// Runs an action through the action gate: the action, then the work to do if it is allowed.
pub type Gate<'a> = &'a dyn Fn(&crate::act::Action, &mut dyn FnMut() -> anyhow::Result<()>) -> anyhow::Result<()>;

/// Everything the service commands touch, so tests can point them at a scratch folder and a fake
/// `launchctl`.
pub struct Env<'a> {
    pub agents: PathBuf,
    pub data: PathBuf,
    pub exe: PathBuf,
    pub env: Vec<(String, String)>,
    pub domain: String,
    pub launchctl: Launchctl<'a>,
    /// Whether a supervisor task is running now.
    pub task_running: &'a dyn Fn() -> bool,
    /// Runs an automatic action through the action gate (`jazari watch pause` holds it, and the
    /// actions log records it).
    pub act: Gate<'a>,
}

fn task_running() -> bool {
    let db = super::dir().join("jazari.db");
    db.exists()
        && super::store::Store::open(&db).and_then(|s| s.counts()).is_ok_and(|c| c.iter().any(|(status, n)| status == "running" && *n > 0))
}

fn act(a: &crate::act::Action, f: &mut dyn FnMut() -> anyhow::Result<()>) -> anyhow::Result<()> {
    crate::act::run(a, f)
}

impl Env<'_> {
    pub fn real() -> anyhow::Result<Env<'static>> {
        Ok(Env {
            agents: agents_dir(),
            data: crate::data_dir(),
            exe: std::env::current_exe()?,
            env: service_env(),
            domain: domain(),
            launchctl: &launchctl,
            task_running: &task_running,
            act: &act,
        })
    }
}

fn unsupported() -> anyhow::Result<()> {
    anyhow::ensure!(cfg!(target_os = "macos"), "background services work only on macOS so far. Elsewhere, run `jazari build bridge` and `jazari build run` in tmux, screen or a terminal that stays open.");
    Ok(())
}

/// Writes and loads the LaunchAgents for `parts`. A service that is already loaded is replaced, so
/// a new jazari binary or PATH takes effect.
pub fn install(e: &Env, parts: &[Part], out: &mut dyn FnMut(String)) -> anyhow::Result<()> {
    unsupported()?;
    std::fs::create_dir_all(&e.agents)?;
    std::fs::create_dir_all(e.data.join("logs"))?;
    for &part in parts {
        let path = plist_path(&e.agents, part);
        let log = log_path(&e.data, part);
        let _ = (e.launchctl)(&["bootout".into(), format!("{}/{}", e.domain, part.label())]);
        std::fs::write(&path, plist(part, &e.exe, &log, &e.env))?;
        let (ok, said) = (e.launchctl)(&["bootstrap".into(), e.domain.clone(), path.to_string_lossy().into()]);
        anyhow::ensure!(ok, "launchctl couldn't load {}: {}", path.display(), said.trim());
        out(format!("Installed the {}: it runs now, and again at each login. Log: {}", part.name(), log.display()));
        out(format!("Stop any {} you started by hand first: only one can run at a time.", part.name()));
    }
    Ok(())
}

/// Stops and removes the LaunchAgents for `parts`. The logs are kept.
pub fn uninstall(e: &Env, parts: &[Part], out: &mut dyn FnMut(String)) -> anyhow::Result<()> {
    unsupported()?;
    for &part in parts {
        let path = plist_path(&e.agents, part);
        let (stopped, _) = (e.launchctl)(&["bootout".into(), format!("{}/{}", e.domain, part.label())]);
        let removed = std::fs::remove_file(&path).is_ok();
        out(if stopped || removed { format!("Removed the {}.", part.name()) } else { format!("The {} wasn't installed.", part.name()) });
    }
    Ok(())
}

/// After an update: starts each installed service again on this binary. A plist that this version
/// would write differently (a new path, a new key) is written again and reloaded; otherwise
/// `launchctl kickstart -k` restarts the service, which waits for it to exit. A supervisor in the
/// middle of a task is left alone: it notices the new binary after the task and restarts itself.
pub fn refresh(e: &Env, out: &mut dyn FnMut(String)) -> anyhow::Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    for part in Part::ALL {
        let path = plist_path(&e.agents, part);
        let Ok(have) = std::fs::read_to_string(&path) else { continue };
        if part == Part::Supervisor && (e.task_running)() {
            out("supervisor: a task is running, so it restarts on the new version when the task ends.".into());
            continue;
        }
        let want = plist(part, &e.exe, &log_path(&e.data, part), &e.env);
        let target = format!("{}/{}", e.domain, part.label());
        let a = crate::act::Action::new(crate::act::Kind::Install, part.label()).detail("restart on the new version").on_its_own(true);
        let result = (e.act)(&a, &mut || {
            if have != want {
                std::fs::write(&path, &want)?;
                let _ = (e.launchctl)(&["bootout".into(), target.clone()]);
                let (ok, said) = (e.launchctl)(&["bootstrap".into(), e.domain.clone(), path.to_string_lossy().into()]);
                anyhow::ensure!(ok, "launchctl couldn't load {}: {}", path.display(), said.trim());
            } else {
                let (ok, said) = (e.launchctl)(&["kickstart".into(), "-k".into(), target.clone()]);
                anyhow::ensure!(ok, "launchctl couldn't restart {}: {}", part.label(), said.trim());
            }
            Ok(())
        });
        match result {
            Ok(()) => out(format!("{}: restarted on the new version.", part.name())),
            Err(err) => out(format!("{}: not restarted ({err}). Run `jazari build service install {}` to restart it.", part.name(), part.name())),
        }
    }
    Ok(())
}

/// The last line with text in a log, from its final 4 KB.
fn last_log_line(log: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(log).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(4096))).ok()?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).ok()?;
    String::from_utf8_lossy(&tail).lines().rev().map(str::trim).find(|l| !l.is_empty()).map(String::from)
}

/// One line per part: installed or not, running or not, and the log.
pub fn status(e: &Env, out: &mut dyn FnMut(String)) -> anyhow::Result<()> {
    unsupported()?;
    for part in Part::ALL {
        if !plist_path(&e.agents, part).exists() {
            out(format!("{}: not installed", part.name()));
            continue;
        }
        let (loaded, said) = (e.launchctl)(&["print".into(), format!("{}/{}", e.domain, part.label())]);
        let s = parse_print(&said);
        let state = if !loaded {
            "installed, but not loaded (run `jazari build service install` again)".to_string()
        } else if s.running {
            format!("running (pid {})", s.pid.map_or("?".into(), |p| p.to_string()))
        } else if s.last_exit.as_deref().is_some_and(|c| c.split(':').next().map(str::trim) == Some(&RESTART_CODE.to_string())) {
            "restarting on the new version".to_string()
        } else if last_log_line(&log_path(&e.data, part)).is_some_and(|l| l.contains("is already running")) {
            // The service exits at once because another copy holds the lock, and launchd starts it
            // again every minute.
            format!("keeps stopping, because another {0} is already running. Stop the {0} you started by hand, or run `jazari build service uninstall {0}`", part.name())
        } else {
            format!("stopped (last exit code {})", s.last_exit.as_deref().unwrap_or("unknown"))
        };
        out(format!("{}: {state}. Log: {}", part.name(), log_path(&e.data, part).display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_runs_jazari_with_the_environment_and_logs() {
        let env = vec![("PATH".to_string(), "/opt/homebrew/bin:/usr/bin".to_string()), ("JAZARI_HOME".to_string(), "/x/a&b".to_string())];
        let p = plist(Part::Supervisor, Path::new("/u/bin/jazari"), Path::new("/x/logs/supervisor.log"), &env);
        assert!(p.contains("<string>jazari.supervisor</string>"));
        assert!(p.contains("<string>/u/bin/jazari</string>\n        <string>build</string>\n        <string>run</string>"), "{p}");
        assert!(p.contains("<key>PATH</key>\n        <string>/opt/homebrew/bin:/usr/bin</string>"));
        assert!(p.contains("<string>/x/a&amp;b</string>"), "values are escaped");
        assert!(p.contains("<key>SuccessfulExit</key>\n        <false/>"), "restarted only after an error");
        assert!(p.contains("<key>ExitTimeOut</key>\n    <integer>120</integer>"), "{p}");
        assert_eq!(p.matches("<string>/x/logs/supervisor.log</string>").count(), 2);
        assert!(plist(Part::Bridge, Path::new("/j"), Path::new("/l"), &[]).contains("<string>build</string>\n        <string>bridge</string>"));
    }

    #[test]
    fn launchctl_print_gives_the_state() {
        let out = "gui/501/jazari.bridge = {\n\tactive count = 1\n\tstate = running\n\tpid = 4321\n\tlast exit code = 0\n}";
        assert_eq!(parse_print(out), State { running: true, pid: Some(4321), last_exit: Some("0".into()) });
        let stopped = "\tstate = not running\n\tlast exit code = 1: Operation not permitted";
        assert_eq!(parse_print(stopped), State { running: false, pid: None, last_exit: Some("1: Operation not permitted".into()) });
        // As macOS prints it: the service's parts have their own nested state lines.
        let real = "gui/501/jazari.bridge = {\n\tactive count = 1\n\tstate = running\n\tpid = 30173\n\tlast exit code = (never exited)\n\tendpoints = {\n\t\tstate = active\n\t}\n\tsockets = {\n\t\tstate = active\n\t}\n}";
        assert_eq!(parse_print(real), State { running: true, pid: Some(30173), last_exit: Some("(never exited)".into()) });
    }

    /// The action gate for tests: runs the action, records nothing.
    fn pass(_: &crate::act::Action, f: &mut dyn FnMut() -> anyhow::Result<()>) -> anyhow::Result<()> {
        f()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn refresh_restarts_services_on_the_new_binary_but_leaves_a_busy_supervisor() {
        let root = std::env::temp_dir().join(format!("jazari-service-refresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let calls = std::cell::RefCell::new(Vec::<String>::new());
        let fake = |args: &[String]| {
            calls.borrow_mut().push(args.join(" "));
            (true, String::new())
        };
        let busy = std::cell::Cell::new(true);
        let task_running = || busy.get();
        let mk = |exe: &str| Env { agents: root.join("LaunchAgents"), data: root.join("data"), exe: exe.into(), env: vec![], domain: "gui/501".into(), launchctl: &fake, task_running: &task_running, act: &pass };
        let old = mk("/old/jazari");
        install(&old, &Part::ALL, &mut |_| {}).unwrap();
        calls.borrow_mut().clear();

        // Same binary path: the bridge is restarted in place; the busy supervisor is left alone.
        let mut lines = Vec::new();
        refresh(&old, &mut |l| lines.push(l)).unwrap();
        assert_eq!(*calls.borrow(), ["kickstart -k gui/501/jazari.bridge"]);
        assert_eq!(lines, ["bridge: restarted on the new version.", "supervisor: a task is running, so it restarts on the new version when the task ends."]);

        // A new binary path: each plist is written again and reloaded.
        calls.borrow_mut().clear();
        busy.set(false);
        let new = mk("/new/jazari");
        refresh(&new, &mut |_| {}).unwrap();
        assert!(std::fs::read_to_string(plist_path(&new.agents, Part::Bridge)).unwrap().contains("<string>/new/jazari</string>"));
        let bridge = plist_path(&new.agents, Part::Bridge).display().to_string();
        assert_eq!(calls.borrow()[..2], ["bootout gui/501/jazari.bridge".to_string(), format!("bootstrap gui/501 {bridge}")]);
        assert_eq!(calls.borrow().len(), 4, "both services");

        // A service that isn't installed is left alone, and a paused gate stops the restart.
        std::fs::remove_file(plist_path(&new.agents, Part::Supervisor)).unwrap();
        calls.borrow_mut().clear();
        let paused = |_: &crate::act::Action, _: &mut dyn FnMut() -> anyhow::Result<()>| -> anyhow::Result<()> { anyhow::bail!("jazari is paused") };
        let held = Env { act: &paused, ..mk("/new/jazari") };
        let mut lines = Vec::new();
        refresh(&held, &mut |l| lines.push(l)).unwrap();
        assert!(calls.borrow().is_empty());
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("bridge: not restarted (jazari is paused)"), "{lines:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn install_status_and_uninstall_use_launchctl_in_the_user_domain() {
        let root = std::env::temp_dir().join(format!("jazari-service-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let calls = std::cell::RefCell::new(Vec::<String>::new());
        let fake = |args: &[String]| {
            calls.borrow_mut().push(args.join(" "));
            match args[0].as_str() {
                "print" => (true, "\tstate = running\n\tpid = 77\n".to_string()),
                _ => (true, String::new()),
            }
        };
        let e = Env { agents: root.join("LaunchAgents"), data: root.join("data"), exe: "/bin/jazari".into(), env: vec![], domain: "gui/501".into(), launchctl: &fake, task_running: &|| false, act: &pass };
        let mut lines = Vec::new();
        install(&e, &[Part::Bridge], &mut |l| lines.push(l)).unwrap();
        assert!(plist_path(&e.agents, Part::Bridge).exists() && !plist_path(&e.agents, Part::Supervisor).exists());
        assert!(e.data.join("logs").is_dir());
        let bootstrap = format!("bootstrap gui/501 {}", plist_path(&e.agents, Part::Bridge).display());
        assert_eq!(*calls.borrow(), ["bootout gui/501/jazari.bridge".to_string(), bootstrap]);

        status(&e, &mut |l| lines.push(l)).unwrap();
        assert!(lines.iter().any(|l| l.starts_with("bridge: running (pid 77)")), "{lines:?}");
        assert!(lines.contains(&"supervisor: not installed".to_string()), "{lines:?}");

        // A second copy that can't get the lock: launchd says it isn't running, and the log says why.
        let stopped = |args: &[String]| if args[0] == "print" { (true, "\tstate = not running\n\tlast exit code = 1\n".to_string()) } else { (true, String::new()) };
        let e2 = Env { launchctl: &stopped, agents: e.agents.clone(), data: e.data.clone(), exe: e.exe.clone(), env: vec![], domain: e.domain.clone(), task_running: &|| false, act: &pass };
        let mut after = Vec::new();
        status(&e2, &mut |l| after.push(l)).unwrap();
        assert!(after[0].starts_with("bridge: stopped (last exit code 1)"), "{after:?}");
        let restarting = |args: &[String]| if args[0] == "print" { (true, "\tstate = not running\n\tlast exit code = 75: Software caused connection abort\n".to_string()) } else { (true, String::new()) };
        let e3 = Env { launchctl: &restarting, agents: e.agents.clone(), data: e.data.clone(), exe: e.exe.clone(), env: vec![], domain: e.domain.clone(), task_running: &|| false, act: &pass };
        after.clear();
        std::fs::write(log_path(&e.data, Part::Bridge), "Error: a jazari bridge is already running\n").unwrap();
        status(&e3, &mut |l| after.push(l)).unwrap();
        assert!(after[0].starts_with("bridge: restarting on the new version"), "exit code 75 is normal, whatever the log says: {after:?}");
        std::fs::write(log_path(&e.data, Part::Bridge), "Listening\nError: a jazari bridge is already running\n\n").unwrap();
        after.clear();
        status(&e2, &mut |l| after.push(l)).unwrap();
        assert!(after[0].starts_with("bridge: keeps stopping, because another bridge is already running."), "{after:?}");
        assert!(lines.iter().any(|l| l.starts_with("Stop any bridge you started by hand first")), "{lines:?}");

        uninstall(&e, &Part::ALL, &mut |l| lines.push(l)).unwrap();
        assert!(!plist_path(&e.agents, Part::Bridge).exists());
        assert!(lines.contains(&"Removed the bridge.".to_string()), "{lines:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}

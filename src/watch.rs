//! `marsad guard`: a background watcher that checks the disk every 20 seconds while Claude Code
//! sessions run, whether or not the dashboard is open. It sends a desktop notification (and, when
//! set, a phone notification) once per level crossing, interrupts sessions at the emergency level,
//! and keeps an optional ballast file that it deletes to give a full disk room to recover.
//! The first `SessionStart` hook starts it; it exits on its own once no session has run for
//! ten minutes.

use crate::disk::{Level, Reading, Thresholds};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const EVERY: Duration = Duration::from_secs(20);
/// With no live session for this long the watcher exits.
const IDLE_EXIT_SECS: f64 = 600.0;
const GB: f64 = 1_000_000_000.0;
/// The ballast is written in chunks of this size.
const CHUNK: usize = 1 << 20;

/// The watcher's settings, under `disk` beside the thresholds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// Bytes of ballast to keep, 0 for none.
    pub ballast: u64,
    /// An ntfy topic URL, such as `https://ntfy.sh/<topic>`.
    pub ntfy: Option<String>,
    /// A URL that gets a JSON POST.
    pub webhook: Option<String>,
}

impl Settings {
    pub fn from_settings(disk: Option<&Value>) -> Self {
        let url = |key: &str| {
            disk.and_then(|d| d.get(key))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|u| (u.starts_with("https://") || u.starts_with("http://")) && !u.contains(char::is_whitespace))
                .map(String::from)
        };
        let gb = disk.and_then(|d| d.get("ballast_gb")).and_then(Value::as_f64).filter(|g| g.is_finite() && *g > 0.0).unwrap_or(0.0);
        Settings { ballast: (gb.min(1000.0) * GB) as u64, ntfy: url("ntfy"), webhook: url("webhook") }
    }

    pub fn load() -> Self {
        Self::from_settings(crate::settings::get("disk").as_ref())
    }
}

/// A live session the watcher can interrupt.
#[derive(Clone, Debug, PartialEq)]
pub struct Live {
    pub id: String,
    pub cwd: String,
    pub pid: Option<u32>,
    /// When this run of the session started (Unix seconds); its process can't be younger.
    pub since: f64,
}

type Measure = Box<dyn Fn(&Path) -> Option<Reading>>;
type Alert = Box<dyn FnMut(&str, &str)>;
type Curl = Box<dyn FnMut(&[String])>;
type Halted = Box<dyn FnMut(&str, &Reading)>;

/// Everything the watcher reads from or does to the machine, so tests can replace it.
pub struct Env {
    pub now: Box<dyn FnMut() -> f64>,
    /// The thresholds and the watcher settings, read on every check.
    pub settings: Box<dyn FnMut() -> (Thresholds, Settings)>,
    /// Sessions whose process is still running.
    pub sessions: Box<dyn FnMut() -> Vec<Live>>,
    pub measure: Measure,
    /// A desktop notification with a sound.
    pub alert: Alert,
    /// Starts `curl` with these arguments, without waiting for it.
    pub curl: Curl,
    /// Starts a cleanup of rebuildable data at this level.
    pub clean: Box<dyn FnMut(Level)>,
    /// Memory and CPU pressure now, with a description for the notification.
    pub pressure: Box<dyn FnMut() -> Option<(crate::pressure::Level, String)>>,
    /// The largest items `marsad clean` could remove (tiers A and B), largest first.
    pub suggest: Box<dyn FnMut() -> Vec<(String, u64)>>,
    /// Whether this pid is still a process that looks like Claude Code.
    /// Whether the pid is still a Claude Code process that started by the given time, so a reused
    /// pid never gets the signal.
    pub is_claude: Box<dyn Fn(u32, f64) -> bool>,
    /// Sends SIGINT; true when it was delivered.
    pub interrupt: Box<dyn FnMut(u32) -> bool>,
    /// Records that a session was stopped, so the dashboard marks it.
    pub halted: Halted,
    /// The ballast file.
    pub ballast: PathBuf,
}

/// What the watcher remembers between checks.
#[derive(Default)]
pub struct Watcher {
    announced: Level,
    /// The memory and CPU level last announced; back to normal re-arms it.
    pressure_announced: crate::pressure::Level,
    /// Pids interrupted during the current emergency crossing.
    interrupted: HashSet<u32>,
    idle_since: Option<f64>,
}

impl Watcher {
    /// One check of the disk. Returns false once no session has run for the idle timeout.
    pub fn tick(&mut self, env: &mut Env) -> bool {
        let now = (env.now)();
        let (t, settings) = (env.settings)();
        let sessions = (env.sessions)();
        let mut paths = crate::system::watched_paths(sessions.iter().map(|s| s.cwd.clone()));
        if let Some(dir) = env.ballast.parent() {
            paths.push(dir.to_path_buf());
        }
        if let Some((level, reading)) = crate::disk::worst(paths.iter().map(PathBuf::as_path), &t, &env.measure) {
            // Space first, so stopping sessions cleanly and cleaning up have room to write. The
            // ballast sits on the home volume, so only freeing that one helps.
            if level == Level::Emergency && reading.system {
                let _ = std::fs::remove_file(&env.ballast);
            }
            let (announce, announced) = t.step(&reading, self.announced);
            self.announced = announced;
            if let Some(l) = announce {
                // From the warning level, name what can be freed, so the note says what to do.
                let free = if l >= Level::Warning { (env.suggest)() } else { Vec::new() };
                let (title, body) = alert_text(l, &reading, &free);
                (env.alert)(&title, &body);
                if l >= Level::Warning {
                    for args in phone_requests(&settings, l, &reading) {
                        (env.curl)(&args);
                    }
                }
            }
            if level >= Level::Warning {
                (env.clean)(level);
            }
            if self.announced < Level::Emergency {
                self.interrupted.clear();
            }
            if level == Level::Emergency {
                let hit: Vec<Live> = on_full_volume(&sessions, &reading, &t, &env.measure).into_iter().cloned().collect();
                self.interrupt(env, &hit, &reading);
            }
            keep_ballast(env, level, &t, &settings);
        }
        if let Some((level, what)) = (env.pressure)() {
            use crate::pressure::Level as P;
            if level == P::Ok {
                self.pressure_announced = P::Ok;
            } else if level > self.pressure_announced {
                self.pressure_announced = level;
                let body = if level == P::Critical {
                    format!("{what}. Sessions are refused heavy work until it eases.")
                } else {
                    format!("{what}. Sessions are told to hold off heavy work.")
                };
                (env.alert)(&format!("marsad · machine {}", level.key()), &body);
            }
        }
        if sessions.is_empty() {
            let since = *self.idle_since.get_or_insert(now);
            now - since < IDLE_EXIT_SECS
        } else {
            self.idle_since = None;
            true
        }
    }

    /// SIGINT to each session's Claude Code process, once per emergency crossing.
    fn interrupt(&mut self, env: &mut Env, sessions: &[Live], reading: &Reading) {
        for s in sessions {
            let Some(pid) = s.pid.filter(|p| *p > 1 && *p != std::process::id()) else { continue };
            if self.interrupted.contains(&pid) || !(env.is_claude)(pid, s.since) {
                continue;
            }
            self.interrupted.insert(pid);
            if (env.interrupt)(pid) {
                (env.halted)(&s.id, reading);
            }
        }
    }
}

fn alert_text(level: Level, r: &Reading, free: &[(String, u64)]) -> (String, String) {
    let what = match level {
        Level::Notice => "",
        Level::Warning => " Sessions are told to wrap up.",
        Level::Critical => " Sessions stop before their next tool call until space is freed.",
        _ => " Running sessions are being interrupted so the machine doesn't hang.",
    };
    let free = if free.is_empty() {
        String::new()
    } else {
        let top: Vec<String> = free.iter().take(2).map(|(what, n)| format!("{what} {}", crate::clean::fmt_bytes(*n))).collect();
        format!(" Can be freed: {}; open marsad, key D.", top.join(", "))
    };
    (format!("marsad · disk {}", level.key()), format!("The disk is {}.{what}{free}", crate::disk::fmt_reading(r)))
}

// ---------- phone notifications ----------

/// The text a phone notification carries: no project names or paths.
fn phone_text(level: Level, r: &Reading) -> String {
    format!("marsad: disk {}, {:.0}% full, {:.1} GB free", level.key(), r.used_pct(), r.free_gb())
}

/// The `curl` argument lists for this level's phone notifications; none below warning.
fn phone_requests(s: &Settings, level: Level, r: &Reading) -> Vec<Vec<String>> {
    if level < Level::Warning {
        return Vec::new();
    }
    let base = |url: &str, extra: Vec<String>| {
        let mut args: Vec<String> = ["--silent", "--show-error", "--max-time", "10", "--proto", "=https,http"].map(String::from).to_vec();
        args.extend(extra);
        args.extend(["--url".to_string(), url.to_string()]);
        args
    };
    let text = phone_text(level, r);
    let mut out = Vec::new();
    if let Some(url) = &s.ntfy {
        let priority = if level >= Level::Critical { "urgent" } else { "high" };
        out.push(base(url, vec![
            "--header".into(), format!("Title: marsad: disk {}", level.key()),
            "--header".into(), format!("Priority: {priority}"),
            "--data-raw".into(), text.clone(),
        ]));
    }
    if let Some(url) = &s.webhook {
        let body = json!({
            "level": level.key(),
            "used_pct": (r.used_pct() * 10.0).round() / 10.0,
            "free_gb": (r.free_gb() * 10.0).round() / 10.0,
            "text": text,
        });
        out.push(base(url, vec!["--header".into(), "Content-Type: application/json".into(), "--data-raw".into(), body.to_string()]));
    }
    out
}

// ---------- ballast ----------

/// Deletes the ballast at the emergency level or when it is turned off, and writes it when it is
/// set and space stays under the warning level with it in place.
fn keep_ballast(env: &Env, level: Level, t: &Thresholds, s: &Settings) {
    let have = std::fs::metadata(&env.ballast).map(|m| m.len()).ok();
    if level == Level::Emergency || (s.ballast == 0 && have.is_some()) {
        let _ = std::fs::remove_file(&env.ballast);
        return;
    }
    if s.ballast == 0 || have == Some(s.ballast) || level >= Level::Warning {
        return;
    }
    if have.is_some() {
        let _ = std::fs::remove_file(&env.ballast);
    }
    let Some(dir) = env.ballast.parent() else { return };
    let Some(r) = (env.measure)(dir) else { return };
    let after = Reading { available: r.available.saturating_sub(s.ballast), ..r };
    if t.level(&after) < Level::Warning {
        let _ = write_ballast(&env.ballast, s.ballast);
    }
}

/// Writes `bytes` of pseudo-random data, so the blocks are really allocated: not sparse, and not
/// shrunk by a compressing file system. Written under a temporary name, then renamed.
fn write_ballast(path: &Path, bytes: u64) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        let mut buf = vec![0u8; CHUNK];
        let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ u64::from(std::process::id());
        let mut left = bytes;
        while left > 0 {
            for word in buf.as_chunks_mut::<8>().0 {
                // xorshift64: never zero, so no block is all zeros.
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *word = state.to_le_bytes();
            }
            let n = left.min(CHUNK as u64) as usize;
            f.write_all(&buf[..n])?;
            left -= n as u64;
        }
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Where the ballast lives.
pub fn ballast_path() -> PathBuf {
    crate::data_dir().join("ballast")
}

// ---------- one watcher at a time ----------

fn pid_path(dir: &Path) -> PathBuf {
    dir.join("guard.pid")
}

/// Held while the watcher runs. The lock is on its own file, because Windows blocks reading a
/// locked file and `marsad disk` reads the pid file.
pub struct Lock {
    _file: std::fs::File,
    pid_file: PathBuf,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let ours = std::fs::read_to_string(&self.pid_file).ok().is_some_and(|s| s.trim() == std::process::id().to_string());
        if ours {
            let _ = std::fs::remove_file(&self.pid_file);
        }
    }
}

/// Takes the watcher lock in `dir` and writes this process's pid, replacing a stale pid file.
/// None when another watcher holds it.
pub fn acquire(dir: &Path) -> Option<Lock> {
    std::fs::create_dir_all(dir).ok()?;
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("guard.lock")).ok()?;
    file.try_lock().ok()?;
    let pid_file = pid_path(dir);
    std::fs::write(&pid_file, std::process::id().to_string()).ok()?;
    Some(Lock { _file: file, pid_file })
}

/// The running watcher's pid: the pid file's, when that process is still a marsad.
pub fn running_pid(dir: &Path, is_marsad: impl Fn(u32) -> bool) -> Option<u32> {
    std::fs::read_to_string(pid_path(dir)).ok()?.trim().parse().ok().filter(|p| is_marsad(*p))
}

/// The sessions to stop for an emergency on `full`: all of them when it is the home volume, whose
/// filling hangs the machine; otherwise only those working on a volume at the emergency level.
fn on_full_volume<'a>(sessions: &'a [Live], full: &Reading, t: &Thresholds, measure: &Measure) -> Vec<&'a Live> {
    sessions
        .iter()
        .filter(|s| full.system || measure(Path::new(&s.cwd)).is_some_and(|r| t.level(&r) == Level::Emergency))
        .collect()
}

/// Hooks fire after the process starts; this covers a slow start and clock rounding.
const START_SLACK_SECS: f64 = 120.0;

/// A process's name, executable and start time (Unix seconds).
fn process(pid: u32) -> Option<(String, Option<PathBuf>, u64)> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    let pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet));
    let p = sys.process(pid)?;
    Some((p.name().to_string_lossy().into_owned(), p.exe().map(Path::to_path_buf), p.start_time()))
}

fn is_marsad(pid: u32) -> bool {
    process(pid).is_some_and(|(name, ..)| name.to_lowercase().contains("marsad"))
}

/// Claude Code runs as `claude`, as `node` (npm installs), or as a binary named after its version
/// inside a `claude` folder (native installs).
fn looks_like_claude(name: &str, exe: Option<&Path>) -> bool {
    let name = name.to_lowercase();
    let name = name.trim_end_matches(".exe");
    name.contains("claude") || name == "node" || exe.is_some_and(|e| e.to_string_lossy().to_lowercase().contains("claude"))
}

/// Ends the session's Claude Code process if it is still that process; true when it was told to.
pub(crate) fn end_session(pid: u32, since: f64) -> bool {
    pid > 1 && pid != std::process::id() && process(pid).is_some_and(|(name, exe, started)| still_the_session(&name, exe.as_deref(), started, since)) && terminate(pid)
}

#[cfg(unix)]
fn terminate(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: kill only sends a signal; the pid was checked to be a Claude Code process.
    pid > 1 && unsafe { libc::kill(pid, libc::SIGTERM) } == 0
}

#[cfg(not(unix))]
fn terminate(pid: u32) -> bool {
    std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Sends SIGINT to the session's Claude Code process if it is still that process; true when sent.
pub(crate) fn interrupt_session(pid: u32, since: f64) -> bool {
    pid > 1 && pid != std::process::id() && process(pid).is_some_and(|(name, exe, started)| still_the_session(&name, exe.as_deref(), started, since)) && sigint(pid)
}

/// The process is Claude Code and was already running when the session's run started; a pid
/// reused by a newer process fails this.
fn still_the_session(name: &str, exe: Option<&Path>, started: u64, since: f64) -> bool {
    looks_like_claude(name, exe) && started as f64 <= since + START_SLACK_SECS
}

/// Starts the watcher, detached, unless one is already running.
pub fn ensure_running() {
    if running_pid(&crate::claude_data_dir(), is_marsad).is_some() {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        let _ = crate::hook::detached(std::process::Command::new(exe).arg("guard")).spawn();
    }
}

#[cfg(unix)]
fn sigint(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: kill only sends a signal; the pid was checked to be a Claude Code process.
    pid > 1 && unsafe { libc::kill(pid, libc::SIGINT) } == 0
}

/// Elsewhere sessions are only warned.
#[cfg(not(unix))]
fn sigint(_: u32) -> bool {
    false
}

impl Env {
    fn real() -> Self {
        let dir = crate::claude_data_dir();
        let mut sessions = std::collections::HashMap::new();
        for ev in crate::jsonl::JsonlTail::new(dir.join("events.1.jsonl")).read_new() {
            crate::events::apply(&mut sessions, &ev);
        }
        let mut tail = crate::jsonl::JsonlTail::new(dir.join("events.jsonl"));
        let mut sys = sysinfo::System::new();
        Env {
            now: Box::new(crate::now),
            settings: Box::new(|| (crate::disk::Config::load().thresholds, Settings::load())),
            sessions: Box::new(move || {
                for ev in tail.read_new() {
                    crate::events::apply(&mut sessions, &ev);
                }
                crate::app::mark_exited(&mut sessions, &mut sys);
                sessions.retain(|_, s| s.status != crate::model::Status::Ended);
                sessions.values().map(|s| Live { id: s.id.clone(), cwd: s.cwd.clone(), pid: s.pid, since: s.run_started.max(s.started) }).collect()
            }),
            measure: Box::new(crate::disk::measure),
            alert: Box::new(|title, body| {
                crate::alerts::attention_sound();
                crate::alerts::notify(title, body);
            }),
            curl: Box::new(|args| {
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                let _ = crate::alerts::detached("curl", &args);
            }),
            clean: Box::new(crate::clean::maybe_start),
            pressure: Box::new(|| {
                let load = crate::pressure::Load::read()?;
                Some((crate::pressure::Limits::load().level(&load), load.describe()))
            }),
            suggest: Box::new(|| {
                let items = crate::clean::plan(&crate::clean::Env::real());
                let mut out: Vec<(String, u64)> = items.into_iter().filter(|i| matches!(i.tier, crate::clean::Tier::A | crate::clean::Tier::B)).map(|i| (i.what, i.bytes)).collect();
                out.sort_by_key(|i| std::cmp::Reverse(i.1));
                out
            }),
            is_claude: Box::new(|pid, since| process(pid).is_some_and(|(name, exe, started)| still_the_session(&name, exe.as_deref(), started, since))),
            interrupt: Box::new(sigint),
            halted: Box::new(crate::disk::log_halt),
            ballast: ballast_path(),
        }
    }
}

/// `marsad guard`: runs the watcher until no session has run for ten minutes.
pub fn run() -> anyhow::Result<()> {
    let Some(_lock) = acquire(&crate::claude_data_dir()) else { return Ok(()) };
    let mut env = Env::real();
    let mut w = Watcher::default();
    while w.tick(&mut env) {
        std::thread::sleep(EVERY);
    }
    Ok(())
}

/// What `marsad disk` shows about the watcher.
pub struct Status {
    pub pid: Option<u32>,
    pub settings: Settings,
    /// The ballast file's size now.
    pub ballast_on_disk: Option<u64>,
}

pub fn status() -> Status {
    Status {
        pid: running_pid(&crate::claude_data_dir(), is_marsad),
        settings: Settings::load(),
        ballast_on_disk: std::fs::metadata(ballast_path()).map(|m| m.len()).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// A 1 TB disk at `p` percent full.
    fn pct(p: f64) -> Reading {
        let total = 1_000_000_000_000u64;
        Reading { system: true, path: "/x".into(), total, available: (total as f64 * (100.0 - p) / 100.0) as u64 }
    }

    /// What a fake environment saw the watcher do.
    #[derive(Default)]
    struct Seen {
        alerts: Vec<String>,
        curls: Vec<Vec<String>>,
        cleans: Vec<Level>,
        interrupts: Vec<u32>,
        halted: Vec<String>,
    }

    struct Fake {
        now: Rc<Cell<f64>>,
        disk: Rc<RefCell<Reading>>,
        sessions: Rc<RefCell<Vec<Live>>>,
        settings: Rc<RefCell<Settings>>,
        seen: Rc<RefCell<Seen>>,
        /// Pressure readings still to come, last first.
        pressure: Rc<RefCell<Vec<(crate::pressure::Level, String)>>>,
    }

    fn fake(ballast: PathBuf) -> (Fake, Env) {
        let f = Fake {
            now: Rc::new(Cell::new(0.0)),
            disk: Rc::new(RefCell::new(pct(50.0))),
            sessions: Rc::new(RefCell::new(Vec::new())),
            settings: Rc::new(RefCell::new(Settings::default())),
            seen: Rc::new(RefCell::new(Seen::default())),
            pressure: Rc::new(RefCell::new(Vec::new())),
        };
        let p = f.pressure.clone();
        let names = |pid: u32| match pid {
            100 => Some("claude"),
            200 => Some("node"),
            300 => Some("python3"),
            400 => Some("2.1.0"),
            500 => Some("claude"),
            _ => None,
        };
        let (now, disk, sessions, settings) = (f.now.clone(), f.disk.clone(), f.sessions.clone(), f.settings.clone());
        let (s1, s2, s3, s4, s5) = (f.seen.clone(), f.seen.clone(), f.seen.clone(), f.seen.clone(), f.seen.clone());
        let env = Env {
            now: Box::new(move || now.get()),
            settings: Box::new(move || (Thresholds::default(), settings.borrow().clone())),
            sessions: Box::new(move || sessions.borrow().clone()),
            measure: Box::new(move |p| Some(Reading { system: true, path: p.to_path_buf(), ..disk.borrow().clone() })),
            alert: Box::new(move |title, _| s1.borrow_mut().alerts.push(title.to_string())),
            curl: Box::new(move |args| s2.borrow_mut().curls.push(args.to_vec())),
            clean: Box::new(move |l| s3.borrow_mut().cleans.push(l)),
            pressure: Box::new(move || p.borrow_mut().pop()),
            suggest: Box::new(|| vec![("Docker build cache".into(), 30_000_000_000), ("npm cache".into(), 7_200_000_000)]),
            is_claude: Box::new(move |pid, since| since >= 0.0 && names(pid).is_some_and(|n| looks_like_claude(n, None))),
            interrupt: Box::new(move |pid| {
                s4.borrow_mut().interrupts.push(pid);
                true
            }),
            halted: Box::new(move |id, _| s5.borrow_mut().halted.push(id.to_string())),
            ballast,
        };
        (f, env)
    }

    fn live(id: &str, pid: Option<u32>) -> Live {
        Live { id: id.into(), cwd: format!("/p/{id}"), pid, since: 0.0 }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("marsad-watch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn each_level_is_announced_once_per_crossing_and_the_phone_only_from_warning() {
        let (f, mut env) = fake(temp("levels").join("ballast"));
        *f.settings.borrow_mut() = Settings { ntfy: Some("https://ntfy.sh/t".into()), ..Settings::default() };
        f.sessions.borrow_mut().push(live("a", None));
        let mut w = Watcher::default();
        for p in [70.0, 80.0, 85.0, 90.0, 91.0, 95.0, 98.0, 98.5, 97.0, 95.5, 98.0] {
            *f.disk.borrow_mut() = pct(p);
            assert!(w.tick(&mut env));
        }
        let seen = f.seen.borrow();
        assert_eq!(seen.alerts, [
            "marsad · disk notice",
            "marsad · disk warning",
            "marsad · disk critical",
            "marsad · disk emergency",
            // 97 stays within 2 points of 98; 95.5 re-arms it.
            "marsad · disk emergency",
        ]);
        let phone: Vec<&str> = seen.curls.iter().map(|a| a.iter().find(|x| x.starts_with("Title:")).unwrap().as_str()).collect();
        assert_eq!(phone, ["Title: marsad: disk warning", "Title: marsad: disk critical", "Title: marsad: disk emergency", "Title: marsad: disk emergency"]);
        assert_eq!(seen.cleans.first(), Some(&Level::Warning), "cleanup starts from warning");
        assert_eq!(seen.cleans.len(), 8);
    }

    #[test]
    fn the_emergency_interrupts_live_claude_processes_once_per_crossing() {
        let (f, mut env) = fake(temp("stop").join("ballast"));
        *f.sessions.borrow_mut() = vec![
            live("claude", Some(100)),
            live("node", Some(200)),
            live("python", Some(300)),
            live("versioned-no-exe", Some(400)),
            live("no-pid", None),
            live("gone", Some(999)),
        ];
        let mut w = Watcher::default();
        *f.disk.borrow_mut() = pct(96.0);
        w.tick(&mut env);
        assert!(f.seen.borrow().interrupts.is_empty(), "critical only stops sessions through the hook");
        *f.disk.borrow_mut() = pct(98.5);
        w.tick(&mut env);
        w.tick(&mut env);
        assert_eq!(f.seen.borrow().interrupts, [100, 200]);
        assert_eq!(f.seen.borrow().halted, ["claude", "node"]);
        // A session that starts during the emergency is interrupted once too.
        f.sessions.borrow_mut().push(live("late", Some(500)));
        w.tick(&mut env);
        assert_eq!(f.seen.borrow().interrupts, [100, 200, 500]);
        // Within the hysteresis it is the same crossing; well below it, the next one interrupts again.
        *f.disk.borrow_mut() = pct(97.0);
        w.tick(&mut env);
        *f.disk.borrow_mut() = pct(98.5);
        w.tick(&mut env);
        assert_eq!(f.seen.borrow().interrupts.len(), 3);
        *f.disk.borrow_mut() = pct(90.0);
        w.tick(&mut env);
        *f.disk.borrow_mut() = pct(98.5);
        w.tick(&mut env);
        assert_eq!(f.seen.borrow().interrupts, [100, 200, 500, 100, 200, 500]);
    }

    #[test]
    fn plausible_claude_code_process_names() {
        assert!(looks_like_claude("claude", None));
        assert!(looks_like_claude("node.exe", None));
        assert!(looks_like_claude("2.1.0", Some(Path::new("/Users/me/.local/share/claude/versions/2.1.0"))));
        assert!(!looks_like_claude("2.1.0", None));
        assert!(!looks_like_claude("python3", Some(Path::new("/usr/bin/python3"))));
    }

    #[test]
    fn the_ballast_is_deleted_at_emergency_and_written_again_below_warning() {
        let dir = temp("ballast");
        let path = dir.join("ballast");
        let (f, mut env) = fake(path.clone());
        let mut w = Watcher::default();
        f.sessions.borrow_mut().push(live("a", None));
        w.tick(&mut env);
        assert!(!path.exists(), "off by default");

        f.settings.borrow_mut().ballast = 3 * CHUNK as u64 + 5;
        w.tick(&mut env);
        let data = std::fs::read(&path).unwrap();
        assert_eq!(data.len() as u64, 3 * CHUNK as u64 + 5);
        assert!(data.chunks(4096).all(|block| block.iter().any(|b| *b != 0)), "no block is all zeros");
        assert_ne!(data[..CHUNK / 2], data[CHUNK / 2..CHUNK], "not a repeated pattern");

        *f.disk.borrow_mut() = pct(98.5);
        w.tick(&mut env);
        assert!(!path.exists(), "deleted at emergency");
        *f.disk.borrow_mut() = pct(92.0);
        w.tick(&mut env);
        assert!(!path.exists(), "not written back at warning");
        *f.disk.borrow_mut() = pct(85.0);
        w.tick(&mut env);
        assert!(path.exists(), "written back below warning");

        // Not written when it would itself push the disk to warning.
        std::fs::remove_file(&path).unwrap();
        let total = 1_000_000_000_000u64;
        *f.disk.borrow_mut() = Reading { system: true, path: "/x".into(), total, available: 100_000_000_000 + 1_000_000 };
        w.tick(&mut env);
        assert!(!path.exists());

        // Turning it off deletes it.
        *f.disk.borrow_mut() = pct(50.0);
        w.tick(&mut env);
        assert!(path.exists());
        f.settings.borrow_mut().ballast = 0;
        w.tick(&mut env);
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn one_watcher_holds_the_lock_and_a_stale_pid_file_is_replaced() {
        let dir = temp("lock");
        std::fs::write(pid_path(&dir), "999999").unwrap();
        // A pid that is gone or isn't marsad doesn't count as running.
        assert_eq!(running_pid(&dir, |_| false), None);
        assert_eq!(running_pid(&dir, |p| p == 999999), Some(999999));

        let lock = acquire(&dir).expect("the lock is free");
        let me = std::process::id();
        assert_eq!(running_pid(&dir, |p| p == me), Some(me), "the stale pid was replaced");
        assert!(acquire(&dir).is_none(), "a second watcher doesn't start");
        drop(lock);
        assert!(!pid_path(&dir).exists(), "the pid file goes with the watcher");
        assert!(acquire(&dir).is_some(), "free again");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn phone_payloads_carry_the_level_and_space_but_no_paths() {
        let s = Settings { ballast: 0, ntfy: Some("https://ntfy.sh/topic".into()), webhook: Some("https://example.com/hook".into()) };
        let r = Reading { system: true, path: "/Users/me/secret-project".into(), ..pct(91.04) };
        assert!(phone_requests(&s, Level::Notice, &r).is_empty());
        let reqs = phone_requests(&s, Level::Warning, &r);
        assert_eq!(reqs.len(), 2);
        let ntfy = &reqs[0];
        assert_eq!(ntfy[ntfy.len() - 2..], ["--url".to_string(), "https://ntfy.sh/topic".to_string()]);
        assert!(ntfy.contains(&"Priority: high".to_string()));
        let text = &ntfy[ntfy.iter().position(|a| a == "--data-raw").unwrap() + 1];
        assert_eq!(text, "marsad: disk warning, 91% full, 89.6 GB free");
        let hook = &reqs[1];
        let body: Value = serde_json::from_str(&hook[hook.iter().position(|a| a == "--data-raw").unwrap() + 1]).unwrap();
        assert_eq!(body, json!({ "level": "warning", "used_pct": 91.0, "free_gb": 89.6, "text": "marsad: disk warning, 91% full, 89.6 GB free" }));
        assert!(reqs.iter().flatten().all(|a| !a.contains("secret-project")));
        assert!(phone_requests(&s, Level::Emergency, &r)[0].contains(&"Priority: urgent".to_string()));
    }

    #[test]
    fn settings_parse_the_ballast_and_only_web_urls() {
        let s = Settings::from_settings(Some(&json!({ "ballast_gb": 2.5, "ntfy": "https://ntfy.sh/x", "webhook": "file:///etc/passwd" })));
        assert_eq!(s, Settings { ballast: 2_500_000_000, ntfy: Some("https://ntfy.sh/x".into()), webhook: None });
        assert_eq!(Settings::from_settings(Some(&json!({ "ballast_gb": -1 }))), Settings::default());
        assert_eq!(Settings::from_settings(None), Settings::default());
    }

    #[test]
    fn the_watcher_exits_after_ten_idle_minutes() {
        let (f, mut env) = fake(temp("idle").join("ballast"));
        let mut w = Watcher::default();
        let at = |t: f64, w: &mut Watcher, env: &mut Env| {
            f.now.set(t);
            w.tick(env)
        };
        assert!(at(0.0, &mut w, &mut env));
        f.sessions.borrow_mut().push(live("a", None));
        assert!(at(300.0, &mut w, &mut env));
        f.sessions.borrow_mut().clear();
        assert!(at(600.0, &mut w, &mut env), "idle starts again when the last session ends");
        assert!(at(1199.0, &mut w, &mut env));
        assert!(!at(1200.0, &mut w, &mut env));
    }

    #[test]
    fn a_reused_pid_is_never_interrupted() {
        assert!(still_the_session("claude", None, 1_000, 1_010.0));
        assert!(still_the_session("node", None, 1_100, 1_010.0), "within the start slack");
        assert!(!still_the_session("node", None, 5_000, 1_010.0), "a newer process took the pid");
        assert!(!still_the_session("python", None, 1_000, 1_010.0));
    }

    #[test]
    fn an_emergency_on_another_volume_stops_only_the_sessions_there() {
        let t = Thresholds::default();
        let sessions = vec![live("home", Some(1)), live("usb", Some(2))];
        let measure: Measure = Box::new(|p| {
            let full = p.ends_with("usb");
            let total = 500_000_000_000u64;
            Some(Reading { system: !full, path: p.to_path_buf(), total, available: if full { total / 100 } else { total / 2 } })
        });
        let usb = Reading { system: false, path: "/p/usb".into(), total: 500_000_000_000, available: 5_000_000_000 };
        let ids: Vec<&str> = on_full_volume(&sessions, &usb, &t, &measure).iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["usb"]);
        let home = Reading { system: true, ..usb };
        assert_eq!(on_full_volume(&sessions, &home, &t, &measure).len(), 2, "a full home volume stops every session");
    }

    #[test]
    fn a_warning_names_the_largest_things_to_free() {
        let r = pct(91.0);
        let free = vec![("Docker build cache".to_string(), 30_000_000_000), ("npm cache".to_string(), 7_200_000_000), ("Yarn cache".to_string(), 1)];
        let (_, body) = alert_text(Level::Warning, &r, &free);
        assert!(body.ends_with("Can be freed: Docker build cache 30.0 GB, npm cache 7.2 GB; open marsad, key D."), "{body}");
        assert!(!alert_text(Level::Notice, &r, &[]).1.contains("freed"));
    }

    #[test]
    fn memory_and_cpu_pressure_is_announced_once_per_rise() {
        use crate::pressure::Level as P;
        let dir = temp("pressure");
        let (f, mut env) = fake(dir.join("ballast"));
        let mut w = Watcher::default();
        let readings = [P::Strained, P::Strained, P::Critical, P::Critical, P::Ok, P::Strained];
        f.pressure.borrow_mut().extend(readings.iter().rev().map(|l| (*l, "memory 96% used".to_string())));
        for _ in readings {
            w.tick(&mut env);
        }
        let alerts = f.seen.borrow().alerts.clone();
        assert_eq!(alerts, ["marsad · machine strained", "marsad · machine critical", "marsad · machine strained"]);
        std::fs::remove_dir_all(dir).ok();
    }
}

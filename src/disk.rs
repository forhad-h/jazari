//! Free space on the volume that holds the user's files, the levels at which marsad warns and stops
//! Claude Code sessions, and `marsad disk`. A full disk can hang the machine: swap can't grow and
//! Claude Code can't write its transcript. With `marsad guard --hook` as a synchronous PreToolUse
//! and UserPromptSubmit hook, every tool call of every running session checks the disk first.

use crate::report::Format;
use clap::Args;
use serde_json::{Map, Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};

const GB: f64 = 1_000_000_000.0;

/// Space on one volume, in bytes. `available` is what an ordinary process can still write
/// (`f_bavail`), which leaves out the blocks reserved for root and APFS purgeable space.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// The path that was measured.
    pub path: PathBuf,
    pub total: u64,
    pub available: u64,
}

impl Reading {
    pub fn used_pct(&self) -> f64 {
        if self.total == 0 { 0.0 } else { (self.total.saturating_sub(self.available)) as f64 * 100.0 / self.total as f64 }
    }

    pub fn free_gb(&self) -> f64 {
        self.available as f64 / GB
    }
}

/// Space on the volume that holds `path`, or None when it can't be read.
#[cfg(target_os = "macos")]
pub fn measure(path: &Path) -> Option<Reading> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // statfs, not statvfs: macOS's statvfs has 32-bit block counts that saturate on large disks.
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `s` is a writable statfs.
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let block = u64::from(s.f_bsize);
    Some(Reading { path: path.to_path_buf(), total: s.f_blocks.saturating_mul(block), available: s.f_bavail.saturating_mul(block) })
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn measure(path: &Path) -> Option<Reading> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `s` is a writable statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast, clippy::useless_conversion)]
    let (frsize, blocks, bavail) = (s.f_frsize as u64, s.f_blocks as u64, s.f_bavail as u64);
    Some(Reading { path: path.to_path_buf(), total: blocks.saturating_mul(frsize), available: bavail.saturating_mul(frsize) })
}

/// Elsewhere, the disk whose mount point is the longest one containing `path`.
#[cfg(not(unix))]
pub fn measure(path: &Path) -> Option<Reading> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let disk = disks.list().iter().filter(|d| path.starts_with(d.mount_point())).max_by_key(|d| d.mount_point().as_os_str().len())?;
    Some(Reading { path: path.to_path_buf(), total: disk.total_space(), available: disk.available_space() })
}

/// Identifies the volume holding `path`, so two folders on one disk are shown once.
fn volume_key(path: &Path, r: &Reading) -> u64 {
    #[cfg(unix)]
    if let Ok(m) = std::fs::metadata(path) {
        return std::os::unix::fs::MetadataExt::dev(&m);
    }
    let _ = path;
    r.total
}

/// How full the disk is, from fine to about to hang the machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    #[default]
    Ok,
    Notice,
    Warning,
    Critical,
    Emergency,
}

impl Level {
    const ALL: [Level; 4] = [Level::Notice, Level::Warning, Level::Critical, Level::Emergency];

    pub fn key(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Notice => "notice",
            Level::Warning => "warning",
            Level::Critical => "critical",
            Level::Emergency => "emergency",
        }
    }

    fn parse(s: &str) -> Option<Level> {
        [Level::Ok].into_iter().chain(Level::ALL).find(|l| l.key() == s)
    }
}

/// A level is reached at `pct` used or when less than `free_gb` is left, whichever comes first:
/// a percent alone is wrong on large disks, where 95% of 2 TB still leaves 100 GB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limit {
    pub pct: f64,
    pub free_gb: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Thresholds {
    /// Notice, warning, critical and emergency, in that order.
    limits: [Limit; 4],
}

impl Default for Thresholds {
    fn default() -> Self {
        let l = |pct, free_gb| Limit { pct, free_gb };
        Thresholds { limits: [l(80.0, 30.0), l(90.0, 15.0), l(95.0, 8.0), l(98.0, 3.0)] }
    }
}

/// Usage must drop this many points (and free space grow by this share of the disk) below a level
/// before crossing it again counts as a new crossing.
const HYSTERESIS_PCT: f64 = 2.0;

impl Thresholds {
    /// The defaults with each field the settings set replaced, for example
    /// `{"critical": {"pct": 93}}`.
    fn from_settings(disk: &Map<String, Value>) -> Self {
        let mut t = Thresholds::default();
        for (level, limit) in Level::ALL.iter().zip(t.limits.iter_mut()) {
            let Some(set) = disk.get(level.key()).and_then(Value::as_object) else { continue };
            if let Some(pct) = set.get("pct").and_then(Value::as_f64).filter(|p| (1.0..=100.0).contains(p)) {
                limit.pct = pct;
            }
            if let Some(gb) = set.get("free_gb").and_then(Value::as_f64).filter(|g| *g >= 0.0) {
                limit.free_gb = gb;
            }
        }
        t
    }

    pub fn limit(&self, level: Level) -> Option<Limit> {
        Level::ALL.iter().position(|l| *l == level).map(|i| self.limits[i])
    }

    fn level_at(&self, pct: f64, available: f64) -> Level {
        Level::ALL
            .iter()
            .zip(self.limits)
            .rev()
            .find(|(_, l)| pct >= l.pct || available < l.free_gb * GB)
            .map_or(Level::Ok, |(level, _)| *level)
    }

    pub fn level(&self, r: &Reading) -> Level {
        self.level_at(r.used_pct(), r.available as f64)
    }

    /// One observation of the disk against the level last announced. Returns the level to announce
    /// now, if it is a new crossing, and the level that counts as announced afterwards. A level
    /// stays announced until usage drops clearly below it, so hovering at a threshold announces once.
    pub fn step(&self, r: &Reading, announced: Level) -> (Option<Level>, Level) {
        let now = self.level(r);
        let margin = r.total as f64 * HYSTERESIS_PCT / 100.0;
        let near = self.level_at(r.used_pct() + HYSTERESIS_PCT, r.available as f64 - margin);
        let announced = announced.min(near);
        if now > announced { (Some(now), now) } else { (None, announced) }
    }
}

/// The disk settings: the thresholds and whether the guard stops sessions.
pub struct Config {
    pub thresholds: Thresholds,
    /// Whether the hook stops sessions at critical. Warnings are sent either way.
    pub guard: bool,
}

impl Config {
    pub fn from_settings(disk: Option<&Value>) -> Self {
        let empty = Map::new();
        let disk = disk.and_then(Value::as_object).unwrap_or(&empty);
        Config { thresholds: Thresholds::from_settings(disk), guard: disk.get("guard").and_then(Value::as_bool).unwrap_or(true) }
    }

    pub fn load() -> Self {
        Self::from_settings(crate::settings::get("disk").as_ref())
    }
}

/// The fullest of the volumes holding these paths, measured once per path.
pub fn worst<'a>(paths: impl IntoIterator<Item = &'a Path>, t: &Thresholds, measure: impl Fn(&Path) -> Option<Reading>) -> Option<(Level, Reading)> {
    paths
        .into_iter()
        .filter_map(measure)
        .map(|r| (t.level(&r), r))
        .max_by(|(la, a), (lb, b)| la.cmp(lb).then(a.used_pct().total_cmp(&b.used_pct())))
}

fn fmt_limit(l: Limit) -> String {
    format!("{}% used or {} GB free", trim_num(l.pct), trim_num(l.free_gb))
}

fn trim_num(n: f64) -> String {
    if n.fract() == 0.0 { format!("{n:.0}") } else { format!("{n:.1}") }
}

fn fmt_reading(r: &Reading) -> String {
    format!("{:.0}% full, {:.1} GB free", r.used_pct(), r.free_gb())
}

// ---------- the guard hook ----------

/// What the guard hook prints for one hook call, or None to print nothing.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// Add a note to Claude's context.
    Note(String),
    /// Refuse the tool call and stop the session.
    Halt { user: String, claude: String },
}

impl Verdict {
    fn to_json(&self, event: &str) -> Value {
        match self {
            Verdict::Note(text) => json!({ "hookSpecificOutput": { "hookEventName": event, "additionalContext": text } }),
            // `continue: false` alone lets a PreToolUse call run; the deny stops it.
            Verdict::Halt { user, claude } => json!({
                "continue": false,
                "stopReason": user,
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": claude,
                },
            }),
        }
    }
}

/// Decides what the hook does for one event given the disk reading and the level this session was
/// last warned at. Returns the verdict and the level to remember.
pub fn decide(cfg: &Config, event: &str, session: &str, reading: &Reading, warned: Level) -> (Option<Verdict>, Level) {
    let t = &cfg.thresholds;
    let level = t.level(reading);
    let critical = t.limit(Level::Critical).unwrap_or(Limit { pct: 95.0, free_gb: 8.0 });
    if cfg.guard && level >= Level::Critical && event == "PreToolUse" {
        let user = format!(
            "marsad stopped this session before its next tool call: the disk is {} (it stops sessions at {}). \
             Free some space, then resume with `claude --resume {session}`. `marsad disk` shows the details; \
             `marsad disk --guard off` turns this stop off.",
            fmt_reading(reading),
            fmt_limit(critical),
        );
        let claude = format!(
            "[marsad] Refused: the disk is {} and a full disk hangs the machine. Don't retry this or any other \
             tool call. The session is being stopped so the user can free space.",
            fmt_reading(reading),
        );
        return (Some(Verdict::Halt { user, claude }), level.max(warned));
    }
    let (announce, warned) = t.step(reading, warned);
    let note = announce.filter(|l| *l >= Level::Warning).map(|_| {
        let stop = if cfg.guard { format!("marsad stops sessions at {}", fmt_limit(critical)) } else { "the machine can hang when it fills up".into() };
        Verdict::Note(format!(
            "[marsad] The disk is {}; {stop}. Finish the current step, don't start large builds, image pulls or \
             installs, and write a handoff now so the work can continue after space is freed.",
            fmt_reading(reading),
        ))
    });
    (note, warned)
}

/// Markers holding the disk level each session was last warned at.
fn warned_dir() -> PathBuf {
    crate::claude_data_dir().join("disk-warned")
}

/// Markers older than this belong to sessions long gone.
const MARKER_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 86_400);

/// The synchronous hook: reads the hook input and returns the JSON to print. Every failure means
/// no output, so a broken check never blocks a session.
pub fn hook() -> Option<String> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).ok()?;
    let raw: Value = serde_json::from_str(&input).ok()?;
    let event = raw["hook_event_name"].as_str()?;
    let id = raw["session_id"].as_str().filter(|id| !id.is_empty() && !id.contains(['/', '\\', '.']))?;
    let cfg = Config::load();
    let home = dirs::home_dir();
    let paths: Vec<&Path> = raw["cwd"].as_str().map(Path::new).into_iter().chain(home.as_deref()).collect();
    let (_, reading) = worst(paths, &cfg.thresholds, measure)?;

    let marker = warned_dir().join(id);
    let was = std::fs::read_to_string(&marker).ok().and_then(|s| Level::parse(s.trim())).unwrap_or_default();
    let (verdict, warned) = decide(&cfg, event, id, &reading, was);
    if warned != was {
        if warned == Level::Ok {
            let _ = std::fs::remove_file(&marker);
        } else if std::fs::create_dir_all(warned_dir()).is_ok() {
            let _ = std::fs::write(&marker, warned.key());
            clean_markers(&warned_dir());
        }
    }
    let verdict = verdict?;
    if matches!(verdict, Verdict::Halt { .. }) {
        // Lets the dashboard show why the session stopped and how to resume it.
        let ev = json!({
            "ts": crate::now(),
            "session_id": id,
            "hook_event_name": HALTED_EVENT,
            "reason": "disk",
            "message": fmt_reading(&reading),
        });
        if let Ok(mut line) = serde_json::to_vec(&ev) {
            line.push(b'\n');
            let _ = crate::hook::append(&crate::claude_data_dir(), &line);
        }
    }
    Some(verdict.to_json(event).to_string())
}

/// The event marsad logs when its guard stops a session.
pub const HALTED_EVENT: &str = "MarsadHalted";

fn clean_markers(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > MARKER_MAX_AGE);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

// ---------- marsad disk ----------

#[derive(Args)]
pub struct DiskArgs {
    /// Stop sessions at the critical level (`on`, the default) or only warn (`off`).
    #[arg(long, value_parser = ["on", "off"])]
    guard: Option<String>,
    #[arg(long, value_enum, default_value = "table")]
    format: Format,
}

pub fn run(a: DiskArgs) -> anyhow::Result<()> {
    if let Some(guard) = a.guard {
        let mut disk = crate::settings::get("disk").and_then(|v| v.as_object().cloned()).unwrap_or_default();
        disk.insert("guard".into(), json!(guard == "on"));
        crate::settings::set("disk", Value::Object(disk))?;
        println!("{}", guard_label(guard == "on"));
        return Ok(());
    }
    let cfg = Config::load();
    let t = &cfg.thresholds;
    // The home folder and every live session's folder; paths on the same volume are shown once.
    let mut paths: Vec<PathBuf> = dirs::home_dir().into_iter().collect();
    let mut cwds: Vec<PathBuf> = crate::live::load_sessions(false).into_values().filter(|s| !s.cwd.is_empty()).map(|s| PathBuf::from(s.cwd)).collect();
    cwds.sort();
    paths.extend(cwds);
    let mut volumes: Vec<(u64, Reading)> = Vec::new();
    for p in &paths {
        let Some(r) = measure(p) else { continue };
        let key = volume_key(p, &r);
        if !volumes.iter().any(|(k, _)| *k == key) {
            volumes.push((key, r));
        }
    }
    let level = volumes.iter().map(|(_, r)| t.level(r)).max().unwrap_or_default();

    match a.format {
        Format::Json => {
            let limits: Map<String, Value> = Level::ALL
                .iter()
                .zip(t.limits)
                .map(|(l, lim)| (l.key().to_string(), json!({ "pct": lim.pct, "free_gb": lim.free_gb })))
                .collect();
            let out = json!({
                "level": level.key(),
                "guard": cfg.guard,
                "volumes": volumes.iter().map(|(_, r)| json!({
                    "path": r.path.display().to_string(),
                    "total_bytes": r.total,
                    "available_bytes": r.available,
                    "used_pct": (r.used_pct() * 10.0).round() / 10.0,
                    "free_gb": (r.free_gb() * 10.0).round() / 10.0,
                    "level": t.level(r).key(),
                })).collect::<Vec<_>>(),
                "thresholds": limits,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        _ => {
            if volumes.is_empty() {
                anyhow::bail!("couldn't read the disk space of {}", paths.first().map_or("the home folder".into(), |p| p.display().to_string()));
            }
            for (_, r) in &volumes {
                println!(
                    "{}: {:.0}% full, {:.1} GB free of {:.1} GB · {}",
                    r.path.display(),
                    r.used_pct(),
                    r.free_gb(),
                    r.total as f64 / GB,
                    t.level(r).key()
                );
            }
            for (l, lim) in Level::ALL.iter().zip(t.limits) {
                let what = match l {
                    Level::Notice => "notice",
                    Level::Warning => "warning: sessions are told to wrap up",
                    Level::Critical if cfg.guard => "critical: sessions stop before their next tool call",
                    Level::Critical => "critical (guard off: sessions are only warned)",
                    _ => "emergency",
                };
                println!("  {} at {}  {what}", l.key(), fmt_limit(lim));
            }
            println!("{}", guard_label(cfg.guard));
        }
    }
    Ok(())
}

fn guard_label(on: bool) -> String {
    if on {
        "Guard: on. Sessions stop at the critical level (`marsad disk --guard off` only warns).".into()
    } else {
        "Guard: off. Sessions are warned but never stopped (`marsad disk --guard on`).".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 228 GB disk with `free` GB available.
    fn disk(free: f64) -> Reading {
        Reading { path: "/x".into(), total: (228.0 * GB) as u64, available: (free * GB) as u64 }
    }

    #[test]
    fn a_level_fires_on_percent_or_free_space_whichever_comes_first() {
        let t = Thresholds::default();
        assert_eq!(t.level(&disk(105.0)), Level::Ok);
        assert_eq!(t.level(&disk(40.0)), Level::Notice, "82% full");
        assert_eq!(t.level(&disk(14.0)), Level::Warning, "94% full but under 15 GB free");
        assert_eq!(t.level(&disk(10.0)), Level::Critical, "95.6% full");
        assert_eq!(t.level(&disk(2.0)), Level::Emergency);
        // On a 2 TB disk, 95% full still leaves 100 GB: only the percent fires.
        let big = Reading { path: "/x".into(), total: (2000.0 * GB) as u64, available: (100.0 * GB) as u64 };
        assert_eq!(t.level(&big), Level::Critical);
        // On a small disk the free-space floor fires first.
        let small = Reading { path: "/x".into(), total: (64.0 * GB) as u64, available: (7.0 * GB) as u64 };
        assert_eq!(t.level(&small), Level::Critical, "89% full but under 8 GB free");
        assert_eq!(t.level(&Reading { path: "/x".into(), total: 0, available: 0 }), Level::Emergency);
    }

    #[test]
    fn each_level_is_announced_once_per_crossing() {
        let t = Thresholds { limits: [80.0, 90.0, 95.0, 98.0].map(|pct| Limit { pct, free_gb: 0.0 }) };
        let pct = |p: f64| Reading { path: "/x".into(), total: 1000, available: (1000.0 - p * 10.0) as u64 };
        let mut announced = Level::Ok;
        let mut out = Vec::new();
        for p in [70.0, 80.5, 79.0, 81.0, 90.0, 96.0, 95.5, 93.5, 96.0, 92.0, 96.0, 77.0, 85.0] {
            let (now, a) = t.step(&pct(p), announced);
            announced = a;
            if let Some(l) = now {
                out.push((p, l));
            }
        }
        assert_eq!(out, [
            (80.5, Level::Notice),
            (90.0, Level::Warning),
            (96.0, Level::Critical),
            // 93.5 is within 2 points of 95, so the next 96 is the same crossing; 92 re-arms it.
            (96.0, Level::Critical),
            (85.0, Level::Notice),
        ]);
    }

    #[test]
    fn settings_override_single_fields_and_the_guard() {
        let v = json!({ "critical": { "pct": 93 }, "notice": { "free_gb": 50, "pct": 500 }, "guard": false });
        let cfg = Config::from_settings(Some(&v));
        assert!(!cfg.guard);
        assert_eq!(cfg.thresholds.limit(Level::Critical), Some(Limit { pct: 93.0, free_gb: 8.0 }));
        assert_eq!(cfg.thresholds.limit(Level::Notice), Some(Limit { pct: 80.0, free_gb: 50.0 }), "a percent over 100 is ignored");
        let cfg = Config::from_settings(Some(&json!("nonsense")));
        assert!(cfg.guard);
        assert_eq!(cfg.thresholds, Thresholds::default());
    }

    #[test]
    fn the_hook_warns_once_and_halts_every_tool_call_at_critical() {
        let cfg = Config { thresholds: Thresholds::default(), guard: true };
        // Notice: nothing for Claude.
        let (v, w) = decide(&cfg, "PreToolUse", "s1", &disk(40.0), Level::Ok);
        assert_eq!((v, w), (None, Level::Notice));
        // Warning: one note, on the prompt or the tool call that sees it first.
        let (v, w) = decide(&cfg, "UserPromptSubmit", "s1", &disk(20.0), w);
        let Some(Verdict::Note(text)) = v else { panic!("expected a note") };
        assert!(text.contains("91% full") && text.contains("write a handoff"), "{text}");
        assert_eq!(decide(&cfg, "PreToolUse", "s1", &disk(20.0), w).0, None, "once per crossing");

        // Critical: every PreToolUse is refused, and the stop reason says how to resume.
        for _ in 0..2 {
            let (v, w2) = decide(&cfg, "PreToolUse", "s1", &disk(9.0), w);
            assert_eq!(w2, Level::Critical);
            let v = v.unwrap();
            let Verdict::Halt { user, .. } = &v else { panic!("expected a halt") };
            assert!(user.contains("claude --resume s1") && user.contains("96% full"), "{user}");
            let out = v.to_json("PreToolUse");
            assert_eq!(out["continue"], false);
            assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        }
        // A prompt at critical is not refused; Claude is told instead.
        assert!(matches!(decide(&cfg, "UserPromptSubmit", "s1", &disk(9.0), Level::Warning).0, Some(Verdict::Note(_))));

        // With the guard off, critical only warns.
        let off = Config { guard: false, ..cfg };
        let (v, _) = decide(&off, "PreToolUse", "s1", &disk(9.0), Level::Ok);
        assert!(matches!(v, Some(Verdict::Note(ref t)) if t.contains("can hang")), "{v:?}");
        // Space freed: nothing, and the level re-arms.
        assert_eq!(decide(&off, "PreToolUse", "s1", &disk(105.0), Level::Critical), (None, Level::Ok));
    }

    #[test]
    fn note_json_names_its_event() {
        let out = Verdict::Note("x".into()).to_json("UserPromptSubmit");
        assert_eq!(out, json!({ "hookSpecificOutput": { "hookEventName": "UserPromptSubmit", "additionalContext": "x" } }));
    }

    #[test]
    fn the_fullest_volume_wins_and_unreadable_paths_are_skipped() {
        let t = Thresholds::default();
        let read = |p: &Path| match p.to_str()? {
            "/home" => Some(disk(100.0)),
            "/scratch" => Some(Reading { path: p.into(), ..disk(5.0) }),
            _ => None,
        };
        let (level, r) = worst([Path::new("/home"), Path::new("/gone"), Path::new("/scratch")], &t, read).unwrap();
        assert_eq!((level, r.path), (Level::Critical, PathBuf::from("/scratch")));
        assert!(worst([Path::new("/gone")], &t, read).is_none());
    }

    #[test]
    fn measures_a_real_volume() {
        let r = measure(&std::env::temp_dir()).expect("the temp folder's volume");
        assert!(r.total > 0 && r.available <= r.total);
    }
}

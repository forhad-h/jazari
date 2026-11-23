//! `marsad clean`: finds data that can be rebuilt (package caches, build output in idle projects,
//! Docker build cache), measures it, and removes it safely. Every removal is one of a fixed set of
//! actions; nothing here runs a command a model wrote. marsad knows which projects have live
//! sessions and when each was last active, so "idle" comes from that, not from file times.
//!
//! Tiers: A is rebuilt automatically and holds no data, so marsad runs it on its own once the disk
//! reaches the warning level. B is rebuilt but slow to get back and runs only when asked. C holds
//! data and is only reported.

use crate::report::Format;
use clap::Args;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

const GB: f64 = 1_000_000_000.0;
/// Smaller items aren't worth listing.
const MIN_BYTES: u64 = 50_000_000;
/// Build output is removed only from projects with no session for this long.
const BUILD_IDLE_SECS: f64 = 86_400.0;
/// `node_modules` is offered only for projects idle this long.
const DEPS_IDLE_SECS: f64 = 14.0 * 86_400.0;
/// An automatic cleanup runs at most this often.
const AUTO_EVERY: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    A,
    B,
    C,
}

impl Tier {
    fn key(self) -> &'static str {
        match self {
            Tier::A => "A",
            Tier::B => "B",
            Tier::C => "C",
        }
    }
}

/// What running an item does. Docker arguments are fixed here, never built from input.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Delete a cache or build-output folder (after the checks in `check_remove`).
    RemoveDir { path: PathBuf, kind: DirKind },
    Docker(&'static [&'static str]),
    /// Report only.
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DirKind {
    /// A package or tool cache; skipped while one of `tools` is running.
    Cache { tools: &'static [&'static str] },
    /// Build output or dependencies inside a project folder.
    Project { root: PathBuf },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// `npm-cache`, `docker-build-cache`, or the folder's path for project items.
    pub id: String,
    pub tier: Tier,
    pub what: String,
    pub bytes: u64,
    pub action: Action,
    pub note: String,
}

/// Runs a program (no shell) and returns its stdout when it exits 0 within the time limit.
pub type Runner = Box<dyn Fn(&str, &[&str], Duration) -> Option<String>>;

/// Everything the planner and the runner read from the machine, so tests can fake it.
pub struct Env {
    pub home: PathBuf,
    pub temp: PathBuf,
    pub now: f64,
    /// Folders of sessions still running.
    pub live: Vec<PathBuf>,
    /// Project folders and when a session last ran in each (Unix seconds).
    pub projects: Vec<(PathBuf, f64)>,
    /// Names of running processes, lowercased, without `.exe`.
    pub running: HashSet<String>,
    pub run: Runner,
}

impl Env {
    pub fn real() -> Self {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
        let live: Vec<PathBuf> = crate::live::load_sessions(false)
            .into_values()
            .filter(|s| s.status != crate::model::Status::Ended && !s.cwd.is_empty())
            .map(|s| PathBuf::from(s.cwd))
            .collect();
        let projects = crate::store::open_readonly()
            .and_then(|c| crate::store::project_activity(&c))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(cwd, at)| {
                let t = chrono::DateTime::parse_from_rfc3339(&at).ok()?;
                Some((PathBuf::from(cwd), t.timestamp() as f64))
            })
            .collect();
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
        let running = sys
            .processes()
            .values()
            .map(|p| p.name().to_string_lossy().to_lowercase().trim_end_matches(".exe").to_string())
            .collect();
        Env {
            home: dirs::home_dir().unwrap_or_default(),
            temp: std::env::temp_dir(),
            now: crate::now(),
            live,
            projects,
            running,
            run: Box::new(run_with_timeout),
        }
    }
}

/// Runs `program` with no shell and no stdin, killing it after `limit`.
fn run_with_timeout(program: &str, args: &[&str], limit: Duration) -> Option<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(program).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut out, &mut s);
        s
    });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let text = reader.join().ok()?;
                return status.success().then_some(text);
            }
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

// ---------- measuring ----------

/// Space a folder takes on disk: allocated blocks, each hard-linked file once, without following
/// symlinks or crossing into other volumes.
pub fn dir_size(path: &Path) -> u64 {
    let mut seen = HashSet::new();
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    let dev = device(&meta);
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.file_type().is_symlink() {
                continue;
            }
            if m.is_dir() {
                if device(&m) == dev {
                    stack.push(e.path());
                }
                continue;
            }
            total += on_disk(&m, &mut seen);
        }
    }
    total
}

#[cfg(unix)]
fn device(m: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::dev(m)
}

#[cfg(not(unix))]
fn device(_: &std::fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn on_disk(m: &std::fs::Metadata, seen: &mut HashSet<(u64, u64)>) -> u64 {
    use std::os::unix::fs::MetadataExt;
    if m.nlink() > 1 && !seen.insert((m.dev(), m.ino())) {
        return 0;
    }
    m.blocks() * 512
}

#[cfg(not(unix))]
fn on_disk(m: &std::fs::Metadata, _: &mut HashSet<(u64, u64)>) -> u64 {
    m.len()
}

/// Docker's sizes, such as `27.7GB`, `512MB` or `0B` (decimal units).
fn parse_size(s: &str) -> u64 {
    let s = s.split_whitespace().next().unwrap_or("").trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let Ok(n) = num.parse::<f64>() else { return 0 };
    let mult = match unit.to_ascii_lowercase().as_str() {
        "b" | "" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        _ => return 0,
    };
    (n * mult) as u64
}

pub fn fmt_bytes(n: u64) -> String {
    let n = n as f64;
    if n >= GB { format!("{:.1} GB", n / GB) } else { format!("{:.0} MB", n / 1e6) }
}

// ---------- planning ----------

struct Cache {
    id: &'static str,
    what: &'static str,
    /// Relative to the home folder; the first that exists is used.
    paths: &'static [&'static str],
    tools: &'static [&'static str],
    tier: Tier,
}

const CACHES: &[Cache] = &[
    Cache { id: "npm-cache", what: "npm cache", paths: &[".npm/_cacache"], tools: &["npm", "npx"], tier: Tier::A },
    Cache { id: "npx-cache", what: "npx packages", paths: &[".npm/_npx"], tools: &["npm", "npx"], tier: Tier::A },
    Cache { id: "yarn-cache", what: "Yarn cache", paths: &["Library/Caches/Yarn", ".cache/yarn"], tools: &["yarn"], tier: Tier::A },
    Cache { id: "pip-cache", what: "pip cache", paths: &["Library/Caches/pip", ".cache/pip"], tools: &["pip", "pip3"], tier: Tier::A },
    Cache { id: "uv-cache", what: "uv cache", paths: &[".cache/uv", "Library/Caches/uv"], tools: &["uv", "uvx"], tier: Tier::A },
    Cache { id: "homebrew-cache", what: "Homebrew downloads", paths: &["Library/Caches/Homebrew", ".cache/Homebrew"], tools: &["brew"], tier: Tier::A },
    Cache { id: "go-build-cache", what: "Go build cache", paths: &["Library/Caches/go-build", ".cache/go-build"], tools: &["go"], tier: Tier::A },
    Cache { id: "xcode-derived-data", what: "Xcode DerivedData", paths: &["Library/Developer/Xcode/DerivedData"], tools: &["xcodebuild", "xcode"], tier: Tier::A },
    Cache { id: "cargo-registry-cache", what: "Cargo downloaded crates", paths: &[".cargo/registry/cache"], tools: &["cargo", "rustc"], tier: Tier::A },
    Cache { id: "cargo-registry-src", what: "Cargo unpacked crates", paths: &[".cargo/registry/src"], tools: &["cargo", "rustc", "rust-analyzer"], tier: Tier::A },
    Cache { id: "gradle-cache", what: "Gradle caches", paths: &[".gradle/caches"], tools: &["gradle", "java"], tier: Tier::A },
    Cache { id: "pnpm-store", what: "pnpm store", paths: &["Library/pnpm/store", ".local/share/pnpm/store"], tools: &["pnpm"], tier: Tier::B },
    Cache { id: "maven-repository", what: "Maven repository (also holds artifacts you installed)", paths: &[".m2/repository"], tools: &["mvn", "java"], tier: Tier::B },
];

/// Folder names `RemoveDir` may delete; anything else is refused however it got into a plan.
const REMOVABLE_NAMES: &[&str] = &[
    "_cacache", "_npx", "Yarn", "yarn", "pip", "uv", "Homebrew", "go-build", "DerivedData", "cache", "src", "caches", "store",
    "repository", ".next", ".turbo", ".parcel-cache", "target", "node_modules",
];

/// Build output and dependency folders inside a project.
fn project_dir_kind(path: &Path) -> Option<(&'static str, Tier)> {
    let name = path.file_name()?.to_str()?;
    let parent = path.parent()?;
    let has = |f: &str| parent.join(f).exists();
    match name {
        ".next" if has("package.json") => Some(("Next.js build output", Tier::A)),
        ".turbo" if has("package.json") => Some(("Turborepo cache", Tier::A)),
        ".parcel-cache" if has("package.json") => Some(("Parcel cache", Tier::A)),
        // Cargo marks its own target folder; a folder that happens to be called target isn't one.
        "target" if path.join("CACHEDIR.TAG").exists() => Some(("Rust build output", Tier::A)),
        "node_modules" if has("package.json") => Some(("node_modules", Tier::B)),
        _ => None,
    }
}

/// Folders under `root` (three levels deep, for monorepos) that `project_dir_kind` recognises.
fn find_project_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if !ft.is_dir() || ft.is_symlink() {
                continue;
            }
            let path = e.path();
            if project_dir_kind(&path).is_some() {
                out.push(path);
                continue;
            }
            let name = e.file_name();
            let name = name.to_string_lossy();
            if depth < 3 && !name.starts_with('.') && !matches!(name.as_ref(), "node_modules" | "target" | "vendor") {
                stack.push((path, depth + 1));
            }
        }
    }
    out
}

/// Whether a session is running in or under `root`, or `root` lies inside a session's folder
/// (other than the home folder itself, where people often start Claude Code).
fn is_live(root: &Path, env: &Env) -> bool {
    env.live.iter().any(|l| l.starts_with(root) || (root.starts_with(l) && l != &env.home))
}

/// Whether git says the folder is ignored, meaning it holds nothing committed.
fn git_ignored(path: &Path, env: &Env) -> bool {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else { return false };
    let parent = parent.to_string_lossy();
    (env.run)("git", &["-C", &parent, "check-ignore", "-q", name], Duration::from_secs(5)).is_some()
}

/// Docker's reclaimable space by kind, when the daemon answers.
fn docker_items(env: &Env) -> Vec<Item> {
    let Some(text) = (env.run)("docker", &["system", "df", "--format", "{{json .}}"], Duration::from_secs(8)) else { return Vec::new() };
    let mut out = Vec::new();
    let note = "Docker Desktop's disk image may give the space back only after Docker restarts".to_string();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let bytes = parse_size(v["Reclaimable"].as_str().unwrap_or(""));
        let (id, tier, what, action): (&str, Tier, &str, Action) = match v["Type"].as_str().unwrap_or("") {
            "Build Cache" => ("docker-build-cache", Tier::A, "Docker build cache", Action::Docker(&["builder", "prune", "-a", "-f"])),
            "Images" => ("docker-unused-images", Tier::B, "Docker images no container uses", Action::Docker(&["image", "prune", "-a", "-f"])),
            "Containers" => (
                "docker-stopped-containers",
                Tier::B,
                "Docker containers stopped for over 7 days",
                Action::Docker(&["container", "prune", "-f", "--filter", "until=168h"]),
            ),
            "Local Volumes" => ("docker-volumes", Tier::C, "Docker volumes no container uses (may hold databases)", Action::None),
            _ => continue,
        };
        if bytes >= MIN_BYTES {
            let note = if tier == Tier::C { "check with `docker volume ls -f dangling=true`; marsad never removes volumes".into() } else { note.clone() };
            out.push(Item { id: id.into(), tier, what: what.into(), bytes, action, note });
        }
    }
    out
}

/// Everything that could be cleaned, largest first within each tier.
pub fn plan(env: &Env) -> Vec<Item> {
    let mut items = Vec::new();
    for c in CACHES {
        let Some(path) = c.paths.iter().map(|p| env.home.join(p)).find(|p| p.is_dir()) else { continue };
        let bytes = dir_size(&path);
        if bytes < MIN_BYTES {
            continue;
        }
        let busy: Vec<&str> = c.tools.iter().copied().filter(|t| env.running.contains(*t)).collect();
        let note = if busy.is_empty() { String::new() } else { format!("skipped while {} runs", busy.join(", ")) };
        items.push(Item {
            id: c.id.into(),
            tier: c.tier,
            what: c.what.into(),
            bytes,
            action: Action::RemoveDir { path, kind: DirKind::Cache { tools: c.tools } },
            note,
        });
    }

    // Projects: the folders sessions ran in, each scanned once (a folder inside another is covered).
    let mut roots: Vec<(PathBuf, f64)> = Vec::new();
    let mut projects = env.projects.clone();
    projects.sort_by(|a, b| a.0.cmp(&b.0));
    for (dir, at) in projects {
        if dir == env.home || !dir.starts_with(&env.home) || !dir.is_dir() {
            continue;
        }
        match roots.iter_mut().find(|(r, _)| dir.starts_with(r)) {
            Some((_, last)) => *last = last.max(at),
            None => roots.push((dir, at)),
        }
    }
    for (root, last) in &roots {
        if is_live(root, env) {
            continue;
        }
        let idle = env.now - last;
        for path in find_project_dirs(root) {
            let Some((what, tier)) = project_dir_kind(&path) else { continue };
            let enough = if tier == Tier::A { BUILD_IDLE_SECS } else { DEPS_IDLE_SECS };
            // What runs on its own must be provably uncommitted; tier B waits for a confirmation.
            if idle < enough || (tier == Tier::A && !(path.join("CACHEDIR.TAG").exists() || git_ignored(&path, env))) {
                continue;
            }
            let bytes = dir_size(&path);
            if bytes < MIN_BYTES {
                continue;
            }
            let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            items.push(Item {
                id: path.display().to_string(),
                tier,
                what: format!("{what} in {name}"),
                bytes,
                action: Action::RemoveDir { path, kind: DirKind::Project { root: root.clone() } },
                note: format!("no session for {} days", (idle / 86_400.0).floor()),
            });
        }
    }

    items.extend(docker_items(env));
    items.sort_by(|a, b| a.tier.cmp(&b.tier).then(b.bytes.cmp(&a.bytes)));
    items
}

// ---------- running ----------

/// The last checks right before a folder is deleted. Returns why it must be kept, if it must.
pub fn check_remove(path: &Path, kind: &DirKind, env: &Env) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| "it no longer exists".to_string())?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err("it is not a plain folder".into());
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !REMOVABLE_NAMES.contains(&name) {
        return Err(format!("marsad never removes a folder named {name:?}"));
    }
    if path.components().any(|c| matches!(c.as_os_str().to_str(), Some(".git" | ".ssh" | ".gnupg")) || c.as_os_str().to_string_lossy().starts_with(".env")) {
        return Err("it is inside a protected folder".into());
    }
    // The real location, so a symlinked parent can't point the delete somewhere else.
    let real = path.canonicalize().map_err(|_| "its location can't be resolved".to_string())?;
    let parent_real = path.parent().and_then(|p| p.canonicalize().ok()).map(|p| p.join(name));
    if parent_real.as_deref() != Some(real.as_path()) {
        return Err("its path goes through a link".into());
    }
    let roots = [&env.home, &env.temp].map(|r| r.canonicalize().unwrap_or_else(|_| r.to_path_buf()));
    if !roots.iter().any(|r| real.starts_with(r) && real.components().count() >= r.components().count() + 2) {
        return Err("it is outside the home and temp folders".into());
    }
    match kind {
        DirKind::Cache { tools } => {
            if let Some(t) = tools.iter().find(|t| env.running.contains(**t)) {
                return Err(format!("{t} is running"));
            }
        }
        DirKind::Project { root } => {
            if !path.starts_with(root) {
                return Err("it is not inside its project".into());
            }
            if is_live(root, env) {
                return Err("a session is running in this project".into());
            }
            if project_dir_kind(path).is_none() {
                return Err("it no longer looks like build output".into());
            }
        }
    }
    Ok(())
}

/// What happened to one item.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Done { freed: u64 },
    Kept(String),
    Failed(String),
}

pub fn run_item(item: &Item, env: &Env, measure: &dyn Fn() -> Option<u64>) -> Outcome {
    let before = measure();
    let result = match &item.action {
        Action::None => return Outcome::Kept("report only".into()),
        Action::RemoveDir { path, kind } => match check_remove(path, kind, env) {
            Err(why) => return Outcome::Kept(why),
            Ok(()) => std::fs::remove_dir_all(path).map_err(|e| e.to_string()),
        },
        Action::Docker(args) => (env.run)("docker", args, Duration::from_secs(600)).map(|_| ()).ok_or_else(|| "docker failed".to_string()),
    };
    match result {
        Ok(()) => Outcome::Done { freed: measure().zip(before).map_or(0, |(after, before)| after.saturating_sub(before)) },
        Err(e) => Outcome::Failed(e),
    }
}

/// Runs the items one at a time and records each in the usage history.
fn run_all(items: &[&Item], env: &Env, auto: bool) -> Vec<(String, u64, Outcome)> {
    let home = env.home.clone();
    let measure = move || crate::disk::measure(&home).map(|r| r.available);
    let conn = crate::store::open().ok();
    let mut out = Vec::new();
    for item in items {
        let outcome = run_item(item, env, &measure);
        if let (Some(conn), Outcome::Done { freed }) = (&conn, &outcome) {
            let path = match &item.action {
                Action::RemoveDir { path, .. } => path.display().to_string(),
                _ => String::new(),
            };
            let c = crate::store::Cleanup { item: &item.id, tier: item.tier.key(), path: &path, estimated: item.bytes, freed: *freed, auto };
            let _ = crate::store::record_cleanup(conn, crate::now(), &c);
        }
        out.push((item.what.clone(), item.bytes, outcome));
    }
    out
}

// ---------- automatic cleanup ----------

/// Whether marsad cleans tier A on its own at the warning level (on unless turned off). At the
/// critical level it does so either way.
pub fn auto_enabled() -> bool {
    crate::settings::get("disk").and_then(|d| d.get("auto_clean").and_then(serde_json::Value::as_bool)).unwrap_or(true)
}

fn stamp_path() -> PathBuf {
    crate::claude_data_dir().join("clean-auto.stamp")
}

/// Called by the guard hook: starts a detached automatic cleanup when the disk is at the warning
/// level (critical without auto-clean) and none ran in the last half hour. One stat when not due.
pub fn maybe_start(level: crate::disk::Level) {
    use crate::disk::Level;
    if level < Level::Warning || (level < Level::Critical && !auto_enabled()) {
        return;
    }
    let stamp = stamp_path();
    let due = std::fs::metadata(&stamp).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_none_or(|age| age >= AUTO_EVERY);
    if !due || std::fs::create_dir_all(crate::claude_data_dir()).is_err() || std::fs::write(&stamp, b"").is_err() {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        let _ = crate::hook::detached(std::process::Command::new(exe).args(["clean", "--auto"])).spawn();
    }
}

/// `marsad clean --auto`: tier A only, then one notification saying what was freed.
fn auto() -> anyhow::Result<()> {
    let cfg = crate::disk::Config::load();
    let home = dirs::home_dir().unwrap_or_default();
    let level = crate::disk::measure(&home).map(|r| cfg.thresholds.level(&r)).unwrap_or_default();
    if level < crate::disk::Level::Warning || (level < crate::disk::Level::Critical && !auto_enabled()) {
        return Ok(());
    }
    let lock = crate::claude_data_dir().join("clean.lock");
    // A lock older than an hour was left by a cleanup that died.
    if std::fs::metadata(&lock).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|a| a > Duration::from_secs(3600)) {
        let _ = std::fs::remove_file(&lock);
    }
    if std::fs::OpenOptions::new().write(true).create_new(true).open(&lock).is_err() {
        return Ok(());
    }
    let env = Env::real();
    let items = plan(&env);
    let tier_a: Vec<&Item> = items.iter().filter(|i| i.tier == Tier::A).collect();
    let results = run_all(&tier_a, &env, true);
    let _ = std::fs::remove_file(&lock);
    let done: Vec<String> = results
        .iter()
        .filter(|(_, _, o)| matches!(o, Outcome::Done { .. }))
        .map(|(what, bytes, _)| format!("{what} {}", fmt_bytes(*bytes)))
        .collect();
    if !done.is_empty() {
        let total: u64 = results.iter().filter(|(_, _, o)| matches!(o, Outcome::Done { .. })).map(|(_, b, _)| *b).sum();
        let now = crate::disk::measure(&home).map(|r| format!(" · disk now {:.0}% full", r.used_pct())).unwrap_or_default();
        crate::alerts::notify(&format!("marsad freed about {}", fmt_bytes(total)), &format!("{}{now}", done.join(", ")));
    }
    Ok(())
}

// ---------- marsad clean ----------

#[derive(Args)]
pub struct CleanArgs {
    /// Run these items (ids from the plan); tier C is never run.
    #[arg(long, num_args = 1..)]
    run: Vec<String>,
    /// Run every tier A item (data that is rebuilt automatically).
    #[arg(long)]
    tier_a: bool,
    /// Turn automatic tier A cleanup at the warning level on or off.
    #[arg(long, value_parser = ["on", "off"])]
    auto_clean: Option<String>,
    /// Started by the disk guard: clean tier A if the disk is at the warning level.
    #[arg(long, hide = true)]
    auto: bool,
    #[arg(long, value_enum, default_value = "table")]
    format: Format,
}

pub fn run(a: CleanArgs) -> anyhow::Result<()> {
    if a.auto {
        return auto();
    }
    if let Some(on) = a.auto_clean {
        let mut disk = crate::settings::get("disk").and_then(|v| v.as_object().cloned()).unwrap_or_default();
        disk.insert("auto_clean".into(), json!(on == "on"));
        crate::settings::set("disk", serde_json::Value::Object(disk))?;
        println!("{}", auto_label(on == "on"));
        return Ok(());
    }
    let env = Env::real();
    let items = plan(&env);
    if a.tier_a || !a.run.is_empty() {
        let mut chosen: Vec<&Item> = items.iter().filter(|i| (a.tier_a && i.tier == Tier::A) || a.run.contains(&i.id)).collect();
        for id in &a.run {
            match items.iter().find(|i| &i.id == id) {
                None => eprintln!("{id}: not in the plan (run `marsad clean` to list items)"),
                Some(i) if i.tier == Tier::C => eprintln!("{id}: holds data; marsad only reports it"),
                Some(_) => {}
            }
        }
        chosen.retain(|i| i.tier != Tier::C);
        for (what, bytes, outcome) in run_all(&chosen, &env, false) {
            match outcome {
                Outcome::Done { freed } => println!("removed  {what} ({}, disk gained {})", fmt_bytes(bytes), fmt_bytes(freed)),
                Outcome::Kept(why) => println!("kept     {what}: {why}"),
                Outcome::Failed(e) => println!("failed   {what}: {e}"),
            }
        }
        return Ok(());
    }

    let freed_30d = crate::store::open_readonly().ok().and_then(|c| crate::store::freed_since(&c, env.now - 30.0 * 86_400.0).ok());
    match a.format {
        Format::Json => {
            let out = json!({
                "auto_clean": auto_enabled(),
                "freed_last_30_days_bytes": freed_30d,
                "items": items.iter().map(|i| json!({
                    "id": i.id,
                    "tier": i.tier.key(),
                    "what": i.what,
                    "bytes": i.bytes,
                    "gb": (i.bytes as f64 / GB * 10.0).round() / 10.0,
                    "note": i.note,
                    "path": match &i.action { Action::RemoveDir { path, .. } => Some(path.display().to_string()), _ => None },
                })).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        _ => {
            if let Some(r) = crate::disk::measure(&env.home) {
                println!("Disk {:.0}% full, {:.1} GB free", r.used_pct(), r.free_gb());
            }
            let heads = [
                (Tier::A, "A  rebuilt automatically, holds no data (`marsad clean --tier-a`)"),
                (Tier::B, "B  rebuilt, but slow to get back (`marsad clean --run <id>`)"),
                (Tier::C, "C  holds data: reported, never removed by marsad"),
            ];
            for (tier, head) in heads {
                let list: Vec<&Item> = items.iter().filter(|i| i.tier == tier).collect();
                if list.is_empty() {
                    continue;
                }
                let total: u64 = list.iter().map(|i| i.bytes).sum();
                println!("\n{head}: {}", fmt_bytes(total));
                for i in list {
                    let note = if i.note.is_empty() { String::new() } else { format!("  ({})", i.note) };
                    println!("  {:>9}  {}  [{}]{note}", fmt_bytes(i.bytes), i.what, i.id);
                }
            }
            if items.is_empty() {
                println!("Nothing to clean.");
            }
            println!("\n{}", auto_label(auto_enabled()));
            if let Some(n) = freed_30d.filter(|n| *n > 0) {
                println!("Freed in the last 30 days: {}", fmt_bytes(n));
            }
        }
    }
    Ok(())
}

fn auto_label(on: bool) -> String {
    if on {
        "Auto-clean: on. Tier A is cleaned on its own when the disk reaches the warning level (`marsad clean --auto-clean off`).".into()
    } else {
        "Auto-clean: off. Tier A is still cleaned at the critical level (`marsad clean --auto-clean on`).".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree(PathBuf);
    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tree(name: &str) -> Tree {
        let dir = std::env::temp_dir().join(format!("marsad-clean-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Tree(dir.canonicalize().unwrap())
    }

    fn file(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![7u8; bytes]).unwrap();
    }

    /// An env whose home is `home`, with git saying every folder is ignored unless `tracked`.
    fn env(home: &Path, now: f64, tracked: &'static [&'static str]) -> Env {
        Env {
            home: home.to_path_buf(),
            temp: home.join("tmp"),
            now,
            live: Vec::new(),
            projects: Vec::new(),
            running: HashSet::new(),
            run: Box::new(move |prog, args, _| match prog {
                "git" if !tracked.iter().any(|t| args.contains(t)) => Some(String::new()),
                _ => None,
            }),
        }
    }

    #[test]
    fn sizes_parse_like_docker_prints_them() {
        assert_eq!(parse_size("27.7GB (84%)"), 27_700_000_000);
        assert_eq!(parse_size("512MB"), 512_000_000);
        assert_eq!(parse_size("1.5kB"), 1500);
        assert_eq!(parse_size("0B"), 0);
        assert_eq!(parse_size("lots"), 0);
    }

    #[test]
    fn plans_caches_idle_build_output_and_docker_by_tier() {
        let t = tree("plan");
        let home = &t.0;
        file(&home.join(".npm/_cacache/blob"), 60_000_000);
        file(&home.join(".npm/_npx/tiny"), 10);
        // An idle project with build output, deps and a tracked `.turbo` that must stay.
        let web = home.join("Projects/web");
        file(&web.join("package.json"), 2);
        file(&web.join(".next/cache/a"), 60_000_000);
        file(&web.join("node_modules/x/index.js"), 60_000_000);
        file(&web.join(".turbo/log"), 60_000_000);
        // A Rust project with a live session: nothing in it is offered.
        let api = home.join("Projects/api");
        file(&api.join("target/CACHEDIR.TAG"), 10);
        file(&api.join("target/debug/big"), 60_000_000);
        // A folder that only happens to be called target.
        file(&home.join("Projects/notes/target/plan.md"), 60_000_000);

        let now = 100.0 * 86_400.0;
        let mut e = env(home, now, &[".turbo"]);
        e.projects = vec![
            (web.clone(), now - 20.0 * 86_400.0),
            (web.join("apps"), now - 30.0 * 86_400.0),
            (api.clone(), now - 30.0 * 86_400.0),
            (home.join("Projects/notes"), now - 30.0 * 86_400.0),
        ];
        e.live = vec![api.join("src")];
        e.run = Box::new(|prog, args, _| match (prog, args.first()) {
            ("git", _) if !args.contains(&".turbo") => Some(String::new()),
            ("docker", Some(&"system")) => Some(
                [
                    r#"{"Type":"Build Cache","Reclaimable":"27.7GB"}"#,
                    r#"{"Type":"Images","Reclaimable":"30.2GB (70%)"}"#,
                    r#"{"Type":"Local Volumes","Reclaimable":"24.5GB (80%)"}"#,
                    r#"{"Type":"Containers","Reclaimable":"0B (0%)"}"#,
                ]
                .join("\n"),
            ),
            _ => None,
        });
        let items = plan(&e);
        let got: Vec<(Tier, String)> = items.iter().map(|i| (i.tier, i.id.replace(&home.display().to_string(), "~"))).collect();
        assert_eq!(got, [
            (Tier::A, "docker-build-cache".to_string()),
            (Tier::A, "npm-cache".into()),
            (Tier::A, "~/Projects/web/.next".into()),
            (Tier::B, "docker-unused-images".into()),
            (Tier::B, "~/Projects/web/node_modules".into()),
            (Tier::C, "docker-volumes".into()),
        ]);
        assert_eq!(items[0].action, Action::Docker(&["builder", "prune", "-a", "-f"]));
        assert_eq!(items[5].action, Action::None, "volumes are never removed");
    }

    #[test]
    fn a_recently_active_project_keeps_its_build_output() {
        let t = tree("recent");
        let web = t.0.join("web");
        file(&web.join("package.json"), 2);
        file(&web.join(".next/a"), 60_000_000);
        let mut e = env(&t.0, 1000.0 * 86_400.0, &[]);
        e.projects = vec![(web, 1000.0 * 86_400.0 - 3600.0)];
        assert!(plan(&e).is_empty());
    }

    #[test]
    fn removal_is_rechecked_right_before_it_runs() {
        let t = tree("check");
        let home = &t.0;
        let mut e = env(home, 0.0, &[]);
        let cache = home.join(".npm/_cacache");
        file(&cache.join("blob"), 10);
        let npm = DirKind::Cache { tools: &["npm"] };
        e.running.insert("npm".into());
        assert_eq!(check_remove(&cache, &npm, &e), Err("npm is running".into()));
        e.running.clear();
        assert_eq!(check_remove(&cache, &npm, &e), Ok(()));

        // Only the fixed folder names, never protected folders, never outside home and temp.
        let secrets = home.join(".ssh/cache");
        file(&secrets.join("k"), 1);
        assert!(check_remove(&secrets, &npm, &e).is_err());
        let docs = home.join("Documents");
        file(&docs.join("cv.pdf"), 1);
        assert!(check_remove(&docs, &npm, &e).unwrap_err().contains("never removes"));
        assert!(check_remove(&home.join("gone/_cacache"), &npm, &e).is_err());
        let outside = std::env::temp_dir().join(format!("marsad-outside-{}/_cacache", std::process::id()));
        file(&outside.join("x"), 1);
        assert!(check_remove(&outside, &npm, &e).unwrap_err().contains("outside"));
        std::fs::remove_dir_all(outside.parent().unwrap()).ok();

        // A link that points a removable name somewhere else is refused.
        #[cfg(unix)]
        {
            let real = home.join("Precious");
            file(&real.join("thesis.tex"), 1);
            let link = home.join("Library/Caches");
            std::fs::create_dir_all(&link).unwrap();
            std::os::unix::fs::symlink(&real, link.join("pip")).unwrap();
            assert!(check_remove(&link.join("pip"), &npm, &e).is_err());
            assert!(real.join("thesis.tex").exists());
        }

        // A project folder: refused once a session starts there.
        let web = home.join("web");
        file(&web.join("package.json"), 2);
        file(&web.join(".next/a"), 1);
        let kind = DirKind::Project { root: web.clone() };
        assert_eq!(check_remove(&web.join(".next"), &kind, &e), Ok(()));
        e.live = vec![web.clone()];
        assert!(check_remove(&web.join(".next"), &kind, &e).unwrap_err().contains("session"));
        // A session in the home folder itself doesn't make every project live.
        e.live = vec![home.clone()];
        assert_eq!(check_remove(&web.join(".next"), &kind, &e), Ok(()));
    }

    #[test]
    fn running_an_item_deletes_only_after_the_checks() {
        let t = tree("run");
        let home = &t.0;
        let mut e = env(home, 0.0, &[]);
        let cache = home.join(".npm/_cacache");
        file(&cache.join("blob"), 10);
        let item = Item {
            id: "npm-cache".into(),
            tier: Tier::A,
            what: "npm cache".into(),
            bytes: 10,
            action: Action::RemoveDir { path: cache.clone(), kind: DirKind::Cache { tools: &["npm"] } },
            note: String::new(),
        };
        let counter = std::cell::Cell::new(100u64);
        let measure = || {
            counter.set(counter.get() + 50);
            Some(counter.get())
        };
        e.running.insert("npm".into());
        assert_eq!(run_item(&item, &e, &measure), Outcome::Kept("npm is running".into()));
        assert!(cache.exists());
        e.running.clear();
        assert_eq!(run_item(&item, &e, &measure), Outcome::Done { freed: 50 });
        assert!(!cache.exists() && home.join(".npm").exists());
        let report = Item { action: Action::None, ..item };
        assert!(matches!(run_item(&report, &e, &measure), Outcome::Kept(_)));
    }

    #[test]
    fn dir_size_counts_hard_links_once_and_skips_symlinks() {
        let t = tree("size");
        file(&t.0.join("d/a"), 100_000);
        #[cfg(unix)]
        {
            std::fs::hard_link(t.0.join("d/a"), t.0.join("d/b")).unwrap();
            std::os::unix::fs::symlink(std::env::temp_dir(), t.0.join("d/link")).unwrap();
        }
        let n = dir_size(&t.0.join("d"));
        assert!((100_000..200_000).contains(&n), "{n}");
    }
}

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
/// A worktree changed more recently than this may have a subagent working in it.
const WORKTREE_QUIET: Duration = Duration::from_secs(3600);
/// An automatic cleanup runs at most this often.
const AUTO_EVERY: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    A,
    B,
    C,
}

impl Tier {
    pub fn key(self) -> &'static str {
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
    /// `git worktree remove` (which itself refuses a worktree with changes).
    GitWorktreeRemove { repo: PathBuf, path: PathBuf },
    /// `docker volume rm` of one volume no container uses.
    DockerVolumeRm(String),
    /// `docker rm` of one stopped container, by id.
    DockerContainerRm(String),
    /// Report only.
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DirKind {
    /// A package or tool cache; skipped while one of `tools` is running.
    Cache { tools: &'static [&'static str] },
    /// Build output or dependencies inside a project folder.
    Project { root: PathBuf },
    /// A backup copy that sits next to the folder it copies.
    Backup { original: PathBuf },
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
    /// Working folders of running processes, so a subagent busy in a worktree keeps it.
    pub cwds: Vec<PathBuf>,
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
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_cwd(sysinfo::UpdateKind::Always));
        let running = sys
            .processes()
            .values()
            .map(|p| p.name().to_string_lossy().to_lowercase().trim_end_matches(".exe").to_string())
            .collect();
        let cwds = sys.processes().values().filter_map(|p| p.cwd().map(Path::to_path_buf)).collect();
        Env {
            home: dirs::home_dir().unwrap_or_default(),
            temp: std::env::temp_dir(),
            now: crate::now(),
            live,
            projects,
            running,
            cwds,
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

/// Whether a session or any process is working inside `path`.
fn in_use(path: &Path, env: &Env) -> bool {
    env.live.iter().chain(&env.cwds).any(|l| l.starts_with(path))
}

/// Whether git says the folder is ignored, meaning it holds nothing committed.
fn git_ignored(path: &Path, env: &Env) -> bool {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else { return false };
    let parent = parent.to_string_lossy();
    (env.run)("git", &["-C", &parent, "check-ignore", "-q", name], Duration::from_secs(5)).is_some()
}

/// The name a backup folder copies, when `name` looks like a backup: `src.bak-1119` → `src`,
/// `app-backup` → `app`, `site_old` → `site`.
fn backup_base(name: &str) -> Option<&str> {
    let lower = name.to_ascii_lowercase();
    let cut = [".bak", "-bak", "_bak", " bak", ".backup", "-backup", "_backup", " backup", "-old", "_old", ".old", " copy"]
        .iter()
        .filter_map(|m| lower.find(m))
        .min()?;
    (cut > 0).then(|| &name[..cut])
}

/// Days since a folder last changed.
fn age_days(path: &Path) -> u64 {
    std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map_or(0, |d| d.as_secs() / 86_400)
}

/// Backup copies next to their original, in project folders and the folders that hold them.
fn backup_items(roots: &[(PathBuf, f64)], env: &Env) -> Vec<Item> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for (root, _) in roots {
        for d in [Some(root.as_path()), root.parent()].into_iter().flatten() {
            if d != env.home && d.starts_with(&env.home) && !dirs.iter().any(|x| x == d) {
                dirs.push(d.to_path_buf());
            }
        }
    }
    let mut found: Vec<(Item, PathBuf, u64)> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if !ft.is_dir() || ft.is_symlink() {
                continue;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(base) = backup_base(&name) else { continue };
            let original = dir.join(base);
            let path = e.path();
            if !original.is_dir() || env.live.iter().any(|l| l.starts_with(&path)) {
                continue;
            }
            let bytes = dir_size(&path);
            if bytes < MIN_BYTES {
                continue;
            }
            let age = age_days(&path);
            let item = Item {
                id: path.display().to_string(),
                tier: Tier::C,
                what: format!("backup of {base}"),
                bytes,
                action: Action::RemoveDir { path, kind: DirKind::Backup { original: original.clone() } },
                note: format!("changed {age} days ago"),
            };
            found.push((item, original, age));
        }
    }
    // Of several copies of one folder, the newest is marked so it is kept.
    let mut out = Vec::new();
    for (i, (item, original, age)) in found.iter().enumerate() {
        let mut item = item.clone();
        let copies: Vec<&(Item, PathBuf, u64)> = found.iter().filter(|(_, o, _)| o == original).collect();
        let newest = copies.iter().map(|(_, _, a)| *a).min() == Some(*age) && found.iter().position(|(_, o, a)| o == original && a == age) == Some(i);
        if copies.len() > 1 && newest {
            item.note.push_str(" · the newest copy");
        }
        out.push(item);
    }
    out
}

/// Claude Code worktrees under each project's `.claude/worktrees`: merged and clean ones can go
/// (tier B); ones with changes or unmerged commits are reported so they aren't forgotten.
fn worktree_items(roots: &[(PathBuf, f64)], env: &Env) -> Vec<Item> {
    let git = |dir: &Path, args: &[&str]| {
        let d = dir.to_string_lossy();
        let mut a = vec!["-C", d.as_ref()];
        a.extend_from_slice(args);
        (env.run)("git", &a, Duration::from_secs(10)).map(|s| s.trim().to_string())
    };
    let mut out = Vec::new();
    for (root, _) in roots {
        // A session in the repo may have subagents working in its worktrees.
        if is_live(root, env) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(root.join(".claude/worktrees")) else { continue };
        let default = git(root, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).filter(|b| !b.is_empty()).unwrap_or_else(|| "main".into());
        for e in entries.flatten() {
            let path = e.path();
            // A subagent working in a worktree is not a session of its own, so recent changes count as use.
            let recent = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|a| a < WORKTREE_QUIET);
            if !e.file_type().is_ok_and(|t| t.is_dir()) || recent || in_use(&path, env) {
                continue;
            }
            let Some(status) = git(&path, &["status", "--porcelain"]) else { continue };
            let branch = match git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default() {
                b if b == "HEAD" || b.is_empty() => "(detached)".to_string(),
                b => format!("branch {b}"),
            };
            let head = git(&path, &["rev-parse", "HEAD"]).unwrap_or_default();
            let merged = !head.is_empty() && git(root, &["merge-base", "--is-ancestor", &head, &default]).is_some();
            let bytes = dir_size(&path);
            if bytes < MIN_BYTES {
                continue;
            }
            let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let (tier, action, note) = if status.is_empty() && merged {
                (Tier::B, Action::GitWorktreeRemove { repo: root.clone(), path: path.clone() }, format!("{branch} is merged into {default}, no changes"))
            } else if !status.is_empty() {
                (Tier::C, Action::None, format!("{branch} has uncommitted changes; kept"))
            } else {
                (Tier::C, Action::None, format!("{branch} has commits not in {default}; kept"))
            };
            out.push(Item { id: path.display().to_string(), tier, what: format!("Claude Code worktree in {name}"), bytes, action, note });
        }
    }
    out
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
            _ => continue,
        };
        if bytes >= MIN_BYTES {
            out.push(Item { id: id.into(), tier, what: what.into(), bytes, action, note: note.clone() });
        }
    }
    out.extend(stopped_containers(env));
    // Each volume no container uses, named, since one may hold a database.
    let Some(text) = (env.run)("docker", &["system", "df", "-v", "--format", "{{json .}}"], Duration::from_secs(15)) else { return out };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) else { return out };
    for vol in v["Volumes"].as_array().into_iter().flatten() {
        let name = vol["Name"].as_str().unwrap_or("");
        let bytes = parse_size(vol["Size"].as_str().unwrap_or(""));
        if vol["Links"].as_str() != Some("0") || !valid_volume_name(name) || bytes < MIN_BYTES {
            continue;
        }
        let labels = vol["Labels"].as_str().unwrap_or("");
        let label = |k: &str| labels.split(',').find_map(|kv| kv.strip_prefix(k)?.strip_prefix('=')).unwrap_or("");
        let (project, short) = (label("com.docker.compose.project"), label("com.docker.compose.volume"));
        let what = if project.is_empty() { format!("Docker volume {name}") } else { format!("Docker volume {short} of {project}") };
        out.push(Item {
            id: format!("docker-volume:{name}"),
            tier: Tier::C,
            what,
            bytes,
            action: Action::DockerVolumeRm(name.to_string()),
            note: "no container uses it; may hold a database".into(),
        });
    }
    out
}

/// How long a container must have been stopped before it is offered.
const CONTAINER_STOPPED_SECS: f64 = 7.0 * 86_400.0;

/// Each container stopped for over a week, one item apiece: its own writable layer can hold data
/// (a database kept without a volume), so they are never pruned together.
fn stopped_containers(env: &Env) -> Vec<Item> {
    let Some(list) = (env.run)("docker", &["ps", "-a", "--filter", "status=exited", "--size", "--format", "{{json .}}"], Duration::from_secs(15)) else {
        return Vec::new();
    };
    let rows: Vec<serde_json::Value> = list.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let ids: Vec<&str> = rows.iter().filter_map(|r| r["ID"].as_str()).filter(|id| valid_container_id(id)).collect();
    if ids.is_empty() {
        return Vec::new();
    }
    // When each one stopped: `docker ps` only says when it was created.
    let mut args = vec!["inspect", "--format", "{{.Id}} {{.State.FinishedAt}}"];
    args.extend(&ids);
    let finished: Vec<(String, f64)> = (env.run)("docker", &args, Duration::from_secs(15))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (id, at) = l.split_once(' ')?;
            Some((id.to_string(), chrono::DateTime::parse_from_rfc3339(at.trim()).ok()?.timestamp() as f64))
        })
        .collect();
    let mut out = Vec::new();
    for r in rows {
        let Some(id) = r["ID"].as_str().filter(|id| valid_container_id(id)) else { continue };
        let Some((_, at)) = finished.iter().find(|(full, _)| full.starts_with(id)) else { continue };
        let bytes = parse_size(r["Size"].as_str().unwrap_or(""));
        let days = (env.now - at) / 86_400.0;
        if env.now - at < CONTAINER_STOPPED_SECS || bytes < MIN_BYTES {
            continue;
        }
        let (name, image) = (r["Names"].as_str().unwrap_or(id), r["Image"].as_str().unwrap_or(""));
        out.push(Item {
            id: format!("docker-container:{id}"),
            tier: Tier::C,
            what: format!("stopped Docker container {name} ({image})"),
            bytes,
            action: Action::DockerContainerRm(id.to_string()),
            note: format!("stopped {days:.0} days ago; its own files go with it"),
        });
    }
    out
}

/// Container ids as Docker prints them: hex only, so one can never read as a flag.
fn valid_container_id(id: &str) -> bool {
    (12..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// Docker volume names marsad passes to `docker volume rm`: letters, digits, `_`, `.` and `-`.
fn valid_volume_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('-') && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Everything that could be cleaned, largest first within each tier.
pub fn plan(env: &Env) -> Vec<Item> {
    // Without a real home folder every path check below would pass.
    if !env.home.is_absolute() {
        return Vec::new();
    }
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

    items.extend(worktree_items(&roots, env));
    items.extend(backup_items(&roots, env));
    items.extend(docker_items(env));
    items.sort_by(|a, b| a.tier.cmp(&b.tier).then(b.bytes.cmp(&a.bytes)));
    items
}

// ---------- running ----------

/// The last checks right before a folder is deleted. Returns why it must be kept, if it must.
pub fn check_remove(path: &Path, kind: &DirKind, env: &Env) -> Result<(), String> {
    if !env.home.is_absolute() || !path.is_absolute() {
        return Err("the home folder or the path isn't known for certain".into());
    }
    let meta = std::fs::symlink_metadata(path).map_err(|_| "it no longer exists".to_string())?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err("it is not a plain folder".into());
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let named_right = match kind {
        DirKind::Backup { original } => backup_base(name).is_some_and(|b| path.parent().map(|p| p.join(b)).as_ref() == Some(original)),
        _ => REMOVABLE_NAMES.contains(&name),
    };
    if !named_right {
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
        DirKind::Backup { original } => {
            if !original.is_dir() {
                return Err("the folder it copies is gone, so it may be the only copy".into());
            }
            if env.live.iter().any(|l| l.starts_with(path)) {
                return Err("a session is running in it".into());
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
        Action::DockerVolumeRm(name) => {
            if !valid_volume_name(name) {
                return Outcome::Kept("not a plain volume name".into());
            }
            // Without -f, Docker refuses a volume a container still uses.
            (env.run)("docker", &["volume", "rm", name], Duration::from_secs(60)).map(|_| ()).ok_or_else(|| "docker refused (a container may use it)".to_string())
        }
        Action::DockerContainerRm(id) => {
            if !valid_container_id(id) {
                return Outcome::Kept("not a container id".into());
            }
            // Without -f, Docker refuses a running container.
            (env.run)("docker", &["rm", id], Duration::from_secs(60)).map(|_| ()).ok_or_else(|| "docker refused (it may be running again)".to_string())
        }
        Action::GitWorktreeRemove { repo, path } => {
            if is_live(repo, env) || in_use(path, env) {
                return Outcome::Kept("a session or process is working in it".into());
            }
            let (repo, path) = (repo.to_string_lossy(), path.to_string_lossy());
            // Without --force, git refuses a worktree with changes.
            (env.run)("git", &["-C", &repo, "worktree", "remove", &path], Duration::from_secs(60))
                .map(|_| ())
                .ok_or_else(|| "git refused (the worktree has changes)".to_string())
        }
    };
    match result {
        Ok(()) => Outcome::Done { freed: measure().zip(before).map_or(0, |(after, before)| after.saturating_sub(before)) },
        Err(e) => Outcome::Failed(e),
    }
}

/// Runs one item and records it in the usage history when it ran.
pub fn run_and_record(item: &Item, env: &Env, auto: bool) -> Outcome {
    let home = env.home.clone();
    let measure = move || crate::disk::measure(&home).map(|r| r.available);
    let outcome = run_item(item, env, &measure);
    if let Outcome::Done { freed } = &outcome
        && let Ok(conn) = crate::store::open()
    {
        let path = match &item.action {
            Action::RemoveDir { path, .. } | Action::GitWorktreeRemove { path, .. } => path.display().to_string(),
            Action::DockerVolumeRm(name) | Action::DockerContainerRm(name) => name.clone(),
            _ => String::new(),
        };
        let c = crate::store::Cleanup { item: &item.id, tier: item.tier.key(), path: &path, estimated: item.bytes, freed: *freed, auto };
        let _ = crate::store::record_cleanup(&conn, crate::now(), &c);
    }
    outcome
}

/// Runs the items one at a time.
fn run_all(items: &[&Item], env: &Env, auto: bool) -> Vec<(String, u64, Outcome)> {
    items.iter().map(|item| (item.what.clone(), item.bytes, run_and_record(item, env, auto))).collect()
}

/// Bytes freed in the last 30 days, from the usage history.
pub fn freed_last_30_days(now: f64) -> Option<u64> {
    crate::store::open_readonly().ok().and_then(|c| crate::store::freed_since(&c, now - 30.0 * 86_400.0).ok())
}

/// Turns automatic tier A cleanup on or off.
pub fn set_auto(on: bool) -> anyhow::Result<()> {
    let mut disk = crate::settings::get("disk").and_then(|v| v.as_object().cloned()).unwrap_or_default();
    disk.insert("auto_clean".into(), json!(on));
    crate::settings::set("disk", serde_json::Value::Object(disk))
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

// ---------- daily scan ----------

/// The plan is measured on its own at most this often when the dashboard opens.
const DAILY: Duration = Duration::from_secs(86_400);
/// Smaller totals aren't worth a note.
const HINT_BYTES: u64 = 1_000_000_000;

/// Once a day, measures the plan on a thread and reports what tiers A and B could free, when it
/// is worth a note. None when the last scan was under a day ago.
pub fn daily_scan() -> Option<std::sync::mpsc::Receiver<u64>> {
    let stamp = crate::claude_data_dir().join("clean-scan.stamp");
    let due = std::fs::metadata(&stamp).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_none_or(|age| age >= DAILY);
    if !due || std::fs::create_dir_all(crate::claude_data_dir()).is_err() || std::fs::write(&stamp, b"").is_err() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let total = removable(&plan(&Env::real()));
        if total >= HINT_BYTES {
            let _ = tx.send(total);
        }
    });
    Some(rx)
}

/// What tiers A and B hold together: what can go without looking inside.
fn removable(items: &[Item]) -> u64 {
    items.iter().filter(|i| i.tier != Tier::C).map(|i| i.bytes).sum()
}

// ---------- marsad clean ----------

#[derive(Args)]
pub struct CleanArgs {
    /// Show the plan without running anything (what `marsad clean` does on its own).
    #[arg(long, conflicts_with_all = ["run", "tier_a", "judge", "auto_clean"])]
    plan: bool,
    /// Run these items (ids from the plan); tier C is never run.
    #[arg(long, num_args = 1..)]
    run: Vec<String>,
    /// Run every tier A item (data that is rebuilt automatically).
    #[arg(long)]
    tier_a: bool,
    /// Turn automatic tier A cleanup at the warning level on or off.
    #[arg(long, value_parser = ["on", "off"])]
    auto_clean: Option<String>,
    /// Ask Claude whether each data-holding item (tier C) can go; it only advises.
    #[arg(long)]
    judge: bool,
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
        set_auto(on == "on")?;
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
                Some(i) if i.tier == Tier::C => eprintln!("{id}: holds data; remove it from the dashboard (`D`), one item at a time"),
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

    if a.judge {
        let ask = crate::clean_judge::candidates(&items);
        if ask.is_empty() {
            println!("No data-holding items to ask about.");
        }
        for (id, v) in crate::clean_judge::judge(&ask).map_err(anyhow::Error::msg)? {
            println!("{:<8} {id}\n         {}", v.call, v.reason);
        }
        return Ok(());
    }
    let freed_30d = freed_last_30_days(env.now);
    let verdicts = crate::clean_judge::load();
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
                    "claude": crate::clean_judge::lookup(&verdicts, i).map(|v| json!({ "call": v.call, "reason": v.reason })),
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
                (Tier::C, "C  holds data: removed only one at a time from the dashboard (`D`); `--judge` asks Claude"),
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
                    if let Some(v) = crate::clean_judge::lookup(&verdicts, i) {
                        println!("             Claude: {} · {}", v.call, v.reason);
                    }
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

pub fn auto_label(on: bool) -> String {
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
            cwds: Vec::new(),
            run: Box::new(move |prog, args, _| match prog {
                "git" if !tracked.iter().any(|t| args.contains(t)) => Some(String::new()),
                _ => None,
            }),
        }
    }

    #[test]
    fn the_daily_note_counts_only_what_can_go_without_looking_inside() {
        let item = |tier, bytes| Item { id: String::new(), tier, what: String::new(), bytes, action: Action::None, note: String::new() };
        assert_eq!(removable(&[item(Tier::A, 3), item(Tier::B, 4), item(Tier::C, 100)]), 7);
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
    #[cfg(unix)]
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
            ("docker", Some(&"system")) if !args.contains(&"-v") => Some(
                [
                    r#"{"Type":"Build Cache","Reclaimable":"27.7GB"}"#,
                    r#"{"Type":"Images","Reclaimable":"30.2GB (70%)"}"#,
                    r#"{"Type":"Local Volumes","Reclaimable":"24.5GB (80%)"}"#,
                    r#"{"Type":"Containers","Reclaimable":"0B (0%)"}"#,
                ]
                .join("\n"),
            ),
            ("docker", Some(&"system")) if args.contains(&"-v") => Some(
                r#"{"Volumes":[
                    {"Name":"web_pgdata","Links":"0","Size":"2.1GB","Labels":"com.docker.compose.project=web,com.docker.compose.volume=pgdata"},
                    {"Name":"in_use","Links":"1","Size":"9GB","Labels":""},
                    {"Name":"--evil","Links":"0","Size":"9GB","Labels":""}
                ]}"#
                .into(),
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
            (Tier::C, "docker-volume:web_pgdata".into()),
        ]);
        assert_eq!(items[0].action, Action::Docker(&["builder", "prune", "-a", "-f"]));
        assert_eq!(items[5].what, "Docker volume pgdata of web");
        assert_eq!(items[5].action, Action::DockerVolumeRm("web_pgdata".into()), "one named volume, never a flag");
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
        // Without a home folder nothing is planned or removed.
        let mut lost = env(Path::new(""), 0.0, &[]);
        lost.temp = PathBuf::new();
        assert!(check_remove(&cache, &npm, &lost).is_err());
        assert!(plan(&lost).is_empty());
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
    fn backups_are_found_next_to_their_original_and_the_newest_is_marked() {
        assert_eq!(backup_base("src.bak-1119"), Some("src"));
        assert_eq!(backup_base("App-Backup"), Some("App"));
        assert_eq!(backup_base("site_old"), Some("site"));
        assert_eq!(backup_base("bakery"), None);
        assert_eq!(backup_base(".bak"), None);

        let t = tree("backup");
        let web = t.0.join("Projects/web");
        file(&web.join("src/main.ts"), 10);
        file(&web.join("src.bak-1119/main.ts"), 60_000_000);
        file(&web.join("src.bak-1120/main.ts"), 60_000_000);
        file(&web.join("lib.bak/x"), 60_000_000); // no `lib` next to it: maybe the only copy
        let e = env(&t.0, 0.0, &[]);
        let items = backup_items(&[(web.clone(), 0.0)], &e);
        assert_eq!(items.len(), 2, "{items:?}");
        assert!(items.iter().all(|i| i.tier == Tier::C && i.what == "backup of src"));
        assert_eq!(items.iter().filter(|i| i.note.contains("newest")).count(), 1);

        let path = web.join("src.bak-1119");
        let kind = DirKind::Backup { original: web.join("src") };
        assert_eq!(check_remove(&path, &kind, &e), Ok(()));
        let wrong = DirKind::Backup { original: web.join("other") };
        assert!(check_remove(&path, &wrong, &e).is_err(), "the original must be the folder it is named after");
        std::fs::remove_dir_all(web.join("src")).unwrap();
        assert!(check_remove(&path, &kind, &e).unwrap_err().contains("only copy"));
    }

    #[test]
    #[cfg(unix)]
    fn only_merged_clean_worktrees_can_be_removed() {
        let t = tree("worktrees");
        let repo = t.0.join("Projects/app");
        for w in ["done", "dirty", "ahead", "live", "fresh"] {
            file(&repo.join(format!(".claude/worktrees/{w}/f")), 60_000_000);
            if w != "fresh" {
                let old = std::time::SystemTime::now() - Duration::from_secs(7200);
                std::fs::File::open(repo.join(format!(".claude/worktrees/{w}"))).unwrap().set_modified(old).unwrap();
            }
        }
        let mut e = env(&t.0, 0.0, &[]);
        // A subagent's process works in one; it is no session of its own.
        e.cwds = vec![repo.join(".claude/worktrees/live/src")];
        e.run = Box::new(|prog, args, _| {
            if prog != "git" {
                return None;
            }
            let dir = args[1];
            let wt = dir.rsplit('/').next().unwrap_or("");
            match &args[2..] {
                ["symbolic-ref", ..] => Some("origin/main\n".into()),
                ["status", ..] => Some(if wt == "dirty" { " M f\n".into() } else { String::new() }),
                ["rev-parse", "--abbrev-ref", ..] => Some(format!("{wt}-branch\n")),
                ["rev-parse", "HEAD"] => Some(format!("sha-{wt}\n")),
                ["merge-base", "--is-ancestor", sha, _] => (*sha != "sha-ahead").then(String::new),
                _ => None,
            }
        });
        let mut items = worktree_items(&[(repo.clone(), 0.0)], &e);
        items.sort_by(|a, b| a.id.cmp(&b.id));
        let got: Vec<(String, Tier, bool)> =
            items.iter().map(|i| (i.id.rsplit('/').next().unwrap().to_string(), i.tier, i.action != Action::None)).collect();
        assert_eq!(got, [("ahead".to_string(), Tier::C, false), ("dirty".into(), Tier::C, false), ("done".into(), Tier::B, true)]);
        assert!(items[0].note.contains("not in origin/main"));
        assert!(items[2].note.starts_with("branch done-branch is merged"), "{}", items[2].note);
        // A session in the repo may have subagents in any of its worktrees: none is offered.
        e.live = vec![repo.clone()];
        assert!(worktree_items(&[(repo.clone(), 0.0)], &e).is_empty());
        let item = Item { id: "w".into(), tier: Tier::B, what: "w".into(), bytes: 1, action: Action::GitWorktreeRemove { repo: repo.clone(), path: repo.join(".claude/worktrees/done") }, note: String::new() };
        assert!(matches!(run_item(&item, &e, &|| None), Outcome::Kept(_)), "rechecked before removing");
    }

    #[test]
    fn containers_are_offered_one_by_one_by_when_they_stopped() {
        let t = tree("containers");
        let mut e = env(&t.0, 30.0 * 86_400.0, &[]);
        e.run = Box::new(|prog, args, _| match (prog, args.first()) {
            ("docker", Some(&"ps")) => Some(
                [
                    r#"{"ID":"aaaaaaaaaaaa","Names":"pg","Image":"postgres:16","Size":"900MB (virtual 1.3GB)"}"#,
                    r#"{"ID":"bbbbbbbbbbbb","Names":"fresh","Image":"redis","Size":"200MB (virtual 300MB)"}"#,
                    r#"{"ID":"--rm-all","Names":"x","Image":"x","Size":"9GB"}"#,
                ]
                .join("\n"),
            ),
            // Created long ago both, but `fresh` stopped only yesterday.
            ("docker", Some(&"inspect")) => Some("aaaaaaaaaaaa11 1970-01-02T00:00:00.5Z\nbbbbbbbbbbbb22 1970-01-30T00:00:00Z\n".into()),
            _ => None,
        });
        let items = stopped_containers(&e);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!((items[0].tier, items[0].what.as_str()), (Tier::C, "stopped Docker container pg (postgres:16)"));
        assert_eq!(items[0].action, Action::DockerContainerRm("aaaaaaaaaaaa".into()));
        assert!(!valid_container_id("--rm-all") && !valid_container_id("abc"));
    }

    #[test]
    fn volume_names_are_checked_before_docker_sees_them() {
        assert!(valid_volume_name("web_pgdata-1.2"));
        assert!(!valid_volume_name("-f"));
        assert!(!valid_volume_name("a b"));
        assert!(!valid_volume_name("x;rm"));
        let t = tree("volume");
        let e = env(&t.0, 0.0, &[]);
        let item = Item { id: "v".into(), tier: Tier::C, what: "v".into(), bytes: 1, action: Action::DockerVolumeRm("--all".into()), note: String::new() };
        assert!(matches!(run_item(&item, &e, &|| None), Outcome::Kept(_)));
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

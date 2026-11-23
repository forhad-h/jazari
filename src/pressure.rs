//! Memory and CPU pressure, checked by the guard hook before each tool call and prompt. A machine
//! out of memory swaps until it stops responding, and one buried in load does the same more
//! slowly. Under strain each session is told once to hold off heavy work; at the critical level a
//! heavy command (a build, an install, a container, a test run) is refused with a pointer to
//! `marsad system --wait`, which returns once there is room again. Light work carries on.

use serde_json::{Map, Value};
use std::path::PathBuf;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    #[default]
    Ok,
    Strained,
    Critical,
}

impl Level {
    pub fn key(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Strained => "strained",
            Level::Critical => "critical",
        }
    }

    fn parse(s: &str) -> Option<Level> {
        [Level::Ok, Level::Strained, Level::Critical].into_iter().find(|l| l.key() == s)
    }
}

/// Memory and load at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Load {
    pub mem_total: u64,
    pub mem_available: u64,
    pub swap_used: u64,
    /// One-minute load average (0 where the platform has none).
    pub load1: f64,
    pub cores: usize,
}

impl Load {
    /// Memory counters and the load average: well under a millisecond, so the hook can afford it.
    pub fn read() -> Option<Load> {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory_specifics(sysinfo::MemoryRefreshKind::everything());
        let mem_total = sys.total_memory();
        (mem_total > 0).then(|| Load {
            mem_total,
            mem_available: sys.available_memory(),
            swap_used: sys.used_swap(),
            load1: sysinfo::System::load_average().one,
            cores: std::thread::available_parallelism().map_or(1, |n| n.get()),
        })
    }

    pub fn mem_pct(&self) -> f64 {
        if self.mem_total == 0 { 0.0 } else { self.mem_total.saturating_sub(self.mem_available) as f64 * 100.0 / self.mem_total as f64 }
    }

    /// `memory 96% used, swap 6.1 GiB · load 31.2 on 10 cores`
    pub fn describe(&self) -> String {
        let mut s = format!("memory {:.0}% used, swap {:.1} GiB", self.mem_pct(), self.swap_used as f64 / GIB);
        if self.load1 > 0.0 {
            s.push_str(&format!(" · load {:.1} on {} cores", self.load1, self.cores));
        }
        s
    }
}

/// When memory and load count as strained or critical (settings key `resources`).
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    pub mem_strained_pct: f64,
    pub mem_critical_pct: f64,
    /// Swap in use, as a share of RAM, that must come with critical memory: a full page cache
    /// alone is not pressure, swapping is.
    pub swap_critical_ratio: f64,
    pub load_strained_per_core: f64,
    pub load_critical_per_core: f64,
    /// Whether heavy commands are refused at the critical level; notes are sent either way.
    pub guard: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { mem_strained_pct: 90.0, mem_critical_pct: 95.0, swap_critical_ratio: 0.25, load_strained_per_core: 2.0, load_critical_per_core: 4.0, guard: true }
    }
}

impl Limits {
    pub fn from_settings(v: Option<&Value>) -> Self {
        let empty = Map::new();
        let m = v.and_then(Value::as_object).unwrap_or(&empty);
        let mut l = Limits::default();
        let num = |k: &str| m.get(k).and_then(Value::as_f64).filter(|n| *n > 0.0);
        if let Some(n) = num("memory_strained_pct") {
            l.mem_strained_pct = n;
        }
        if let Some(n) = num("memory_critical_pct") {
            l.mem_critical_pct = n;
        }
        if let Some(n) = num("load_strained_per_core") {
            l.load_strained_per_core = n;
        }
        if let Some(n) = num("load_critical_per_core") {
            l.load_critical_per_core = n;
        }
        if let Some(b) = m.get("guard").and_then(Value::as_bool) {
            l.guard = b;
        }
        l
    }

    pub fn load() -> Self {
        Self::from_settings(crate::settings::get("resources").as_ref())
    }

    pub fn level(&self, l: &Load) -> Level {
        let cores = l.cores.max(1) as f64;
        let swapping = l.swap_used as f64 >= l.mem_total as f64 * self.swap_critical_ratio;
        if (l.mem_pct() >= self.mem_critical_pct && swapping) || l.load1 >= cores * self.load_critical_per_core {
            Level::Critical
        } else if l.mem_pct() >= self.mem_strained_pct || l.load1 >= cores * self.load_strained_per_core {
            Level::Strained
        } else {
            Level::Ok
        }
    }
}

/// Programs and subcommands that take a lot of memory or CPU for a while.
pub fn heavy(cmd: &str) -> bool {
    let words: Vec<&str> = cmd.split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')')).filter(|w| !w.is_empty()).collect();
    words.iter().enumerate().any(|(i, w)| {
        let prog = w.rsplit('/').next().unwrap_or(w);
        let next = words.get(i + 1).copied().unwrap_or("");
        let after = words.get(i + 2).copied().unwrap_or("");
        match prog {
            "cargo" => matches!(next, "build" | "b" | "test" | "t" | "run" | "r" | "check" | "c" | "clippy" | "install" | "bench" | "doc"),
            "npm" | "pnpm" | "yarn" | "bun" => {
                matches!(next, "install" | "i" | "ci" | "add" | "build" | "test" | "t") || (next == "run" && matches!(after, "build" | "test" | "dev" | "e2e" | "lint"))
            }
            "npx" | "pnpx" | "bunx" => matches!(next, "next" | "vite" | "webpack" | "tsc" | "jest" | "vitest" | "playwright" | "turbo" | "nx"),
            "docker" | "podman" => matches!(next, "build" | "pull" | "run" | "compose" | "buildx"),
            "docker-compose" => matches!(next, "up" | "build" | "pull"),
            "go" => matches!(next, "build" | "test" | "install"),
            "pip" | "pip3" => next == "install",
            "uv" => matches!(next, "sync" | "pip" | "add") || (next == "run" && !after.is_empty()),
            "brew" => matches!(next, "install" | "upgrade" | "reinstall"),
            "swift" => matches!(next, "build" | "test"),
            "make" | "gmake" | "cmake" | "ninja" | "bazel" | "xcodebuild" | "gradle" | "gradlew" | "mvn" | "pytest" | "jest" | "vitest" | "tsc" | "webpack" | "ffmpeg" | "rustc" => true,
            _ => false,
        }
    })
}

/// The heavy work a tool call would start, if any: only Bash runs local programs.
fn heavy_call(tool: &str, input: &Value) -> bool {
    tool == "Bash" && input["command"].as_str().is_some_and(heavy)
}

/// What the hook does about pressure for one call: a note for Claude, and a refusal for a heavy
/// command at the critical level. Returns them with the level to remember for the session.
pub fn decide(limits: &Limits, load: &Load, event: &str, tool: &str, input: &Value, warned: Level) -> (Option<String>, Option<String>, Level) {
    let level = limits.level(load);
    // Back to normal re-arms the note; a rise to a higher level sends it again.
    let warned = if level == Level::Ok { Level::Ok } else { warned };
    let note = (level > warned).then(|| {
        format!(
            "[marsad] The machine is {} ({}). Hold off on heavy work (builds, test runs, installs, containers) until it eases: \
             `marsad system` shows the state, and `marsad system --wait` returns once there is room. Light work can go on.",
            if level == Level::Critical { "under heavy pressure" } else { "busy" },
            load.describe(),
        )
    });
    let refuse = (limits.guard && level == Level::Critical && event == "PreToolUse" && heavy_call(tool, input)).then(|| {
        format!(
            "[marsad] Not now: the machine is under heavy pressure ({}), and this command would add to it until the machine stops \
             responding. Run `marsad system --wait` (it returns once memory and CPU have room), then try again. Don't work around this.",
            load.describe(),
        )
    });
    (note, refuse, level.max(warned))
}

fn warned_dir() -> PathBuf {
    crate::claude_data_dir().join("pressure-warned")
}

/// The pressure part of the guard hook: (note, refusal). Every failure means neither.
pub fn hook(raw: &Value, event: &str, session: &str) -> (Option<String>, Option<String>) {
    let Some(load) = Load::read() else { return (None, None) };
    let limits = Limits::load();
    let marker = warned_dir().join(session);
    let was = std::fs::read_to_string(&marker).ok().and_then(|s| Level::parse(s.trim())).unwrap_or_default();
    let tool = raw["tool_name"].as_str().unwrap_or("");
    let (note, refuse, warned) = decide(&limits, &load, event, tool, &raw["tool_input"], was);
    if warned != was {
        if warned == Level::Ok {
            let _ = std::fs::remove_file(&marker);
        } else if std::fs::create_dir_all(warned_dir()).is_ok() {
            let _ = std::fs::write(&marker, warned.key());
        }
    }
    (note, refuse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn load(mem_pct: f64, swap_gib: f64, load1: f64) -> Load {
        let total = 16 * GIB as u64;
        Load { mem_total: total, mem_available: (total as f64 * (100.0 - mem_pct) / 100.0) as u64, swap_used: (swap_gib * GIB) as u64, load1, cores: 10 }
    }

    #[test]
    fn levels_need_swapping_for_critical_memory() {
        let l = Limits::default();
        assert_eq!(l.level(&load(60.0, 0.0, 3.0)), Level::Ok);
        assert_eq!(l.level(&load(92.0, 0.0, 3.0)), Level::Strained);
        assert_eq!(l.level(&load(97.0, 1.0, 3.0)), Level::Strained, "a full cache is not pressure without swapping");
        assert_eq!(l.level(&load(97.0, 6.0, 3.0)), Level::Critical);
        assert_eq!(l.level(&load(50.0, 0.0, 25.0)), Level::Strained, "load over twice the cores");
        assert_eq!(l.level(&load(50.0, 0.0, 45.0)), Level::Critical);
    }

    #[test]
    fn heavy_commands_are_told_apart_from_light_ones() {
        for c in ["cargo build --release", "npm ci", "pnpm install", "docker build .", "cd web && npm run build", "make -j8", "pytest -x", "uv sync", "/usr/bin/xcodebuild -scheme A"] {
            assert!(heavy(c), "{c}");
        }
        for c in ["git status", "ls -la", "cat Cargo.toml", "npm view react", "docker ps", "cargo --version", "rg build"] {
            assert!(!heavy(c), "{c}");
        }
    }

    #[test]
    fn critical_pressure_refuses_heavy_calls_and_notes_once() {
        let l = Limits::default();
        let bash = |c: &str| json!({ "command": c });
        let hot = load(97.0, 6.0, 3.0);
        let (note, refuse, warned) = decide(&l, &hot, "PreToolUse", "Bash", &bash("cargo build"), Level::Ok);
        assert!(note.unwrap().contains("heavy pressure") && refuse.unwrap().contains("marsad system --wait"));
        assert_eq!(warned, Level::Critical);
        let (note, refuse, _) = decide(&l, &hot, "PreToolUse", "Bash", &bash("git diff"), warned);
        assert_eq!((note, refuse), (None, None), "light work goes on, and the note came once");
        let (_, refuse, _) = decide(&l, &hot, "PreToolUse", "Read", &json!({ "file_path": "/x" }), warned);
        assert!(refuse.is_none());
        let off = Limits { guard: false, ..Limits::default() };
        assert!(decide(&off, &hot, "PreToolUse", "Bash", &bash("cargo build"), Level::Ok).1.is_none(), "guard off only notes");
        // Back to normal re-arms the note.
        assert_eq!(decide(&l, &load(40.0, 0.0, 1.0), "PreToolUse", "Bash", &bash("ls"), Level::Critical), (None, None, Level::Ok));
        let (note, refuse, _) = decide(&l, &load(92.0, 0.0, 1.0), "UserPromptSubmit", "", &Value::Null, Level::Ok);
        assert!(note.unwrap().contains("busy") && refuse.is_none());
    }

    #[test]
    fn settings_override_the_limits() {
        let l = Limits::from_settings(Some(&json!({ "memory_strained_pct": 80, "guard": false, "load_critical_per_core": -1 })));
        assert_eq!((l.mem_strained_pct, l.guard, l.load_critical_per_core), (80.0, false, 4.0));
    }
}

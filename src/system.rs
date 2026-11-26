//! Disk, memory and CPU in one reading, for the dashboard's meters and `jazari watch system`, so a
//! session (or the user) can tell whether the machine has room for heavy work.

use crate::disk::{Level, Reading};
use crate::report::Format;
use clap::Args;
use serde_json::json;
use std::path::{Path, PathBuf};
use sysinfo::{MemoryRefreshKind, System};

const GB: f64 = 1_000_000_000.0;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// The fullest volume holding the home folder or a live session's folder, and its level.
    pub disk: Option<(Level, Reading)>,
    pub mem_total: u64,
    pub mem_available: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    /// Busy share of all cores, 0–100; None until two samples are apart.
    pub cpu_pct: Option<f32>,
    /// 1, 5 and 15 minute load averages (all 0 where the platform has none).
    pub load: [f64; 3],
    pub cores: usize,
    /// jazari's own process: CPU (percent of one core) and resident memory in bytes.
    pub own_cpu: Option<f32>,
    pub own_mem: u64,
    /// Size of jazari's data folder on disk, re-measured every minute.
    pub own_data: Option<u64>,
}

impl Snapshot {
    pub fn mem_used_ratio(&self) -> f64 {
        if self.mem_total == 0 { 0.0 } else { self.mem_total.saturating_sub(self.mem_available) as f64 / self.mem_total as f64 }
    }

    /// Short verdicts a session can act on, worst first; empty when there is room for heavy work.
    pub fn concerns(&self, t: &crate::disk::Thresholds) -> Vec<String> {
        let mut out = Vec::new();
        if let Some((level, r)) = &self.disk
            && *level >= Level::Notice
        {
            let at = t.limit(Level::Critical).map(|l| format!("; sessions stop at {:.0}% or {:.0} GB free", l.pct, l.free_gb)).unwrap_or_default();
            out.push(format!("disk {}: {:.0}% full, {:.1} GB free{at}", level.key(), r.used_pct(), r.free_gb()));
        }
        let mem = self.mem_used_ratio();
        if mem >= 0.90 {
            out.push(format!("memory {:.0}% used, {:.1} GiB available", mem * 100.0, self.mem_available as f64 / GIB));
        }
        // Swap that has grown past half the RAM means the machine is paging hard, and on a small
        // disk the swap files are what fills it.
        if self.mem_total > 0 && self.swap_used as f64 > self.mem_total as f64 * 0.5 {
            out.push(format!("swap {:.1} GiB in use", self.swap_used as f64 / GIB));
        }
        if self.cores > 0 && self.load[0] > self.cores as f64 * 2.0 {
            out.push(format!("load {:.1} on {} cores", self.load[0], self.cores));
        }
        out
    }
}

/// Keeps the CPU counters between readings: CPU use is the change since the previous refresh.
pub struct Sampler {
    sys: System,
    primed: bool,
    data: Option<(std::time::Instant, u64)>,
}

impl Default for Sampler {
    fn default() -> Self {
        Sampler { sys: System::new(), primed: false, data: None }
    }
}

/// How often the data folder's size is walked again.
const DATA_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

impl Sampler {
    pub fn sample<'a>(&mut self, paths: impl IntoIterator<Item = &'a Path>, t: &crate::disk::Thresholds) -> Snapshot {
        self.sys.refresh_memory_specifics(MemoryRefreshKind::everything());
        self.sys.refresh_cpu_usage();
        let cpu = self.primed.then(|| self.sys.global_cpu_usage());
        let pid = sysinfo::Pid::from_u32(std::process::id());
        self.sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid]),
            true,
            sysinfo::ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        let own = self.sys.process(pid);
        let own_cpu = own.filter(|_| self.primed).map(|p| p.cpu_usage());
        let own_mem = own.map_or(0, |p| p.memory());
        self.primed = true;
        if self.data.is_none_or(|(at, _)| at.elapsed() >= DATA_EVERY) {
            self.data = Some((std::time::Instant::now(), crate::clean::dir_size(&crate::data_dir())));
        }
        let load = System::load_average();
        Snapshot {
            // The fullest volume by statfs (cheap), then that one as Finder shows it.
            disk: crate::disk::worst(paths, t, crate::disk::measure).map(|(_, r)| {
                let r = crate::disk::as_finder_shows(r);
                (t.level(&r), r)
            }),
            mem_total: self.sys.total_memory(),
            mem_available: available_memory(&self.sys),
            swap_total: self.sys.total_swap(),
            swap_used: self.sys.used_swap(),
            cpu_pct: cpu,
            load: [load.one, load.five, load.fifteen],
            cores: self.sys.cpus().len().max(std::thread::available_parallelism().map_or(0, |n| n.get())),
            own_cpu,
            own_mem,
            own_data: self.data.map(|(_, n)| n),
        }
    }
}

/// Memory still free for new work, counted the way Activity Monitor does on macOS: everything
/// except app memory, wired memory and the pages the compressor holds, so cached files count as
/// free. sysinfo's own figure also counts active and compressed pages as free, which on a busy
/// Mac reads several GiB higher than Activity Monitor.
pub fn available_memory(sys: &System) -> u64 {
    #[cfg(target_os = "macos")]
    if let Some(used) = mac_used_memory() {
        return sys.total_memory().saturating_sub(used);
    }
    sys.available_memory()
}

/// Activity Monitor's "Memory Used": app memory (anonymous pages less purgeable ones), wired
/// memory and the compressor's pages. None if the kernel won't answer.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn mac_used_memory() -> Option<u64> {
    use std::sync::OnceLock;
    // One send right to the host port, kept for the life of the process.
    static HOST: OnceLock<libc::mach_port_t> = OnceLock::new();
    let host = *HOST.get_or_init(|| unsafe { libc::mach_host_self() });
    let mut s: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    let r = unsafe { libc::host_statistics64(host, libc::HOST_VM_INFO64, (&raw mut s).cast(), &mut count) };
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if r != libc::KERN_SUCCESS || page <= 0 {
        return None;
    }
    let app = u64::from(s.internal_page_count).saturating_sub(u64::from(s.purgeable_count));
    let pages = app + u64::from(s.wire_count) + u64::from(s.compressor_page_count);
    Some(pages * page as u64)
}

/// The home folder and the given session folders: the places whose volume matters.
pub fn watched_paths(cwds: impl IntoIterator<Item = String>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = dirs::home_dir().into_iter().collect();
    for cwd in cwds {
        let p = PathBuf::from(cwd);
        if !p.as_os_str().is_empty() && !paths.contains(&p) {
            paths.push(p);
        }
    }
    paths
}

#[derive(Args)]
pub struct SystemArgs {
    #[arg(long, value_enum, default_value = "table")]
    format: Format,
    /// Wait until memory and CPU have room again (checks every 5 s), then report.
    #[arg(long)]
    wait: bool,
    /// Give up waiting after this many seconds (default 110, under the Bash tool's 2-minute limit).
    #[arg(long, default_value_t = 110, requires = "wait")]
    timeout: u64,
}

/// `jazari watch system --wait`: returns once memory and CPU are back under strain, or fails after the
/// timeout so the caller can decide (wait again, or go on with light work).
fn wait(timeout: u64) -> anyhow::Result<()> {
    let limits = crate::pressure::Limits::load();
    let start = std::time::Instant::now();
    loop {
        let load = crate::pressure::Load::read().ok_or_else(|| anyhow::anyhow!("couldn't read memory and load"))?;
        let level = limits.level(&load);
        if level == crate::pressure::Level::Ok {
            println!("Room again after {} s: {}.", start.elapsed().as_secs(), load.describe());
            return Ok(());
        }
        if start.elapsed().as_secs() >= timeout {
            anyhow::bail!("still {} after {} s: {}; wait again, or go on with light work only", level.key(), timeout, load.describe());
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
}

pub fn run(a: SystemArgs) -> anyhow::Result<()> {
    if a.wait {
        return wait(a.timeout);
    }
    let cfg = crate::disk::Config::load();
    let sessions = crate::live::load_sessions(false);
    let paths = watched_paths(sessions.into_values().map(|s| s.cwd));
    let mut sampler = Sampler::default();
    sampler.sample(std::iter::empty(), &cfg.thresholds);
    // A one-second window, like `top -s 1`: shorter ones swing with every spike.
    std::thread::sleep(std::time::Duration::from_secs(1));
    let snap = sampler.sample(paths.iter().map(PathBuf::as_path), &cfg.thresholds);
    let concerns = snap.concerns(&cfg.thresholds);
    let round = |x: f64| (x * 10.0).round() / 10.0;
    match a.format {
        Format::Json => {
            let disk = snap.disk.as_ref().map(|(level, r)| {
                json!({
                    "path": r.path.display().to_string(),
                    "used_pct": round(r.used_pct()),
                    "free_gb": round(r.free_gb()),
                    "total_gb": round(r.total as f64 / GB),
                    "level": level.key(),
                })
            });
            let out = json!({
                "ok": concerns.is_empty(),
                "concerns": concerns,
                "disk": disk,
                "guard": cfg.guard,
                "memory": {
                    "used_pct": round(snap.mem_used_ratio() * 100.0),
                    "available_gib": round(snap.mem_available as f64 / GIB),
                    "total_gib": round(snap.mem_total as f64 / GIB),
                    "swap_used_gib": round(snap.swap_used as f64 / GIB),
                    "swap_total_gib": round(snap.swap_total as f64 / GIB),
                },
                "jazari_data_bytes": snap.own_data,
                "cpu": {
                    "used_pct": snap.cpu_pct.map(|c| round(c as f64)),
                    "load": snap.load.map(round),
                    "cores": snap.cores,
                },
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        _ => {
            println!(
                "CPU     {} · load {:.1} {:.1} {:.1} on {} cores",
                snap.cpu_pct.map_or("unknown".into(), |c| format!("{c:.0}% busy")),
                snap.load[0],
                snap.load[1],
                snap.load[2],
                snap.cores
            );
            println!(
                "Memory  {:.0}% used, {:.1} GiB available of {:.1} GiB · swap {:.1} GiB used",
                snap.mem_used_ratio() * 100.0,
                snap.mem_available as f64 / GIB,
                snap.mem_total as f64 / GIB,
                snap.swap_used as f64 / GIB
            );
            match &snap.disk {
                Some((level, r)) => println!(
                    "Disk    {:.0}% full, {:.1} GB free of {:.1} GB · {}",
                    r.used_pct(),
                    r.free_gb(),
                    r.total as f64 / GB,
                    level.key()
                ),
                None => println!("Disk    unknown"),
            }
            if let Some(n) = snap.own_data {
                println!("jazari  data folder {}", crate::clean::fmt_bytes(n));
            }
            if concerns.is_empty() {
                println!("OK: room for heavy work.");
            } else {
                for c in &concerns {
                    println!("Careful: {c}");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> Snapshot {
        Snapshot { mem_total: 16 * GIB as u64, mem_available: 8 * GIB as u64, cores: 8, load: [2.0, 2.0, 2.0], ..Default::default() }
    }

    #[test]
    fn a_roomy_machine_has_no_concerns() {
        let t = crate::disk::Thresholds::default();
        let mut s = snap();
        s.disk = Some((Level::Ok, Reading { system: true, path: "/x".into(), total: 228_000_000_000, available: 105_000_000_000 }));
        assert!(s.concerns(&t).is_empty());
    }

    #[test]
    fn concerns_name_the_disk_memory_swap_and_load() {
        let t = crate::disk::Thresholds::default();
        let mut s = snap();
        let r = Reading { system: true, path: "/x".into(), total: 228_000_000_000, available: 10_000_000_000 };
        s.disk = Some((t.level(&r), r));
        s.mem_available = GIB as u64;
        s.swap_used = 10 * GIB as u64;
        s.load = [20.0, 10.0, 5.0];
        let c = s.concerns(&t);
        assert_eq!(c.len(), 4, "{c:?}");
        assert!(c[0].starts_with("disk critical: 96% full, 10.0 GB free"), "{}", c[0]);
        assert!(c[1].starts_with("memory 94% used"), "{}", c[1]);
    }

    #[test]
    fn samples_this_machine() {
        let mut s = Sampler::default();
        let first = s.sample([std::env::temp_dir().as_path()], &crate::disk::Thresholds::default());
        assert!(first.cpu_pct.is_none(), "the first sample has nothing to compare against");
        assert!(first.disk.is_some() && first.mem_total > 0);
        assert!(s.sample(std::iter::empty(), &crate::disk::Thresholds::default()).cpu_pct.is_some());
    }
}

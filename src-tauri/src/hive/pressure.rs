//! Memory pressure (docs/hive.md §6.8): what may start while the machine is short of memory.
//!
//! On macOS the kernel's `kern.memorystatus_vm_pressure_level` (1 normal, 2 warn, 4 critical) is
//! the authority: at 2 nothing new starts (no queued job, no worker reservation), at 4 the heavy
//! pools also drop to one each. A free-memory percentage gates only where no kernel level exists
//! (Linux, Windows): under 25% free holds, under 10% is critical. sysinfo's `available_memory` on
//! macOS subtracts the compressor from free + inactive + purgeable, which read a steady 11% on a
//! 128 GB Mac at level 1 with `memory_pressure` saying 68% free, and refused every spawn (second
//! live test). So macOS's free % is the kernel's `kern.memorystatus_level`, the very figure
//! `memory_pressure` prints, and it is only recorded. CPU is read as idle %, never load average
//! (spike 8: load 80 with the CPU 67% idle).
//!
//! Nothing here runs on a timer of its own. The run queue samples every `SAMPLE_EVERY` while it
//! holds a job (`queue::ticker`), a spawn reservation reads a fresh sample on demand
//! (`spawn_gate`), and one watch (`until_open`) samples while a refused worker waits for memory
//! (`waiters::poll_when_memory_frees`), so an idle maiTerm never reads it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

pub const SAMPLE_EVERY: Duration = Duration::from_secs(5);
/// What the header says while anything is held back.
pub const THROTTLED: &str = "memory pressure";

const HOLD_LEVEL: u32 = 2;
const CRITICAL_LEVEL: u32 = 4;
const HOLD_FREE_PCT: f64 = 25.0;
const CRITICAL_FREE_PCT: f64 = 10.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Sample {
    /// None off macOS, or when the sysctl can't be read.
    pub level: Option<u32>,
    /// Gates only when `level` is None.
    pub free_pct: Option<f64>,
    /// Recorded for the log, never a gate: idle CPU is not what runs out first.
    pub cpu_idle_pct: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Gate {
    #[default]
    Open,
    /// Start nothing new.
    Hold,
    /// Start nothing new, and every heavy pool's capacity is one.
    Critical,
}

impl Gate {
    pub fn starts(self) -> bool {
        self == Gate::Open
    }

    pub fn throttled(self) -> Option<&'static str> {
        (!self.starts()).then_some(THROTTLED)
    }
}

/// The gate a sample sets: the kernel's level where there is one, else free %. Nothing readable
/// is Open, so a machine maiTerm can't read never holds the queue forever.
pub fn gate_of(s: &Sample) -> Gate {
    match (s.level, s.free_pct) {
        (Some(l), _) if l >= CRITICAL_LEVEL => Gate::Critical,
        (Some(l), _) if l >= HOLD_LEVEL => Gate::Hold,
        (Some(_), _) | (None, None) => Gate::Open,
        (None, Some(f)) if f < CRITICAL_FREE_PCT => Gate::Critical,
        (None, Some(f)) if f < HOLD_FREE_PCT => Gate::Hold,
        (None, Some(_)) => Gate::Open,
    }
}

struct Sampler {
    sys: sysinfo::System,
    last: Option<(Instant, Sample)>,
}

static SAMPLER: Mutex<Option<Sampler>> = Mutex::new(None);

/// Read the machine now. Blocks briefly (a sysctl and sysinfo's memory and CPU refresh).
pub fn sample() -> Sample {
    let mut g = SAMPLER.lock().unwrap_or_else(|e| e.into_inner());
    let s = g.get_or_insert_with(|| Sampler { sys: sysinfo::System::new(), last: None });
    s.sys.refresh_memory();
    // The first refresh has no interval to measure over, so a first sample has no idle %.
    let had_cpu = s.last.is_some();
    s.sys.refresh_cpu_usage();
    let total = s.sys.total_memory();
    let out = Sample {
        level: vm_pressure_level(),
        free_pct: os_free_pct().or_else(|| (total > 0).then(|| s.sys.available_memory() as f64 * 100.0 / total as f64)),
        cpu_idle_pct: had_cpu.then(|| (100.0 - s.sys.global_cpu_usage() as f64).clamp(0.0, 100.0)),
    };
    s.last = Some((Instant::now(), out));
    out
}

/// The last sample if it's fresher than `SAMPLE_EVERY`, else a new one.
pub fn recent() -> Sample {
    {
        let g = SAMPLER.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, s)) = g.as_ref().and_then(|s| s.last) {
            if at.elapsed() < SAMPLE_EVERY {
                return s;
            }
        }
    }
    sample()
}

/// Whether a worker may be reserved a slot now (`fuses::reserve`): under pressure the effective
/// cap is the live count. Reads a sample on demand; blocks briefly.
pub fn spawn_gate() -> Gate {
    gate_of(&recent())
}

/// At most one watch at a time (`Watch::begin`); whoever begins it ends it.
pub struct Watch(AtomicBool);

impl Watch {
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// True when this caller starts the watch; false when one is already running.
    pub fn begin(&self) -> bool {
        !self.0.swap(true, Ordering::SeqCst)
    }

    pub fn end(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Default for Watch {
    fn default() -> Self {
        Self::new()
    }
}

/// Waits until `gate` lets starts through, reading it every `every` (off the async threads: a
/// sample blocks briefly). The first read comes after one interval: the caller was just refused.
pub async fn until_open(gate: fn() -> Gate, every: Duration) {
    loop {
        tokio::time::sleep(every).await;
        if tokio::task::spawn_blocking(gate).await.unwrap_or_default().starts() {
            return;
        }
    }
}

#[cfg(target_os = "macos")]
fn sysctl_int(name: &str) -> Option<i32> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut v: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is NUL-terminated and `v` is an int the size we pass.
    let rc = unsafe { libc::sysctlbyname(name.as_ptr(), &mut v as *mut _ as *mut libc::c_void, &mut size, std::ptr::null_mut(), 0) };
    (rc == 0).then_some(v)
}

#[cfg(target_os = "macos")]
fn vm_pressure_level() -> Option<u32> {
    sysctl_int("kern.memorystatus_vm_pressure_level").filter(|l| *l > 0).map(|l| l as u32)
}

/// macOS's "System-wide memory free percentage", as `memory_pressure` prints it.
#[cfg(target_os = "macos")]
fn os_free_pct() -> Option<f64> {
    sysctl_int("kern.memorystatus_level").filter(|p| (0..=100).contains(p)).map(f64::from)
}

#[cfg(not(target_os = "macos"))]
fn vm_pressure_level() -> Option<u32> {
    None
}

/// Elsewhere sysinfo's available memory is the OS's own figure (Linux `MemAvailable`, Windows
/// `ullAvailPhys`).
#[cfg(not(target_os = "macos"))]
fn os_free_pct() -> Option<f64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(level: Option<u32>, free: f64) -> Sample {
        Sample { level, free_pct: Some(free), cpu_idle_pct: Some(50.0) }
    }

    #[test]
    fn the_thresholds_are_the_spikes() {
        assert_eq!(gate_of(&s(Some(1), 60.0)), Gate::Open);
        assert_eq!(gate_of(&s(Some(2), 60.0)), Gate::Hold, "level 2 holds");
        assert_eq!(gate_of(&s(Some(4), 60.0)), Gate::Critical);
        assert_eq!(gate_of(&s(None, 60.0)), Gate::Open, "off macOS, free % alone");
        assert_eq!(gate_of(&s(None, 24.9)), Gate::Hold, "under 25% free holds");
        assert_eq!(gate_of(&s(None, 25.0)), Gate::Open);
        assert_eq!(gate_of(&s(None, 9.0)), Gate::Critical, "under 10% free is critical");
        assert_eq!(gate_of(&Sample::default()), Gate::Open, "nothing readable never holds the queue forever");
        assert!(Gate::Open.starts() && !Gate::Hold.starts() && !Gate::Critical.starts());
        assert_eq!(Gate::Hold.throttled(), Some("memory pressure"));
        assert_eq!(Gate::Open.throttled(), None);
    }

    /// The second live test: level 1 and sysinfo's 11% "available" refused every spawn.
    #[test]
    fn the_kernel_level_decides_whatever_free_memory_reads() {
        for free in [0.0, 5.0, 11.0, 24.0, 68.0] {
            assert_eq!(gate_of(&s(Some(1), free)), Gate::Open, "level 1 at {free}% free");
            assert_eq!(gate_of(&s(Some(2), free)), Gate::Hold, "level 2 at {free}% free");
            assert_eq!(gate_of(&s(Some(4), free)), Gate::Critical, "level 4 at {free}% free");
        }
        assert_eq!(gate_of(&Sample { level: Some(1), free_pct: None, cpu_idle_pct: None }), Gate::Open);
        assert_eq!(gate_of(&Sample { level: Some(2), free_pct: None, cpu_idle_pct: None }), Gate::Hold);
    }

    #[test]
    fn a_sample_reads_the_machine() {
        let a = sample();
        let free = a.free_pct.expect("the OS reports free memory");
        assert!((0.0..=100.0).contains(&free), "{free}");
        if cfg!(target_os = "macos") {
            assert!(matches!(a.level, Some(1 | 2 | 4)), "{:?}", a.level);
            // `memory_pressure`'s own figure is there to read, so sysinfo's is never the one used.
            assert!(os_free_pct().is_some());
        }
        assert_eq!(recent(), sample_or_same(a), "a fresh sample is reused");
    }

    #[test]
    fn one_watch_at_a_time() {
        let w = Watch::new();
        assert!(w.begin(), "the first starts it");
        assert!(!w.begin(), "a second refusal meanwhile joins it");
        w.end();
        assert!(w.begin(), "after it ends, the next refusal starts another");
    }

    static GATE_READS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// Held for two reads, then open.
    fn eases_on_the_third() -> Gate {
        if GATE_READS.fetch_add(1, Ordering::SeqCst) < 2 { Gate::Hold } else { Gate::Open }
    }

    #[tokio::test]
    async fn the_watch_returns_once_the_gate_opens() {
        until_open(eases_on_the_third, Duration::from_millis(1)).await;
        assert_eq!(GATE_READS.load(Ordering::SeqCst), 3, "read until it opened, and not after");
    }

    /// `recent` within `SAMPLE_EVERY` hands back the stored one; a parallel test may have stored
    /// a newer one meanwhile, which is just as fresh.
    fn sample_or_same(a: Sample) -> Sample {
        let g = SAMPLER.lock().unwrap();
        g.as_ref().and_then(|s| s.last).map(|(_, s)| s).unwrap_or(a)
    }
}

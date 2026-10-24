//! Memory pressure (docs/hive.md §6.8): what may start while the machine is short of memory.
//!
//! Read from `kern.memorystatus_vm_pressure_level` (macOS: 1 normal, 2 warn, 4 critical) and the
//! free-memory percentage; CPU is read as idle %, never load average (spike 8: load 80 with the
//! CPU 67% idle). At level 2 or more, or under 25% free, nothing new starts: no queued job and no
//! worker reservation. At level 4, or under 10% free, the heavy pools also drop to one each.
//!
//! Nothing here runs on a timer of its own. The run queue samples every `SAMPLE_EVERY` while it
//! holds a job (`queue::ticker`), and a spawn reservation reads a fresh sample on demand
//! (`spawn_gate`), so an idle maiTerm never reads it.

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
    pub free_pct: Option<f64>,
    /// Recorded for the log, never a gate: idle CPU is not what runs out first.
    pub cpu_idle_pct: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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

/// The gate a sample sets. Off macOS there is no level, so free % alone decides.
pub fn gate_of(s: &Sample) -> Gate {
    let level = s.level.unwrap_or(1);
    let free = s.free_pct.unwrap_or(100.0);
    if level >= CRITICAL_LEVEL || free < CRITICAL_FREE_PCT {
        Gate::Critical
    } else if level >= HOLD_LEVEL || free < HOLD_FREE_PCT {
        Gate::Hold
    } else {
        Gate::Open
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
        free_pct: (total > 0).then(|| s.sys.available_memory() as f64 * 100.0 / total as f64),
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

#[cfg(target_os = "macos")]
fn vm_pressure_level() -> Option<u32> {
    let name = std::ffi::CString::new("kern.memorystatus_vm_pressure_level").ok()?;
    let mut level: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is NUL-terminated and `level` is an int the size we pass.
    let rc = unsafe { libc::sysctlbyname(name.as_ptr(), &mut level as *mut _ as *mut libc::c_void, &mut size, std::ptr::null_mut(), 0) };
    (rc == 0 && level > 0).then_some(level as u32)
}

#[cfg(not(target_os = "macos"))]
fn vm_pressure_level() -> Option<u32> {
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
        assert_eq!(gate_of(&s(Some(1), 24.9)), Gate::Hold, "under 25% free holds");
        assert_eq!(gate_of(&s(Some(1), 25.0)), Gate::Open);
        assert_eq!(gate_of(&s(Some(4), 60.0)), Gate::Critical);
        assert_eq!(gate_of(&s(Some(1), 9.0)), Gate::Critical, "under 10% free is critical");
        assert_eq!(gate_of(&s(None, 60.0)), Gate::Open, "off macOS, free % alone");
        assert_eq!(gate_of(&s(None, 20.0)), Gate::Hold);
        assert_eq!(gate_of(&Sample::default()), Gate::Open, "nothing readable never holds the queue forever");
        assert!(Gate::Open.starts() && !Gate::Hold.starts() && !Gate::Critical.starts());
        assert_eq!(Gate::Hold.throttled(), Some("memory pressure"));
        assert_eq!(Gate::Open.throttled(), None);
    }

    #[test]
    fn a_sample_reads_the_machine() {
        let a = sample();
        let free = a.free_pct.expect("sysinfo reads total memory");
        assert!((0.0..=100.0).contains(&free), "{free}");
        if cfg!(target_os = "macos") {
            assert!(matches!(a.level, Some(1 | 2 | 4)), "{:?}", a.level);
        }
        assert_eq!(recent(), sample_or_same(a), "a fresh sample is reused");
    }

    /// `recent` within `SAMPLE_EVERY` hands back the stored one; a parallel test may have stored
    /// a newer one meanwhile, which is just as fresh.
    fn sample_or_same(a: Sample) -> Sample {
        let g = SAMPLER.lock().unwrap();
        g.as_ref().and_then(|s| s.last).map(|(_, s)| s).unwrap_or(a)
    }
}

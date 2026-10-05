//! Per-process CPU and RSS for one round, read from `/proc` every 250 ms on a
//! dedicated thread. A process belongs to the round when its environment
//! carries the round's `ROOST_BENCH_RUN` marker, or its environment or command
//! line names the round directory (a keeper that scrubbed its env still runs
//! from the round's worker-data). Called by `run` (brackets around scenarios)
//! and `stack::boot` (the leftover sweep after a stop).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::BenchError;
use crate::stack::MARKER_ENV;

mod procfs;

use procfs::{
    classify, clock_ticks_per_sec, contains, read_cmdline, read_environ, read_rss_bytes, read_stat,
};

const SAMPLE_PERIOD: Duration = Duration::from_millis(250);

/// What a sampled process is, by its command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Coord,
    Worker,
    Keeper,
    BrowserRenderer,
    BrowserOther,
    /// The shell and the commands it runs (bash, seq, awk).
    Workload,
}

impl Role {
    pub const ALL: [Self; 6] = [
        Self::Coord,
        Self::Worker,
        Self::Keeper,
        Self::BrowserRenderer,
        Self::BrowserOther,
        Self::Workload,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coord => "coord",
            Self::Worker => "worker",
            Self::Keeper => "keeper",
            Self::BrowserRenderer => "browser_renderer",
            Self::BrowserOther => "browser_other",
            Self::Workload => "workload",
        }
    }
}

/// One role's usage over a bracket.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RoleUsage {
    pub cpu_ms: f64,
    pub rss_peak_bytes: u64,
    pub rss_end_bytes: u64,
}

/// What `Bracket::finish` reports for one scenario.
#[derive(Debug, Clone, Serialize)]
pub struct BracketReport {
    pub name: String,
    pub wall_ms: f64,
    pub roles: BTreeMap<Role, RoleUsage>,
}

/// A pid plus its start time: pids are reused, this pair is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ProcKey {
    pid: u32,
    start_ticks: u64,
}

#[derive(Debug)]
struct ProcRecord {
    role: Role,
    cpu_ticks: u64,
    rss_bytes: u64,
    alive: bool,
}

#[derive(Debug)]
struct OpenBracket {
    start_ticks: HashMap<ProcKey, u64>,
    peak_rss: BTreeMap<Role, u64>,
}

#[derive(Debug)]
struct SamplerState {
    marker_entry: Vec<u8>,
    round_dir: Vec<u8>,
    procs: HashMap<ProcKey, ProcRecord>,
    unrelated: HashSet<ProcKey>,
    brackets: HashMap<u64, OpenBracket>,
    next_bracket: u64,
}

/// The round's sampler; dropping it without `stop` leaves the thread to end
/// at its next tick.
#[derive(Debug)]
pub struct Sampler {
    state: Arc<Mutex<SamplerState>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    clock_ticks_per_sec: f64,
}

/// An open measurement window.
#[derive(Debug)]
pub struct Bracket {
    id: u64,
    name: String,
    started: Instant,
}

impl Sampler {
    pub fn start(marker: &str, round_dir: &Path) -> Result<Self, BenchError> {
        let clock_ticks_per_sec = clock_ticks_per_sec()?;
        let state = Arc::new(Mutex::new(SamplerState {
            marker_entry: format!("{MARKER_ENV}={marker}").into_bytes(),
            round_dir: round_dir.to_string_lossy().into_owned().into_bytes(),
            procs: HashMap::new(),
            unrelated: HashSet::new(),
            brackets: HashMap::new(),
            next_bracket: 0,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("bench-sampler".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        lock(&state).sample();
                        std::thread::sleep(SAMPLE_PERIOD);
                    }
                })
                .map_err(|error| BenchError::io("spawning the sampler thread", error))?
        };
        Ok(Self {
            state,
            stop,
            thread: Some(thread),
            clock_ticks_per_sec,
        })
    }

    pub fn mark(&self, name: &str) -> Bracket {
        let mut state = lock(&self.state);
        state.sample();
        let start_ticks = state
            .procs
            .iter()
            .filter(|(_, record)| record.alive)
            .map(|(key, record)| (*key, record.cpu_ticks))
            .collect();
        let id = state.next_bracket;
        state.next_bracket += 1;
        let mut bracket = OpenBracket {
            start_ticks,
            peak_rss: BTreeMap::new(),
        };
        bracket.observe(&state.procs);
        state.brackets.insert(id, bracket);
        Bracket {
            id,
            name: name.to_string(),
            started: Instant::now(),
        }
    }

    pub fn finish(&self, bracket: Bracket) -> BracketReport {
        let mut state = lock(&self.state);
        state.sample();
        let open = state.brackets.remove(&bracket.id);
        let mut roles: BTreeMap<Role, RoleUsage> = BTreeMap::new();
        for (key, record) in &state.procs {
            let baseline = open
                .as_ref()
                .and_then(|open| open.start_ticks.get(key).copied())
                .unwrap_or(0);
            let usage = roles.entry(record.role).or_default();
            usage.cpu_ms += record.cpu_ticks.saturating_sub(baseline) as f64 * 1000.0
                / self.clock_ticks_per_sec;
            if record.alive {
                usage.rss_end_bytes += record.rss_bytes;
            }
        }
        if let Some(open) = open {
            for (role, peak) in open.peak_rss {
                roles.entry(role).or_default().rss_peak_bytes = peak;
            }
        }
        BracketReport {
            name: bracket.name,
            wall_ms: bracket.started.elapsed().as_secs_f64() * 1000.0,
            roles,
        }
    }

    /// Current RSS per role.
    pub fn rss_now(&self) -> BTreeMap<Role, u64> {
        let mut state = lock(&self.state);
        state.sample();
        let mut rss = BTreeMap::new();
        for record in state.procs.values().filter(|record| record.alive) {
            *rss.entry(record.role).or_insert(0) += record.rss_bytes;
        }
        rss
    }

    /// Every live process of this round, with its command line, freshly scanned.
    pub fn live_pids(&self) -> Vec<u32> {
        let mut state = lock(&self.state);
        state.sample();
        state
            .procs
            .iter()
            .filter(|(_, record)| record.alive)
            .map(|(key, _)| key.pid)
            .collect()
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("the sampler thread panicked");
        }
    }
}

impl OpenBracket {
    fn observe(&mut self, procs: &HashMap<ProcKey, ProcRecord>) {
        let mut totals: BTreeMap<Role, u64> = BTreeMap::new();
        for record in procs.values().filter(|record| record.alive) {
            *totals.entry(record.role).or_insert(0) += record.rss_bytes;
        }
        for (role, total) in totals {
            let peak = self.peak_rss.entry(role).or_insert(0);
            *peak = (*peak).max(total);
        }
    }
}

impl SamplerState {
    fn sample(&mut self) {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return;
        };
        let mut seen = HashSet::new();
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let Some(stat) = read_stat(pid) else {
                continue;
            };
            let key = ProcKey {
                pid,
                start_ticks: stat.start_ticks,
            };
            seen.insert(key);
            if self.unrelated.contains(&key) {
                continue;
            }
            let Some(cmdline) = read_cmdline(pid) else {
                continue;
            };
            let known = self.procs.contains_key(&key);
            if !known && !self.belongs(pid, &cmdline) {
                self.unrelated.insert(key);
                continue;
            }
            // Re-classified every tick: a forked child is classified by its
            // parent's command line until it execs.
            let role = classify(&cmdline, &stat.comm);
            let rss_bytes = read_rss_bytes(pid).unwrap_or(0);
            self.procs.insert(
                key,
                ProcRecord {
                    role,
                    cpu_ticks: stat.cpu_ticks,
                    rss_bytes,
                    alive: true,
                },
            );
        }
        for (key, record) in &mut self.procs {
            if !seen.contains(key) {
                record.alive = false;
            }
        }
        self.unrelated.retain(|key| seen.contains(key));
        for bracket in self.brackets.values_mut() {
            bracket.observe(&self.procs);
        }
    }

    fn belongs(&self, pid: u32, cmdline: &[String]) -> bool {
        let round_dir = String::from_utf8_lossy(&self.round_dir);
        if cmdline.iter().any(|arg| arg.contains(round_dir.as_ref())) {
            return true;
        }
        let Some(environ) = read_environ(pid) else {
            return false;
        };
        environ
            .split(|byte| *byte == 0)
            .any(|entry| entry == self.marker_entry.as_slice() || contains(entry, &self.round_dir))
    }
}

fn lock(state: &Mutex<SamplerState>) -> MutexGuard<'_, SamplerState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

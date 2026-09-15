//! Live CPU / memory / network readouts for the Titan.DS panels.
//!
//! The payload shape has to match what the web app parses, including the field
//! names, so this mirrors what the original desktop app produced: network bars
//! are drawn relative to the highest throughput seen so far in this session.

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sysinfo::{MemoryRefreshKind, Networks, RefreshKind, System};

const MIN_INTERVAL: Duration = Duration::from_millis(2011);
const UNITS: [&str; 5] = ["kb/s", "mb/s", "gb/s", "tb/s", "Zb/s"];

#[derive(Clone, Serialize)]
pub struct Rate {
    pub value: String,
    pub unit: &'static str,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub cpu_current_load_percentage: f64,
    pub mem_used_percentage: f64,
    pub rx_sec_percentage: f64,
    pub tx_sec_percentage: f64,
    pub rx_sec_bytes: Rate,
    pub tx_sec_bytes: Rate,
    pub time: u64,
}

struct Collector {
    system: System,
    networks: Networks,
    last_read: Option<Instant>,
    peak_rx: f64,
    peak_tx: f64,
    cached: Option<Snapshot>,
}

pub struct Stats {
    inner: Mutex<Collector>,
}

fn round_tenth(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn to_rate(bytes_per_second: f64) -> Rate {
    let mut value = bytes_per_second * 8.0 / 1e3;
    let mut unit = 0;

    while value >= 1e3 && unit < UNITS.len() - 1 {
        value /= 1e3;
        unit += 1;
    }

    if unit == UNITS.len() - 1 {
        return Rate { value: "999".into(), unit: UNITS[unit] };
    }

    let text = if value < 1.0 {
        format!("{value:.3}")
    } else if value < 10.0 {
        format!("{value:.2}")
    } else if value < 100.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    };

    Rate { value: text, unit: UNITS[unit] }
}

impl Stats {
    pub fn new() -> Self {
        let refresh = RefreshKind::nothing()
            .with_cpu(sysinfo::CpuRefreshKind::nothing().with_cpu_usage())
            .with_memory(MemoryRefreshKind::nothing().with_ram());

        Self {
            inner: Mutex::new(Collector {
                system: System::new_with_specifics(refresh),
                networks: Networks::new_with_refreshed_list(),
                last_read: None,
                peak_rx: 1.0,
                peak_tx: 1.0,
                cached: None,
            }),
        }
    }

    /// Returns a fresh snapshot, or the cached one while it is recent enough.
    pub fn read(&self) -> Option<Snapshot> {
        let mut state = self.inner.lock().ok()?;

        if let (Some(last), Some(cached)) = (state.last_read, state.cached.clone()) {
            if last.elapsed() < MIN_INTERVAL {
                return Some(cached);
            }
        }

        let elapsed = state.last_read.map(|last| last.elapsed().as_secs_f64()).unwrap_or(1.0).max(0.1);

        state.system.refresh_cpu_usage();
        state.system.refresh_memory();
        state.networks.refresh(true);

        let cpu = state.system.global_cpu_usage() as f64;
        let total = state.system.total_memory() as f64;
        let used = state.system.used_memory() as f64;

        let (received, transmitted) = state
            .networks
            .iter()
            .filter(|(name, _)| !is_loopback(name))
            .fold((0.0, 0.0), |(rx, tx), (_, data)| {
                (rx + data.received() as f64, tx + data.transmitted() as f64)
            });

        let rx_per_second = received / elapsed;
        let tx_per_second = transmitted / elapsed;

        if rx_per_second > state.peak_rx {
            state.peak_rx = rx_per_second;
        }
        if tx_per_second > state.peak_tx {
            state.peak_tx = tx_per_second;
        }

        let snapshot = Snapshot {
            cpu_current_load_percentage: round_tenth(cpu),
            mem_used_percentage: if total > 0.0 { round_tenth(used / total * 100.0) } else { 0.0 },
            rx_sec_percentage: round_tenth(rx_per_second / state.peak_rx * 100.0),
            tx_sec_percentage: round_tenth(tx_per_second / state.peak_tx * 100.0),
            rx_sec_bytes: to_rate(rx_per_second),
            tx_sec_bytes: to_rate(tx_per_second),
            time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_millis() as u64)
                .unwrap_or(0),
        };

        state.last_read = Some(Instant::now());
        state.cached = Some(snapshot.clone());

        Some(snapshot)
    }
}

fn is_loopback(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("loopback") || name.starts_with("lo")
}

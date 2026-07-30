//! Logger for interactive mode.
//!
//! `env_logger` and `carbon-log-metrics` both write to stderr. Under a ratatui
//! alternate screen that interleaves with the back buffer and destroys the
//! layout — the panel ends up shredded across the log output.
//!
//! This routes records into an in-memory ring the TUI renders as an events
//! panel, and optionally mirrors everything to a file so nothing is lost.
//! Metrics spam is dropped from the ring (fifteen lines every five seconds
//! would bury the one `SNIPE` line that matters) but still reaches the file.

use {
    log::{Level, Log, Metadata, Record},
    std::{
        collections::VecDeque,
        fs::File,
        io::Write,
        sync::{Arc, Mutex},
    },
};

/// How many lines the events panel keeps.
const RING_CAPACITY: usize = 200;

#[derive(Clone, Default)]
pub struct LogRing(Arc<Mutex<VecDeque<(Level, String)>>>);

impl LogRing {
    /// Most recent `n` lines, oldest first.
    pub fn tail(&self, n: usize) -> Vec<(Level, String)> {
        let guard = match self.0.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let skip = guard.len().saturating_sub(n);
        guard.iter().skip(skip).cloned().collect()
    }

    fn push(&self, level: Level, line: String) {
        let mut guard = match self.0.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.len() >= RING_CAPACITY {
            guard.pop_front();
        }
        guard.push_back((level, line));
    }
}

pub struct TuiLogger {
    ring: LogRing,
    file: Option<Mutex<File>>,
    level: Level,
}

impl TuiLogger {
    /// Install as the global logger. Returns the ring the TUI reads.
    pub fn install(path: Option<&str>, level: Level) -> LogRing {
        let ring = LogRing::default();
        let file = path
            .and_then(|p| File::create(p).ok())
            .map(Mutex::new);
        let logger = TuiLogger {
            ring: ring.clone(),
            file,
            level,
        };
        let _ = log::set_boxed_logger(Box::new(logger));
        log::set_max_level(level.to_level_filter());
        ring
    }
}

/// Metrics fire ~15 lines every 5s. Useful in the file, useless in a panel
/// that exists to surface snipes and failures.
fn is_noise(target: &str) -> bool {
    target.starts_with("carbon_log_metrics")
        || target.starts_with("solana_connection_cache")
        || target.starts_with("solana_quic_client")
}

impl Log for TuiLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!("{}", record.args());
        if let Some(file) = &self.file {
            if let Ok(mut f) = file.lock() {
                let _ = writeln!(f, "[{}] {} {}", record.level(), record.target(), line);
            }
        }
        if !is_noise(record.target()) {
            self.ring.push(record.level(), line);
        }
    }

    fn flush(&self) {
        if let Some(file) = &self.file {
            if let Ok(mut f) = file.lock() {
                let _ = f.flush();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_only_the_most_recent_lines() {
        let ring = LogRing::default();
        for i in 0..(RING_CAPACITY + 50) {
            ring.push(Level::Info, format!("line {i}"));
        }
        let tail = ring.tail(10);
        assert_eq!(tail.len(), 10);
        // Oldest entries must have been evicted, newest retained.
        assert!(tail.last().unwrap().1.contains(&format!("line {}", RING_CAPACITY + 49)));
    }

    #[test]
    fn tail_handles_a_short_ring() {
        let ring = LogRing::default();
        ring.push(Level::Warn, "only one".into());
        assert_eq!(ring.tail(50).len(), 1);
    }

    #[test]
    fn metrics_targets_are_treated_as_noise() {
        // Fifteen lines every five seconds would bury the SNIPE line.
        assert!(is_noise("carbon_log_metrics"));
        assert!(!is_noise("pumpfun_sniper_example::processor"));
        assert!(!is_noise("pumpfun_sniper_example::dispatch"));
    }
}

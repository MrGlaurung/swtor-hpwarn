use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

/// Max rows kept in the HP-change history (oldest dropped once full), per
/// the user's request for a 500-row capped history.
pub const HISTORY_CAP: usize = 500;

/// Max points kept for the HP curve graph. This is event-based (a new point
/// only when HP actually changes), not time-based, so 2000 points covers a
/// very long play session before anything needs to drop off the front.
pub const CURVE_CAP: usize = 2000;

/// One row of the HP-change history: how HP moved, and why if we could tell.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub pct: u8,
    pub cur: u64,
    pub max: u64,
    /// Signed HP delta since the previous known reading.
    pub delta: i64,
    /// Precomputed display text, e.g. "+400 hp from Sayenne Healing Touch
    /// ability" or "-17000 hp from Republic Grunts Critical Smash ability",
    /// or just "-230 hp" when no ability/source could be attributed.
    pub label: String,
}

/// State shared between the background log-watcher thread and the UI
/// thread(s) (overlay + dashboard), inside the process.
pub struct SharedState {
    /// Your current HP percentage (0-100), if known yet.
    pub pct: Mutex<Option<u8>>,
    /// Threshold at/below which the overlay should show (and which triggers
    /// the beep on the downward crossing). Kept live-updatable by the UI,
    /// which reloads it from config.toml.
    pub threshold: AtomicU8,
    /// Log file or directory to follow. The watcher thread notices changes
    /// to this and switches files accordingly.
    pub log_path: Mutex<String>,
    /// Human-readable status for diagnostics, shown in the dashboard.
    pub status: Mutex<String>,
    /// Most recent HP-change rows, newest first, capped at `HISTORY_CAP`.
    pub history: Mutex<VecDeque<HistoryEntry>>,
    /// HP% curve points, oldest first, capped at `CURVE_CAP`. A new point is
    /// appended only when HP actually changes (not on a timer).
    pub curve: Mutex<VecDeque<u8>>,
}

impl SharedState {
    pub fn new(threshold: u8, log_path: String) -> Self {
        SharedState {
            pct: Mutex::new(None),
            threshold: AtomicU8::new(threshold),
            log_path: Mutex::new(log_path),
            status: Mutex::new("starting".to_string()),
            history: Mutex::new(VecDeque::with_capacity(HISTORY_CAP)),
            curve: Mutex::new(VecDeque::with_capacity(CURVE_CAP)),
        }
    }

    pub fn set_threshold(&self, v: u8) {
        self.threshold.store(v, Ordering::Relaxed);
    }

    pub fn threshold(&self) -> u8 {
        self.threshold.load(Ordering::Relaxed)
    }

    pub fn set_log_path(&self, path: String) {
        *self.log_path.lock().unwrap() = path;
    }

    pub fn log_path(&self) -> String {
        self.log_path.lock().unwrap().clone()
    }

    pub fn set_status(&self, s: impl Into<String>) {
        *self.status.lock().unwrap() = s.into();
    }

    /// Pushes a new history row (newest first), dropping the oldest once
    /// over `HISTORY_CAP`.
    pub fn push_history(&self, entry: HistoryEntry) {
        let mut h = self.history.lock().unwrap();
        h.push_front(entry);
        while h.len() > HISTORY_CAP {
            h.pop_back();
        }
    }

    /// Appends a new HP% curve point, dropping the oldest once over
    /// `CURVE_CAP`.
    pub fn push_curve_point(&self, pct: u8) {
        let mut c = self.curve.lock().unwrap();
        c.push_back(pct);
        while c.len() > CURVE_CAP {
            c.pop_front();
        }
    }
}

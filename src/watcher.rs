use crate::audio::play_boing;
use crate::parser::LogParser;
use crate::state::{HistoryEntry, SharedState};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(50);
// How often we re-scan the configured directory for a newer combat log.
// This is deliberately much coarser than POLL_INTERVAL: a directory scan
// stats every file in it, and SWTOR never cleans up old combat logs, so a
// long-lived CombatLogs folder can hold thousands of them. Doing that scan
// 20x/second would itself be enough I/O to delay reading the live file's
// new bytes - the one thing that actually needs to be fast.
const DIR_RESCAN_INTERVAL: Duration = Duration::from_secs(2);

/// Runs forever on a background thread: resolves the configured log path to
/// an actual file, tails it for new lines, parses your HP out of them, and
/// fires the one-shot beep each time HP crosses from above the threshold to
/// at-or-below it. Also records every HP change into the shared history/
/// curve for the dashboard.
pub fn run(shared: Arc<SharedState>) {
    let mut current_file: Option<PathBuf> = None;
    let mut pos: u64 = 0;
    let mut leftover = String::new();
    let mut parser = LogParser::new();
    let mut prev_above: Option<bool> = None;
    let mut last_cur: Option<u64> = None;
    let mut last_dir_scan = Instant::now() - DIR_RESCAN_INTERVAL; // force a scan on the first tick
    let mut last_configured = String::new();

    loop {
        std::thread::sleep(POLL_INTERVAL);

        let configured = shared.log_path();
        if configured.trim().is_empty() {
            shared.set_status("no log path configured");
            continue;
        }

        // Resolving a directory path means scanning every file in it; only
        // do that occasionally. In between, just keep tailing whatever file
        // we last resolved to (if any) - that's the common case by far. A
        // changed `log_path` (e.g. edited in the dashboard) always forces an
        // immediate rescan rather than waiting out the throttle.
        let path_changed = configured != last_configured;
        let need_dir_scan =
            current_file.is_none() || path_changed || last_dir_scan.elapsed() >= DIR_RESCAN_INTERVAL;
        last_configured = configured.clone();

        let target = if need_dir_scan {
            last_dir_scan = Instant::now();
            match resolve_target(&configured) {
                Some(p) => p,
                None => {
                    shared.set_status(format!("no combat log found under {configured}"));
                    continue;
                }
            }
        } else {
            match &current_file {
                Some(p) => p.clone(),
                None => continue, // unreachable: need_dir_scan is true when current_file is None
            }
        };

        let is_new_file = current_file.as_deref() != Some(target.as_path());
        if is_new_file {
            // Switching files: reset parser state and replay this file from
            // the start (silently, no beeps) so we learn who "you" are and
            // what your current HP is before going live.
            parser = LogParser::new();
            prev_above = None;
            last_cur = None;
            pos = 0;
            leftover.clear();
            current_file = Some(target.clone());
            shared.set_status(format!("following {}", target.display()));
            replay_catchup(
                &target,
                &mut parser,
                &mut pos,
                &mut leftover,
                &shared,
                &mut prev_above,
                &mut last_cur,
            );
            continue;
        }

        // Same file: read whatever has been appended since last time.
        let Ok(mut file) = File::open(&target) else {
            current_file = None; // force re-resolve / re-catchup next tick
            continue;
        };
        let Ok(meta) = file.metadata() else { continue };
        let len = meta.len();

        if len < pos {
            // File was truncated or replaced in place (e.g. game restarted
            // logging to the same path) - start over.
            current_file = None;
            continue;
        }
        if len == pos {
            continue; // nothing new
        }

        if file.seek(SeekFrom::Start(pos)).is_err() {
            continue;
        }
        let mut buf = Vec::with_capacity((len - pos) as usize);
        if file.read_to_end(&mut buf).is_err() {
            continue;
        }
        pos = len;

        feed_bytes(
            &buf,
            &mut leftover,
            &mut parser,
            &shared,
            &mut prev_above,
            &mut last_cur,
            true,
        );
    }
}

/// Finds the file to tail: the path itself if it's a file, or the newest
/// `combat_*.txt` inside it if it's a directory.
fn resolve_target(configured: &str) -> Option<PathBuf> {
    let p = Path::new(configured);
    if p.is_file() {
        return Some(p.to_path_buf());
    }
    if p.is_dir() {
        let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
        if let Ok(entries) = std::fs::read_dir(p) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if !name.ends_with(".txt") || !name.contains("combat") {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                            best = Some((modified, path));
                        }
                    }
                }
            }
        }
        return best.map(|(_, p)| p);
    }
    None
}

fn replay_catchup(
    path: &Path,
    parser: &mut LogParser,
    pos: &mut u64,
    leftover: &mut String,
    shared: &Arc<SharedState>,
    prev_above: &mut Option<bool>,
    last_cur: &mut Option<u64>,
) {
    let Ok(mut file) = File::open(path) else { return };
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return;
    }
    *pos = buf.len() as u64;
    // beep disabled during catch-up: we only want to learn current state.
    // History/curve are still recorded, so the dashboard has immediate
    // context on startup instead of coming up empty.
    feed_bytes(&buf, leftover, parser, shared, prev_above, last_cur, false);
}

fn feed_bytes(
    buf: &[u8],
    leftover: &mut String,
    parser: &mut LogParser,
    shared: &Arc<SharedState>,
    prev_above: &mut Option<bool>,
    last_cur: &mut Option<u64>,
    beep_enabled: bool,
) {
    let text = String::from_utf8_lossy(buf);
    leftover.push_str(&text);

    let mut last_newline = None;
    for (i, c) in leftover.char_indices() {
        if c == '\n' {
            last_newline = Some(i);
        }
    }
    let Some(cut) = last_newline else { return };

    let complete = leftover[..cut].to_string();
    let remainder = leftover[cut + 1..].to_string();

    for line in complete.lines() {
        if let Some(sample) = parser.process_line(line) {
            if sample.max == 0 {
                continue;
            }
            let pct = ((sample.cur as f64 / sample.max as f64) * 100.0)
                .round()
                .clamp(0.0, 100.0) as u8;
            *shared.pct.lock().unwrap() = Some(pct);

            // Record a history row + curve point whenever HP actually
            // changed (ground truth: the observed cur HP itself, not the
            // log's raw amount sub-fields, which get complicated by
            // absorption/shields). The very first reading just establishes
            // a baseline - nothing to report a delta against yet.
            if let Some(prev_cur) = *last_cur {
                if sample.cur != prev_cur {
                    let delta = sample.cur as i64 - prev_cur as i64;
                    let label = format_change_label(delta, &sample.change);
                    shared.push_history(HistoryEntry {
                        pct,
                        cur: sample.cur,
                        max: sample.max,
                        delta,
                        label,
                    });
                    shared.push_curve_point(pct);
                }
            } else {
                // First-ever sample: seed the curve so the graph doesn't
                // start empty.
                shared.push_curve_point(pct);
            }
            *last_cur = Some(sample.cur);

            let threshold = shared.threshold();
            let above_now = pct > threshold;
            if beep_enabled {
                if let Some(was_above) = *prev_above {
                    if was_above && !above_now {
                        play_boing();
                    }
                }
            }
            *prev_above = Some(above_now);
        }
    }

    *leftover = remainder;
}

/// Builds the human-readable history row text, e.g.
/// "+400 hp from Sayenne Healing Touch ability" or
/// "-17000 hp from Republic Grunts Critical Smash ability", falling back to
/// a plain "+120 hp" / "-85 hp" when no ability/source could be attributed.
fn format_change_label(delta: i64, change: &Option<crate::parser::HpChange>) -> String {
    let sign = if delta >= 0 { "+" } else { "-" };
    let magnitude = delta.unsigned_abs();
    match change {
        Some(c) => format!(
            "{sign}{magnitude} hp from {} {} ability",
            c.source, c.ability
        ),
        None => format!("{sign}{magnitude} hp"),
    }
}

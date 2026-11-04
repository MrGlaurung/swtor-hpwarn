use crate::audio::{play_boing, play_boing_then_panic, play_panic};
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
// Safety cap on how long we'll keep suppressing "cur HP == 1" readings as a
// continuation of a zone-transition placeholder (see `feed_bytes`) before
// giving up and treating the next reading as real regardless. In practice
// the real placeholder clears in well under a second; this is just a
// backstop so hpwarn can't go silent indefinitely if something unexpected
// happens (e.g. a future game update changes this behaviour).
const ZONE_TRANSITION_GRACE: Duration = Duration::from_secs(5);

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
    let mut prev_above_second: Option<bool> = None;
    let mut last_cur: Option<u64> = None;
    // Some(started_at) while we're still inside a zone-transition's
    // placeholder window (see `feed_bytes`); None once a real post-transition
    // reading has been seen.
    let mut zone_grace: Option<Instant> = None;
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
            prev_above_second = None;
            last_cur = None;
            zone_grace = None;
            pos = 0;
            leftover.clear();
            current_file = Some(target.clone());
            shared.set_status(format!("following {}", target.display()));
            // SWTOR starts a fresh combat_*.txt on relog/crash-recovery/new
            // session; treat that as a new game session and clear last
            // session's HP history/curve rather than carrying it over.
            shared.clear_history();
            shared.set_current_log_file(Some(target.clone()));
            // Recompute how many *other* combat logs are sitting in the
            // directory now, so the dashboard's "Clean log" count is fresh.
            // Deliberately only done here (on new-file detection), not on
            // every tick, per instructions not to poll this while tracking.
            shared.set_old_log_count(count_other_combat_logs(&configured, &target));
            replay_catchup(
                &target,
                &mut parser,
                &mut pos,
                &mut leftover,
                &shared,
                &mut prev_above,
                &mut prev_above_second,
                &mut last_cur,
                &mut zone_grace,
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
            &mut prev_above_second,
            &mut last_cur,
            &mut zone_grace,
            true,
        );
    }
}

/// Whether a filename looks like a SWTOR combat log, e.g.
/// `combat_2026-11-04_12_54_55_790600.txt`. Shared between `resolve_target`
/// (finding the newest one to follow) and the "Clean log" feature (finding
/// every *other* one, to delete).
fn is_combat_log_filename(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".txt") && lower.contains("combat")
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
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !is_combat_log_filename(name) {
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

/// The directory to scan for "other" combat logs: `configured` itself if
/// it's a folder, or its parent if it's a specific file.
fn log_dir_for(configured: &str) -> Option<PathBuf> {
    let p = Path::new(configured);
    if p.is_dir() {
        Some(p.to_path_buf())
    } else {
        p.parent().map(|d| d.to_path_buf())
    }
}

/// Lists every `combat_*.txt` file in the configured log directory except
/// `current`, the file currently being followed.
fn other_combat_logs(configured: &str, current: &Path) -> Vec<PathBuf> {
    let Some(dir) = log_dir_for(configured) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path == current {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if is_combat_log_filename(name) {
                out.push(path);
            }
        }
    }
    out
}

/// How many combat logs (other than `current`) are sitting in the
/// configured log directory - the count shown next to the "Clean log"
/// button. Deliberately cheap to call only occasionally (on new-file
/// detection), not every tick.
fn count_other_combat_logs(configured: &str, current: &Path) -> usize {
    other_combat_logs(configured, current).len()
}

/// Deletes every combat log in the configured log directory except
/// `current`. Returns how many files were actually removed.
pub fn delete_other_combat_logs(configured: &str, current: &Path) -> usize {
    let mut removed = 0;
    for path in other_combat_logs(configured, current) {
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn replay_catchup(
    path: &Path,
    parser: &mut LogParser,
    pos: &mut u64,
    leftover: &mut String,
    shared: &Arc<SharedState>,
    prev_above: &mut Option<bool>,
    prev_above_second: &mut Option<bool>,
    last_cur: &mut Option<u64>,
    zone_grace: &mut Option<Instant>,
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
    feed_bytes(
        &buf,
        leftover,
        parser,
        shared,
        prev_above,
        prev_above_second,
        last_cur,
        zone_grace,
        false,
    );
}

fn feed_bytes(
    buf: &[u8],
    leftover: &mut String,
    parser: &mut LogParser,
    shared: &Arc<SharedState>,
    prev_above: &mut Option<bool>,
    prev_above_second: &mut Option<bool>,
    last_cur: &mut Option<u64>,
    zone_grace: &mut Option<Instant>,
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

            if sample.zone_transition {
                // A teleport/taxi/ship-transfer just happened: the HP on
                // this line is a known-unreliable sync artifact (often a
                // transient "1 HP" placeholder, and max HP can jump from
                // the level-squash rescaling between zones), so it's not a
                // real combat event. Don't beep on it, don't log it to
                // history, and don't let the crossing comparison carry
                // over - reset the baseline so the *next* real reading
                // starts clean instead of being compared against stale
                // pre-transition state. Start the grace window below too:
                // the placeholder doesn't always clear on this exact line -
                // see below.
                *last_cur = None;
                *prev_above = None;
                *prev_above_second = None;
                *zone_grace = Some(Instant::now());
                continue;
            }

            if let Some(started) = *zone_grace {
                // SWTOR keeps echoing the "1 HP" placeholder on several
                // more lines after the AreaEntered line itself (observed:
                // buff/stim/immunity lines logged in the same burst, all
                // still carrying cur=1) before the real post-transition HP
                // syncs in a beat later. Treating that first *real* value as
                // a fresh baseline - rather than a delta from the "1" - is
                // exactly what already happens below as long as we keep
                // skipping every lingering "1" and keep last_cur at None
                // until then.
                if sample.cur == 1 && started.elapsed() < ZONE_TRANSITION_GRACE {
                    continue;
                }
                *zone_grace = None;
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
                    let timestamp = format_timestamp(&sample.time);
                    shared.push_history(HistoryEntry {
                        pct,
                        cur: sample.cur,
                        max: sample.max,
                        delta,
                        label,
                        timestamp,
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

            // Second, optional "panic" beep at a lower HP%. Its threshold is
            // only meaningful while enabled, and the dashboard already keeps
            // it clamped below `threshold`, so no extra validation is
            // needed here.
            let second_enabled = shared.second_beep_enabled();
            let second_threshold = shared.second_threshold();
            let above_now_second = pct > second_threshold;

            if beep_enabled {
                let crosses_first = prev_above.unwrap_or(false) && !above_now;
                let crosses_second = second_enabled
                    && prev_above_second.unwrap_or(false)
                    && !above_now_second;

                match (crosses_first, crosses_second) {
                    (true, true) => play_boing_then_panic(),
                    (true, false) => play_boing(),
                    (false, true) => play_panic(),
                    (false, false) => {}
                }
            }
            *prev_above = Some(above_now);
            *prev_above_second = Some(above_now_second);
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

/// Formats a history row's timestamp as "YYYYMMDD HH:MM:SS.cc", e.g.
/// "20261020 17:30:42.12".
///
/// The combat log line itself only carries a time-of-day ("20:20:08.374"),
/// never a date, so the date is today's local date. That's correct for the
/// overwhelmingly common case (watching a live log as it's written); it
/// would only be wrong if hpwarn were pointed at an old log file that spans
/// a date boundary relative to when it's being read.
fn format_timestamp(log_time: &str) -> String {
    let date = chrono::Local::now().format("%Y%m%d");

    // "HH:MM:SS.mmm" -> "HH:MM:SS" + centiseconds (first two digits of the
    // millisecond field, i.e. milliseconds / 10).
    let (hms, millis_str) = log_time.split_once('.').unwrap_or((log_time, "0"));
    let millis: u32 = millis_str.parse().unwrap_or(0);
    let centis = millis / 10;

    format!("{date} {hms}.{centis:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_timestamp_with_todays_date_and_centiseconds() {
        let today = chrono::Local::now().format("%Y%m%d").to_string();
        assert_eq!(format_timestamp("17:30:42.123"), format!("{today} 17:30:42.12"));
        // Single/zero-digit millisecond fields still produce 2-digit centis.
        assert_eq!(format_timestamp("00:00:00.005"), format!("{today} 00:00:00.00"));
        assert_eq!(format_timestamp("09:08:07.090"), format!("{today} 09:08:07.09"));
    }

    /// Regression test: a teleport/ship-transfer (an `AreaEntered` line)
    /// must not produce a false "massive HP change" history entry, even
    /// though the real post-transition HP can be wildly different from
    /// the last reading before the transition (level-squash rescaling).
    #[test]
    fn zone_transition_does_not_pollute_history() {
        let shared = Arc::new(SharedState::new(50, String::new()));
        let mut parser = LogParser::new();
        let mut prev_above = None;
        let mut prev_above_second = None;
        let mut last_cur = None;
        let mut zone_grace = None;
        let mut leftover = String::new();

        // 1) Establish self (an AreaEntered line - itself a zone_transition
        // sample, so it should be skipped for display/history purposes too).
        let establish_self = "[20:20:08.374] [@Rach'ka#687136982611430|(31.73,13.92,9.02,-147.61)|(1/379515)] [] [] [AreaEntered {836045448953664}: Fury {137438956742}] (he4000) <v7.0.0b>\n";

        // 2) First real reading: baseline only, no history entry yet.
        let first_real = "[20:55:56.628] [@Rach'ka#687136982611430|(-640.01,360.65,27.97,160.91)|(441133/441133)] [Annihilation Droid XRR-3 {1779997656219648}:15855000027709|(-640.00,358.00,27.97,0.00)|(11857644/11869049)] [Maul {808269895434240}] [ApplyEffect {836045448945477}: Damage {836045448945501}] (10917 energy {836045448940874}) <32751.0>\n";

        // 3) A real incoming hit: this SHOULD produce exactly one history
        // entry (legitimate combat damage, not a zone transition).
        let real_hit = "[20:56:06.037] [Annihilation Droid XRR-3 {1779997656219648}:15855000027709|(-640.00,358.00,27.97,0.00)|(10672188/11869049)] [@Rach'ka#687136982611430|(-641.97,364.21,27.97,-17.61)|(433507/441133)] [Burning {2017371908735236}] [ApplyEffect {836045448945477}: Damage {836045448945501}] (6503 kinetic {836045448940873}) <6503.0>\n";

        // 4) Ship transfer: AreaEntered with the classic "1 HP" placeholder
        // and a completely different (level-squashed) max HP.
        let area_entered = "[22:12:47.926] [@Rach'ka#687136982611430|(-0.02,43.05,8.00,-45.80)|(1/410680)] [] [] [AreaEntered {836045448953664}: Fury {137438956742}] (he4000) <v7.0.0b>\n";

        // 4b) The real log keeps echoing the "1 HP" placeholder across
        // several more buff/stim/immunity lines before the real value
        // syncs in - these must ALSO be ignored (not treated as a fresh
        // baseline on their own), otherwise the baseline gets seeded at
        // "1" and the real sync line two steps below produces a bogus
        // "+410679 hp" entry, which was the actual bug being fixed here.
        let lingering_placeholder = "[22:12:47.968] [@Rach'ka#687136982611430|(-0.02,43.05,8.00,-45.80)|(1/410680)] [=] [ {2835571768557568}] [ApplyEffect {836045448945477}: Mental Exhaustion {2835571768557864}]\n";

        // 5) The real post-transition HP syncs in, wildly different from
        // reading 3 (433507/441133) - this must NOT be logged as a
        // 433507->410680 (or 1->410680) "hp change" history entry.
        let synced_hp = "[22:12:48.063] [@Rach'ka#687136982611430|(-0.02,43.05,8.00,-45.80)|(410680/441133)] [=] [Mark of Power {4503122886000640}] [ApplyEffect {836045448945477}: Unnatural Might {4503122886000920}]\n";

        let all = format!(
            "{establish_self}{first_real}{real_hit}{area_entered}{lingering_placeholder}{synced_hp}"
        );
        feed_bytes(
            all.as_bytes(),
            &mut leftover,
            &mut parser,
            &shared,
            &mut prev_above,
            &mut prev_above_second,
            &mut last_cur,
            &mut zone_grace,
            true,
        );

        let history = shared.history.lock().unwrap();
        assert_eq!(
            history.len(),
            1,
            "expected exactly one history entry (the real hit), got: {:?}",
            *history
        );
        assert_eq!(history[0].cur, 433507);
        assert_eq!(history[0].delta, 433507 - 441133);

        // The zone transition reset the baseline, so after it, last_cur is
        // the synced HP, ready to track real deltas again from here on.
        assert_eq!(last_cur, Some(410680));
    }

    #[test]
    fn recognizes_combat_log_filenames() {
        assert!(is_combat_log_filename("combat_2026-11-04_12_54_55_790600.txt"));
        assert!(is_combat_log_filename("COMBAT_2026-11-04_12_54_55_790600.TXT"));
        assert!(!is_combat_log_filename("notes.txt"));
        assert!(!is_combat_log_filename("combat_2026-11-04.log"));
    }

    /// "Clean log" must count/delete every other combat log in the
    /// directory, but never the one currently being followed.
    #[test]
    fn other_combat_logs_excludes_current_file_and_non_matching_names() {
        let dir = std::env::temp_dir().join(format!(
            "hpwarn_test_other_logs_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let current = dir.join("combat_2026-11-04_12_54_55_790600.txt");
        let old1 = dir.join("combat_2026-11-03_08_00_00_000000.txt");
        let old2 = dir.join("combat_2026-11-02_08_00_00_000000.txt");
        let unrelated = dir.join("readme.txt");
        for f in [&current, &old1, &old2, &unrelated] {
            std::fs::write(f, b"x").unwrap();
        }

        let configured = dir.to_string_lossy().to_string();
        assert_eq!(count_other_combat_logs(&configured, &current), 2);

        let removed = delete_other_combat_logs(&configured, &current);
        assert_eq!(removed, 2);
        assert!(current.exists(), "current log must never be deleted");
        assert!(unrelated.exists(), "non-combat-log files must never be deleted");
        assert!(!old1.exists());
        assert!(!old2.exists());
        assert_eq!(count_other_combat_logs(&configured, &current), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sequencing: when a single sample crosses both the first and second
    /// thresholds downward at once, both beeps must fire (verified here via
    /// the crossing booleans rather than actual audio playback, which isn't
    /// observable in a unit test).
    #[test]
    fn single_drop_crossing_both_thresholds_triggers_both_crossings() {
        let shared = Arc::new(SharedState::new(50, String::new()));
        shared.set_second_beep_enabled(true);
        shared.set_second_threshold(30);

        let mut parser = LogParser::new();
        let mut prev_above = None;
        let mut prev_above_second = None;
        let mut last_cur = None;
        let mut zone_grace = None;
        let mut leftover = String::new();

        let establish_self = "[20:20:08.374] [@Rach'ka#687136982611430|(31.73,13.92,9.02,-147.61)|(1/100)] [] [] [AreaEntered {836045448953664}: Fury {137438956742}] (he4000) <v7.0.0b>\n";
        let first_real = "[20:55:56.628] [@Rach'ka#687136982611430|(-640.01,360.65,27.97,160.91)|(60/100)] [Annihilation Droid XRR-3 {1779997656219648}:15855000027709|(-640.00,358.00,27.97,0.00)|(11857644/11869049)] [Maul {808269895434240}] [ApplyEffect {836045448945477}: Damage {836045448945501}] (10917 energy {836045448940874}) <32751.0>\n";
        let big_hit = "[20:56:06.037] [Annihilation Droid XRR-3 {1779997656219648}:15855000027709|(-640.00,358.00,27.97,0.00)|(10672188/11869049)] [@Rach'ka#687136982611430|(-641.97,364.21,27.97,-17.61)|(20/100)] [Burning {2017371908735236}] [ApplyEffect {836045448945477}: Damage {836045448945501}] (40 kinetic {836045448940873}) <6503.0>\n";

        let all = format!("{establish_self}{first_real}{big_hit}");
        feed_bytes(
            all.as_bytes(),
            &mut leftover,
            &mut parser,
            &shared,
            &mut prev_above,
            &mut prev_above_second,
            &mut last_cur,
            &mut zone_grace,
            true,
        );

        // 60% was above both 50% and 30%; the big hit to 20% drops below
        // both in one sample, so both should now read "not above".
        assert_eq!(prev_above, Some(false));
        assert_eq!(prev_above_second, Some(false));
    }
}

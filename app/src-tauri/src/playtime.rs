// How long each game has been played, and when it was last opened.
//
// The UI has offered "Recently played" and "Playtime" as sort options
// since the first version, and for real games both did nothing at all:
// the mock catalog carried `hours` and `lastPlayedDaysAgo` fields the
// server has no idea about, so sorting a real library by either was a
// no-op dressed up as a feature. Everything needed to fill them in was
// already there — the launcher knows when a game starts, and the exit
// supervision it added for cloud saves knows when it stops.
//
// Stored as a flat JSON map beside installs.json, written once per
// session rather than on a timer: a session that ends because the
// machine lost power is a session this does not record, which is a
// better trade than rewriting the file every minute for the lifetime
// of every game anyone plays.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub struct Playtime {
    /// Total seconds across every recorded session.
    pub seconds: u64,
    /// When the game was last launched, seconds since the epoch.
    pub last_played: u64,
    pub sessions: u32,
}

pub type PlaytimeMap = HashMap<String, Playtime>;

fn state_file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("playtime.json"))
}

fn load(app: &AppHandle) -> PlaytimeMap {
    match state_file(app) {
        Ok(path) => std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        Err(_) => PlaytimeMap::new(),
    }
}

fn save(app: &AppHandle, map: &PlaytimeMap) -> Result<(), String> {
    let path = state_file(app)?;
    let json = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[tauri::command]
pub fn get_playtime(app: AppHandle) -> PlaytimeMap {
    load(&app)
}

/// Records that a game has just been launched. The session's length is
/// not known yet, so only the timestamp moves — which is enough for
/// "Recently played" to be right the moment a game opens, rather than
/// only after it closes.
pub fn started(app: &AppHandle, game_id: &str) {
    let mut map = load(app);
    let entry = map.entry(game_id.to_string()).or_default();
    entry.last_played = now();
    let updated = entry.clone();
    let _ = save(app, &map);
    let _ = app.emit("playtime:changed", (game_id, updated));
}

/// Records a finished session of `seconds`, and pushes the new total to
/// the frontend so a sort by playtime is correct without a restart.
pub fn finished(app: &AppHandle, game_id: &str, seconds: u64) {
    let mut map = load(app);
    let entry = map.entry(game_id.to_string()).or_default();
    *entry = accumulate(entry, seconds, now());
    let updated = entry.clone();
    let _ = save(app, &map);
    let _ = app.emit("playtime:changed", (game_id, updated));
}

/// The arithmetic, separated from the filesystem so it can be tested
/// without an app handle.
///
/// A session shorter than a minute is counted toward the session tally
/// but not the clock: a game that failed to start, or was closed
/// immediately because it opened on the wrong monitor, is not playtime,
/// and a library where every mis-click adds a minute stops being a
/// useful sort within a week.
fn accumulate(current: &Playtime, seconds: u64, ended_at: u64) -> Playtime {
    const MINIMUM_SESSION: u64 = 60;
    // A session longer than this is not a session, it is a game left
    // running overnight. Counted at the cap rather than discarded,
    // since something was almost certainly played.
    const MAXIMUM_SESSION: u64 = 12 * 60 * 60;

    let counted = if seconds < MINIMUM_SESSION {
        0
    } else {
        seconds.min(MAXIMUM_SESSION)
    };

    Playtime {
        seconds: current.seconds.saturating_add(counted),
        last_played: ended_at.max(current.last_played),
        sessions: current.sessions.saturating_add(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64, last_played: u64, sessions: u32) -> Playtime {
        Playtime {
            seconds,
            last_played,
            sessions,
        }
    }

    #[test]
    fn a_session_adds_to_the_total() {
        let after = accumulate(&at(3600, 1000, 1), 1800, 5000);
        assert_eq!(after, at(5400, 5000, 2));
    }

    #[test]
    fn a_first_session_starts_from_nothing() {
        assert_eq!(
            accumulate(&Playtime::default(), 600, 5000),
            at(600, 5000, 1)
        );
    }

    #[test]
    fn a_game_closed_immediately_is_not_playtime() {
        // Wrong monitor, missing dependency, changed their mind — all
        // of these open and close a game without playing it, and a
        // sort that counts them stops being useful within a week.
        let after = accumulate(&at(3600, 1000, 1), 12, 5000);
        assert_eq!(after.seconds, 3600, "no time should have been added");
        assert_eq!(after.sessions, 2, "but it was still a launch");
        assert_eq!(after.last_played, 5000, "and it was still recent");
    }

    #[test]
    fn a_game_left_running_overnight_is_capped() {
        let after = accumulate(&Playtime::default(), 30 * 60 * 60, 5000);
        assert_eq!(after.seconds, 12 * 60 * 60);
    }

    #[test]
    fn a_clock_that_went_backwards_does_not_rewrite_history() {
        // Whether by NTP or by someone changing the timezone: the most
        // recent play is still the most recent play.
        let after = accumulate(&at(3600, 9000, 1), 600, 5000);
        assert_eq!(after.last_played, 9000);
    }

    #[test]
    fn totals_saturate_rather_than_wrapping() {
        let after = accumulate(&at(u64::MAX, 0, u32::MAX), 600, 5000);
        assert_eq!(after.seconds, u64::MAX);
        assert_eq!(after.sessions, u32::MAX);
    }
}

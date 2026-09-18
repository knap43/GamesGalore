// How long each game has been played.
//
// The UI has offered "Playtime" as a sort option since the first
// version, and for real games it did nothing at all: the mock catalog
// carried an `hours` field the server has no idea about, so sorting a
// real library by it was a no-op dressed up as a feature. Everything
// needed to fill it in was already there — the launcher knows when a
// game starts, and the exit supervision it added for cloud saves knows
// when it stops.
//
// When a game was last played is deliberately not recorded. It bought
// one sort order and a line in the detail header, and the line read
// "Last played Playing now" for as long as a game was open.
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

/// Records a finished session of `seconds`, and pushes the new total to
/// the frontend so a sort by playtime is correct without a restart.
pub fn finished(app: &AppHandle, game_id: &str, seconds: u64) {
    let mut map = load(app);
    let entry = map.entry(game_id.to_string()).or_default();
    *entry = accumulate(entry, seconds);
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
fn accumulate(current: &Playtime, seconds: u64) -> Playtime {
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
        sessions: current.sessions.saturating_add(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64, sessions: u32) -> Playtime {
        Playtime { seconds, sessions }
    }

    #[test]
    fn a_session_adds_to_the_total() {
        assert_eq!(accumulate(&at(3600, 1), 1800), at(5400, 2));
    }

    #[test]
    fn a_first_session_starts_from_nothing() {
        assert_eq!(accumulate(&Playtime::default(), 600), at(600, 1));
    }

    #[test]
    fn a_game_closed_immediately_is_not_playtime() {
        // Wrong monitor, missing dependency, changed their mind — all
        // of these open and close a game without playing it, and a
        // sort that counts them stops being useful within a week.
        let after = accumulate(&at(3600, 1), 12);
        assert_eq!(after.seconds, 3600, "no time should have been added");
        assert_eq!(after.sessions, 2, "but it was still a launch");
    }

    #[test]
    fn a_game_left_running_overnight_is_capped() {
        assert_eq!(
            accumulate(&Playtime::default(), 30 * 60 * 60).seconds,
            12 * 60 * 60
        );
    }

    #[test]
    fn totals_saturate_rather_than_wrapping() {
        let after = accumulate(&at(u64::MAX, u32::MAX), 600);
        assert_eq!(after.seconds, u64::MAX);
        assert_eq!(after.sessions, u32::MAX);
    }
}

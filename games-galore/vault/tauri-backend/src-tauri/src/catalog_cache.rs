// A local copy of the catalog entries for the titles that are actually
// installed, so the app has something real to draw the moment it opens.
//
// The reason this exists: /library rescans the whole source tree on
// every call, which on a large library takes long enough that the grid
// sat on "Loading your library…" for seconds after launch — including
// for the handful of games you have on disk and could already play.
// Those titles are the ones the app opens on (the Installed filter is
// on by default), and their catalog entries change rarely, so there is
// no reason to wait on the server to show them.
//
// Deliberately scoped to installed titles rather than the whole
// catalog. A stale entry for something you have on disk is harmless —
// the files are right there, and the entry only supplies its name,
// cover and blurb — whereas a stale copy of the other several hundred
// would be offering you downloads of titles the server may no longer
// have. It also keeps the file small enough to read and parse without
// anyone noticing.
//
// Written alongside installs.json in the app data directory, and kept
// in step with it: every transition that adds or removes an install
// updates this too, and a successful /library fetch refreshes whatever
// entries it still covers.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

use crate::install_state::installed_ids;
use crate::server::Game;

fn cache_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("installed-cache.json"))
}

/// Missing, unreadable or malformed all mean the same thing here: no
/// cache. This is a shortcut, never a source of truth, so a bad file
/// costs a slower first paint and nothing else.
fn read_cache(path: &Path) -> Vec<Game> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_cache(path: &Path, games: &[Game]) -> Result<(), String> {
    let json = serde_json::to_string_pretty(games).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// Live catalog data wins wherever the server still lists a title;
/// cached entries survive only for installed titles the live catalog
/// no longer covers, which is the case worth protecting — a game
/// pulled from the shared library (or a server that answered with a
/// partial scan) shouldn't quietly disappear from the shelf of things
/// you have on disk and can still launch.
///
/// Sorted by id so the file doesn't churn between writes purely
/// because a HashMap iterated in a different order.
fn merge(cached: Vec<Game>, live: &[Game], installed: &HashSet<String>) -> Vec<Game> {
    let mut out: Vec<Game> =
        live.iter().filter(|g| installed.contains(&g.id)).cloned().collect();
    let covered: HashSet<String> = out.iter().map(|g| g.id.clone()).collect();
    out.extend(
        cached
            .into_iter()
            .filter(|g| installed.contains(&g.id) && !covered.contains(&g.id)),
    );
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn upsert(mut cached: Vec<Game>, game: Game) -> Vec<Game> {
    cached.retain(|g| g.id != game.id);
    cached.push(game);
    cached.sort_by(|a, b| a.id.cmp(&b.id));
    cached
}

/// Every mutation below is best-effort: this file is an optimisation,
/// and failing to write it is never a reason to fail an install, an
/// uninstall or a library fetch.
fn update(app: &AppHandle, f: impl FnOnce(Vec<Game>) -> Vec<Game>) {
    let Ok(path) = cache_path(app) else { return };
    let next = f(read_cache(&path));
    let _ = write_cache(&path, &next);
}

/// Refreshes the cache against a catalog just fetched from the server.
pub fn sync(app: &AppHandle, live: &[Game]) {
    let installed = installed_ids(app);
    update(app, |cached| merge(cached, live, &installed));
}

/// Records a title that just finished installing, so it is on the
/// shelf at next launch even if the app is never online again.
pub fn remember(app: &AppHandle, game: &Game) {
    let game = game.clone();
    update(app, |cached| upsert(cached, game));
}

pub fn forget(app: &AppHandle, id: &str) {
    let id = id.to_string();
    update(app, |mut cached| {
        cached.retain(|g| g.id != id);
        cached
    });
}

/// The frontend calls this before it calls fetch_library, draws
/// whatever comes back, and then reconciles once the live catalog
/// arrives. Synchronous and local, so it returns in the time it takes
/// to read a few kilobytes off disk.
#[tauri::command]
pub fn get_cached_library(app: AppHandle) -> Vec<Game> {
    match cache_path(&app) {
        Ok(path) => read_cache(&path),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::GameFile;

    fn game(id: &str, title: &str) -> Game {
        Game {
            id: id.to_string(),
            title: title.to_string(),
            platform: id.split('/').next().unwrap_or("PC").to_string(),
            release_year: None,
            description: String::new(),
            files: vec![GameFile {
                filename: "game.exe".to_string(),
                format: "exe".to_string(),
                needs_conversion: false,
                size_bytes: 10,
            }],
            screenshots: vec![],
            cover: None,
            trailer: None,
        }
    }

    fn ids(list: &[Game]) -> Vec<&str> {
        list.iter().map(|g| g.id.as_str()).collect()
    }

    fn installed(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn merge_keeps_only_installed_titles() {
        let live = vec![game("PC/One", "One"), game("PC/Two", "Two")];
        let out = merge(vec![], &live, &installed(&["PC/One"]));
        assert_eq!(ids(&out), vec!["PC/One"]);
    }

    #[test]
    fn merge_takes_the_live_entry_over_the_cached_one() {
        let cached = vec![game("PC/One", "Old Name")];
        let live = vec![game("PC/One", "New Name")];
        let out = merge(cached, &live, &installed(&["PC/One"]));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "New Name");
    }

    #[test]
    fn merge_keeps_an_installed_title_the_server_no_longer_lists() {
        // The files are on this machine either way, so dropping it
        // would take a playable game off the shelf over a change on
        // the far side of the network.
        let cached = vec![game("PC/Gone", "Gone")];
        let out = merge(cached, &[], &installed(&["PC/Gone"]));
        assert_eq!(ids(&out), vec!["PC/Gone"]);
    }

    #[test]
    fn merge_drops_a_cached_title_that_is_no_longer_installed() {
        let cached = vec![game("PC/Gone", "Gone")];
        let out = merge(cached, &[], &installed(&[]));
        assert!(out.is_empty());
    }

    #[test]
    fn merge_sorts_by_id_so_the_file_does_not_churn() {
        let live = vec![game("Switch/B", "B"), game("PC/A", "A")];
        let out = merge(vec![], &live, &installed(&["Switch/B", "PC/A"]));
        assert_eq!(ids(&out), vec!["PC/A", "Switch/B"]);
    }

    #[test]
    fn upsert_replaces_rather_than_duplicating() {
        let cached = vec![game("PC/One", "Old")];
        let out = upsert(cached, game("PC/One", "New"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "New");
    }

    #[test]
    fn a_missing_or_corrupt_cache_reads_as_empty_not_an_error() {
        let dir = std::env::temp_dir().join("gg-cache-test-read");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(read_cache(&dir.join("nothing.json")).is_empty());

        let bad = dir.join("bad.json");
        std::fs::write(&bad, "{not json").unwrap();
        assert!(read_cache(&bad).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_written_cache_reads_back_as_the_same_titles() {
        let dir = std::env::temp_dir().join("gg-cache-test-roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("installed-cache.json");

        let games = vec![game("PC/One", "One"), game("Switch/Two", "Two")];
        write_cache(&path, &games).unwrap();

        let back = read_cache(&path);
        assert_eq!(ids(&back), vec!["PC/One", "Switch/Two"]);
        assert_eq!(back[0].files[0].filename, "game.exe");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

// Bringing a game's saves back inside its Wine prefix, by watching it
// play rather than by asking anyone anything.
//
// Wine points a prefix's Documents, Saved Games and the rest at the
// real home directory. For a prefix this app creates, the launcher
// replaces those links before anything can use them. For a prefix that
// already existed, it cannot: a game may have been saving through the
// link for weeks, and cutting it would leave that save outside the
// prefix while the game starts fresh — indistinguishable, from where
// the player is sitting, from losing it.
//
// What the app can do is notice which directory the game actually
// writes to. The same trick already identifies a Switch title from a
// play session: take a picture of the directory before launch, take
// another after the game exits, and the difference is the answer.
// Here the difference is "the folder in ~/Documents this game touched
// while it was running", which is exactly the folder to move in.
//
// Deliberately conservative about what counts:
//
//   - Directories only. A game keeps its save in a folder of its own;
//     a document edited during a play session is usually a file, and
//     moving one into a Wine prefix would be a genuinely bad surprise.
//   - Only what changed inside the session's own window, so a folder
//     that merely sits there is never touched.
//   - A symlink is left where each folder was, pointing at its new
//     home, so anything else on the machine that referred to it still
//     resolves.
//   - If the scan is too large to be quick, nothing happens at all.
//     Guessing badly here costs somebody their saves; doing nothing
//     costs them the notice they were already seeing.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// How deep to look inside a candidate folder for the newest change.
/// A save lives a level or two down (`ULTRAKILL/Saves/slot1.bepis`);
/// past that is somebody's file tree, not a save.
const SCAN_DEPTH: usize = 3;

/// Total directory entries a single snapshot may stat. A home
/// directory can be enormous, and this runs on the launch path — past
/// this, the snapshot is abandoned rather than made slowly.
const SCAN_BUDGET: usize = 20_000;

/// One linked-out profile folder, and what was in it before the game ran.
#[derive(Debug, Clone)]
pub struct LinkSnapshot {
    /// "Documents", "Saved Games", …
    pub folder: String,
    /// The link inside the prefix.
    pub link: PathBuf,
    /// Where it points, out in the home directory.
    pub target: PathBuf,
    /// Top-level directory name -> newest modification time beneath it,
    /// in milliseconds since the epoch.
    pub entries: HashMap<String, u64>,
}

/// Photographs every profile folder that links out of the prefix.
///
/// Returns nothing at all when there is nothing to watch, and also when
/// the directories involved are too large to scan quickly — see
/// SCAN_BUDGET.
pub fn snapshot(prefix: &Path) -> Vec<LinkSnapshot> {
    let mut shots = Vec::new();
    for (folder, link, target) in linked_out_folders(prefix) {
        let mut budget = SCAN_BUDGET;
        match scan_entries(&target, &mut budget) {
            Some(entries) => shots.push(LinkSnapshot {
                folder,
                link,
                target,
                entries,
            }),
            None => return Vec::new(), // too big to watch; watch none of it
        }
    }
    shots
}

/// Profile folders inside the prefix that are symlinks pointing out of
/// it, as (name, link path, resolved target).
fn linked_out_folders(prefix: &Path) -> Vec<(String, PathBuf, PathBuf)> {
    let users = prefix.join("drive_c").join("users");
    let Ok(profiles) = fs::read_dir(&users) else {
        return Vec::new();
    };
    let inside = fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());

    let mut out = Vec::new();
    for profile in profiles.flatten() {
        for name in crate::saves::PROFILE_SAVE_FOLDERS {
            let link = profile.path().join(name);
            let Ok(meta) = fs::symlink_metadata(&link) else {
                continue;
            };
            if !meta.file_type().is_symlink() {
                continue;
            }
            let Ok(target) = fs::canonicalize(&link) else {
                continue; // dangling; the launcher reclaims those itself
            };
            if target.starts_with(&inside) {
                continue; // points back inside, so it is archived anyway
            }
            out.push((name.to_string(), link, target));
        }
    }
    out
}

/// Every top-level *directory* in `dir`, with the newest modification
/// time anywhere beneath it. None if the budget runs out.
fn scan_entries(dir: &Path, budget: &mut usize) -> Option<HashMap<String, u64>> {
    let mut entries = HashMap::new();
    for entry in fs::read_dir(dir).ok()?.flatten() {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue; // files are not what a game keeps its save in
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let newest = newest_mtime(&entry.path(), SCAN_DEPTH, budget)?;
        entries.insert(name, newest);
    }
    Some(entries)
}

/// The newest mtime at or under `path`, to `depth` levels. None if the
/// budget runs out; symlinks are never followed.
fn newest_mtime(path: &Path, depth: usize, budget: &mut usize) -> Option<u64> {
    let meta = fs::symlink_metadata(path).ok()?;
    let mut newest = mtime_of(&meta);

    if depth > 0 && meta.is_dir() && !meta.file_type().is_symlink() {
        for entry in fs::read_dir(path).ok()?.flatten() {
            if *budget == 0 {
                return None;
            }
            *budget -= 1;
            newest = newest.max(newest_mtime(&entry.path(), depth - 1, budget)?);
        }
    }
    Some(newest)
}

/// Milliseconds, not seconds. A save written in the same second as the
/// snapshot was taken would otherwise look unchanged — unlikely across
/// a real play session, certain across a test, and wrong either way.
fn mtime_of(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The same clock the snapshots are measured against, for a caller
/// marking when a session began.
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Compares the two pictures and names the directories this session
/// actually wrote to.
///
/// A directory qualifies by being new, or by having changed inside the
/// session's window. The window matters: without it, a folder whose
/// mtime happens to be newer for some unrelated reason would be
/// swept in.
fn touched_during(
    before: &HashMap<String, u64>,
    after: &HashMap<String, u64>,
    session_start: u64,
) -> Vec<String> {
    let mut names: Vec<String> = after
        .iter()
        .filter(|(name, newest)| {
            if **newest < session_start {
                return false;
            }
            match before.get(*name) {
                Some(previous) => *newest > previous,
                None => true, // created while the game ran
            }
        })
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    names
}

/// What a completed migration did, for the UI to report.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Migrated {
    /// "Documents/ULTRAKILL", … as moved.
    pub moved: Vec<String>,
}

/// Run after a session ends: works out what the game wrote and moves
/// those folders into the prefix, replacing the link as it goes.
///
/// Best-effort throughout. A failure here leaves the prefix exactly as
/// it was; nothing is half-moved, because the folders are gathered in a
/// staging directory inside the prefix and only swapped into place once
/// they are all there.
pub fn migrate_after_session(before: &[LinkSnapshot], session_start: u64) -> Migrated {
    let mut result = Migrated::default();

    for shot in before {
        let mut budget = SCAN_BUDGET;
        let Some(after) = scan_entries(&shot.target, &mut budget) else {
            continue;
        };
        let touched = touched_during(&shot.entries, &after, session_start);
        if touched.is_empty() {
            continue;
        }

        match adopt(&shot.link, &shot.target, &touched) {
            Ok(()) => result
                .moved
                .extend(touched.iter().map(|name| format!("{}/{name}", shot.folder))),
            Err(e) => crate::log_line!("could not bring {} into the prefix: {e}", shot.folder),
        }
    }
    result
}

/// Replaces `link` with a real directory holding `names`, moved out of
/// `target`, and leaves a symlink behind for each one.
///
/// The staging directory is the reason this is safe to interrupt: the
/// link is only removed once every folder has already been moved, so a
/// failure partway leaves the original arrangement intact apart from a
/// stray directory that the next attempt overwrites.
fn adopt(link: &Path, target: &Path, names: &[String]) -> Result<(), String> {
    let mut staging = link.to_path_buf();
    staging.as_mut_os_string().push(".incoming");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|e| e.to_string())?;

    for name in names {
        move_dir(&target.join(name), &staging.join(name)).inspect_err(|_| {
            let _ = fs::remove_dir_all(&staging);
        })?;
    }

    // Swap: the link goes, the staging directory takes its name.
    fs::remove_file(link).map_err(|e| e.to_string())?;
    fs::rename(&staging, link).map_err(|e| e.to_string())?;

    // And a pointer at each old location, so anything else on this
    // machine that referred to the folder still finds it.
    for name in names {
        let _ = std::os::unix::fs::symlink(link.join(name), target.join(name));
    }
    Ok(())
}

/// A rename where possible, a copy where not — a prefix and a home
/// directory are often on different filesystems, and rename refuses to
/// cross one.
fn move_dir(from: &Path, to: &Path) -> Result<(), String> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_dir(from, to)?;
    fs::remove_dir_all(from).map_err(|e| format!("removing {}: {e}", from.display()))
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(from).map_err(|e| e.to_string())?.flatten() {
        let source = entry.path();
        let dest = to.join(entry.file_name());
        let meta = fs::symlink_metadata(&source).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            continue; // not this game's data, wherever it points
        }
        if meta.is_dir() {
            copy_dir(&source, &dest)?;
        } else {
            fs::copy(&source, &dest).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gg-migrate-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// A prefix whose Documents links out to a home directory holding
    /// one game's saves and one of the user's own folders.
    fn fixture(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = scratch(name);
        let prefix = base.join("prefix");
        let users = prefix.join("drive_c/users/you");
        let home_docs = base.join("home/Documents");

        write(&home_docs.join("ULTRAKILL/Saves/slot1.bepis"), "act III");
        write(&home_docs.join("Thesis/chapter1.md"), "mine");
        fs::create_dir_all(&users).unwrap();
        std::os::unix::fs::symlink(&home_docs, users.join("Documents")).unwrap();

        (base, prefix, home_docs)
    }

    #[test]
    fn only_what_changed_during_the_session_counts() {
        let before = HashMap::from([
            ("ULTRAKILL".to_string(), 1_000u64),
            ("Thesis".to_string(), 1_000),
            ("Old".to_string(), 1_000),
        ]);
        let after = HashMap::from([
            ("ULTRAKILL".to_string(), 5_050u64), // written to while playing
            ("Thesis".to_string(), 1_000),       // untouched
            ("Old".to_string(), 4_000),          // changed, but before the session
            ("New".to_string(), 5_060),          // created while playing
        ]);

        assert_eq!(
            touched_during(&before, &after, 5_000),
            vec!["New".to_string(), "ULTRAKILL".to_string()]
        );
    }

    #[test]
    fn a_session_that_wrote_nothing_moves_nothing() {
        let entries = HashMap::from([("ULTRAKILL".to_string(), 1_000u64)]);
        assert!(touched_during(&entries, &entries, 5_000).is_empty());
    }

    #[test]
    fn the_folder_a_game_wrote_to_is_brought_into_the_prefix() {
        let (base, prefix, home_docs) = fixture("adopts");
        let users = prefix.join("drive_c/users/you");

        let shots = snapshot(&prefix);
        assert_eq!(shots.len(), 1, "one linked-out folder to watch");
        assert!(shots[0].entries.contains_key("ULTRAKILL"));

        // The game plays and writes a save; the thesis is not touched.
        let session_start = now_millis();
        // A moment later, as a real session would be.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(&home_docs.join("ULTRAKILL/Saves/slot1.bepis"), "act IV");
        let moved = migrate_after_session(&shots, session_start);

        assert_eq!(moved.moved, vec!["Documents/ULTRAKILL".to_string()]);

        // Documents is now a real directory inside the prefix...
        let docs = users.join("Documents");
        assert!(!fs::symlink_metadata(&docs)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_to_string(docs.join("ULTRAKILL/Saves/slot1.bepis")).unwrap(),
            "act IV"
        );

        // ...the user's own folder stayed where it was...
        assert_eq!(
            fs::read_to_string(home_docs.join("Thesis/chapter1.md")).unwrap(),
            "mine"
        );
        assert!(!home_docs.join("Thesis").is_symlink());

        // ...and a pointer was left where the save used to be.
        let left_behind = home_docs.join("ULTRAKILL");
        assert!(fs::symlink_metadata(&left_behind)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_to_string(left_behind.join("Saves/slot1.bepis")).unwrap(),
            "act IV"
        );

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_session_that_touched_nothing_leaves_the_prefix_alone() {
        let (base, prefix, home_docs) = fixture("untouched");
        let users = prefix.join("drive_c/users/you");

        let shots = snapshot(&prefix);
        // A session window that starts in the future: nothing can
        // possibly have been written inside it.
        let moved = migrate_after_session(&shots, now_millis() + 10_000);

        assert!(moved.moved.is_empty());
        // Still a link, and everything still where it was.
        assert!(fs::symlink_metadata(users.join("Documents"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(home_docs.join("ULTRAKILL/Saves/slot1.bepis").is_file());

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_prefix_with_nothing_linked_out_has_nothing_to_watch() {
        let base = scratch("no-links");
        let prefix = base.join("prefix");
        fs::create_dir_all(prefix.join("drive_c/users/you/Documents")).unwrap();

        assert!(snapshot(&prefix).is_empty());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_loose_file_is_never_adopted() {
        // Someone's spreadsheet, saved while the game was running, is
        // not the game's save data.
        let (base, prefix, home_docs) = fixture("loose-file");
        let shots = snapshot(&prefix);

        let session_start = now_millis();
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(&home_docs.join("budget.ods"), "not a save");

        let moved = migrate_after_session(&shots, session_start);
        assert!(moved.moved.is_empty());
        assert!(home_docs.join("budget.ods").is_file());

        fs::remove_dir_all(&base).unwrap();
    }
}

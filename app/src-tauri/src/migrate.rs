// One-time moves of state left behind by a rename.
//
// The app's data directory is derived from the bundle identifier, so
// settling on `dev.knap.games-galore` moved it — and with it
// installs.json (what is installed and where), settings.json (the
// server address, install root, emulators, Title IDs) and the startup
// cache. None of that is catastrophic to lose, but all of it is
// irritating to re-enter, and an app that comes up claiming nothing is
// installed looks broken rather than renamed.
//
// Deliberately a move rather than a copy, and only when there is
// nothing at the new path to overwrite: running the old build again
// afterwards would find its directory gone, which is the honest
// outcome — the data lives in one place, and that place is the new one.

use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

/// The identifier this app shipped under before the rename.
const LEGACY_IDENTIFIER: &str = "dev.knap.vault";

pub fn move_legacy_app_data(app: &AppHandle) {
    let Ok(current) = app.path().app_data_dir() else {
        return;
    };
    let Some(legacy) = legacy_dir_for(&current, LEGACY_IDENTIFIER) else {
        return;
    };
    if let Err(e) = migrate_dir(&legacy, &current) {
        // Nothing here is worth refusing to start over: the old
        // directory is still on disk to be moved by hand, and the app
        // works perfectly well from a fresh one.
        crate::log_line!("could not move data from {}: {e}", legacy.display());
    }
}

/// Where the old directory would be, given where the new one is.
///
/// Derived from the current path rather than rebuilt from the platform
/// rules, so it follows whatever the running platform actually decided
/// — including an XDG_DATA_HOME the user has moved somewhere unusual.
/// Returns None if the current path doesn't end in the identifier at
/// all, which means this platform doesn't name the directory after it
/// and there is nothing to guess from.
fn legacy_dir_for(current: &Path, legacy_identifier: &str) -> Option<PathBuf> {
    let parent = current.parent()?;
    let legacy = parent.join(legacy_identifier);
    (legacy != current).then_some(legacy)
}

/// Moves `legacy` to `current`, doing nothing at all if the new
/// directory already exists (this has already run, or the app has been
/// used under the new name) or the old one doesn't.
fn migrate_dir(legacy: &Path, current: &Path) -> std::io::Result<bool> {
    if !legacy.is_dir() {
        return Ok(false);
    }
    // An empty directory Tauri created on the way past doesn't count as
    // "already has data" — it would otherwise block the move for good.
    let current_has_content = std::fs::read_dir(current)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    if current_has_content {
        return Ok(false);
    }

    if let Some(parent) = current.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_dir(current); // the empty one, if it exists
    std::fs::rename(legacy, current)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_old_directory_is_derived_from_the_new_one() {
        let current = Path::new("/home/me/.local/share/dev.knap.games-galore");
        assert_eq!(
            legacy_dir_for(current, "dev.knap.vault"),
            Some(PathBuf::from("/home/me/.local/share/dev.knap.vault"))
        );
    }

    #[test]
    fn nothing_is_guessed_when_the_two_names_would_collide() {
        let current = Path::new("/home/me/.local/share/dev.knap.vault");
        assert_eq!(legacy_dir_for(current, "dev.knap.vault"), None);
    }

    #[test]
    fn state_follows_the_rename() {
        let root = temp("gg-migrate-moves");
        let legacy = root.join("dev.knap.vault");
        let current = root.join("dev.knap.games-galore");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("installs.json"), "{\"PC/One\":{}}").unwrap();

        assert!(migrate_dir(&legacy, &current).unwrap());
        assert!(!legacy.exists(), "the old directory should be gone");
        assert_eq!(
            fs::read_to_string(current.join("installs.json")).unwrap(),
            "{\"PC/One\":{}}"
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_empty_new_directory_does_not_block_the_move() {
        // Tauri creates the data directory on its own the first time
        // anything asks for it, so "it exists" is not the same as "it
        // has been used".
        let root = temp("gg-migrate-empty");
        let legacy = root.join("dev.knap.vault");
        let current = root.join("dev.knap.games-galore");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("settings.json"), "{}").unwrap();
        fs::create_dir_all(&current).unwrap();

        assert!(migrate_dir(&legacy, &current).unwrap());
        assert!(current.join("settings.json").exists());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn real_data_at_the_new_path_is_never_overwritten() {
        let root = temp("gg-migrate-keeps");
        let legacy = root.join("dev.knap.vault");
        let current = root.join("dev.knap.games-galore");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("installs.json"), "old").unwrap();
        fs::create_dir_all(&current).unwrap();
        fs::write(current.join("installs.json"), "new").unwrap();

        assert!(!migrate_dir(&legacy, &current).unwrap());
        assert_eq!(
            fs::read_to_string(current.join("installs.json")).unwrap(),
            "new"
        );
        assert!(legacy.exists(), "the old directory is left for the user");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_first_run_with_nothing_to_move_is_not_an_error() {
        let root = temp("gg-migrate-fresh");
        let legacy = root.join("dev.knap.vault");
        let current = root.join("dev.knap.games-galore");
        assert!(!migrate_dir(&legacy, &current).unwrap());
        fs::remove_dir_all(&root).unwrap();
    }
}

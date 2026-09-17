// Launches an installed game with the emulator configured for its
// platform, in fullscreen. Command and arguments come from settings
// (settings.rs) rather than being hardcoded here — at least PCSX2 and
// Eden are likely to be Flatpaks, and there's no single "the emulator
// lives here" assumption that holds across native installs and
// Flatpak installs alike.

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use tauri::{AppHandle, Emitter};

use crate::settings::{get_settings, Settings};

/// The part of the launch command that's fixed per platform —
/// fullscreen flags and where the game path goes — independent of
/// *how* the emulator itself gets invoked. This is genuinely fixed
/// (DuckStation's `-fullscreen -batch --` isn't going to change based
/// on whether it's native or Flatpak-wrapped), unlike the command and
/// prefix args, which are user configuration.
fn platform_args(platform: &str, path: &str) -> Vec<String> {
    match platform {
        "PS1" | "PS2" => vec!["-fullscreen".into(), "-batch".into(), "--".into(), path.into()],
        "PC" => vec![path.into()],
        // Eden's standard AppImage build takes a bare positional path
        // with -f for fullscreen — no --game flag, no --fullscreen long
        // form. This is specific to that build, confirmed against a
        // working invocation rather than assumed from the CLI-only
        // build's docs, which use a different argument shape entirely.
        "Switch" => vec!["-f".into(), path.into()],
        _ => vec![path.into()],
    }
}

/// Executables that ship alongside a PC game but aren't the game — its
/// uninstaller, bundled runtime installers, crash reporters, separate
/// config tools. Kept in step with NON_GAME_EXE_MARKERS in the server's
/// library.py, which applies the same rule when cataloguing.
const NON_GAME_EXE_MARKERS: &[&str] = &[
    "unins",
    "setup",
    "install",
    "redist",
    "vcredist",
    "directx",
    "dxsetup",
    "dotnet",
    "oalinst",
    "crashhandler",
    "crashreport",
    "crashpad",
    "config",
];

/// Depth limit for the install-directory scan. Game trees are nowhere
/// near this deep; it's a backstop, not a constraint.
const MAX_SCAN_DEPTH: usize = 24;

/// Collects every file under `dir`, at any depth. A PC game is an
/// installed tree, so a non-recursive listing of the install directory
/// will frequently not contain the executable at all.
///
/// Symlinks are not followed and the depth is capped, for the same
/// reason the save walk refuses them: a link pointing back up its own
/// tree turns this into an infinite recursion, and one pointing out of
/// the install directory would offer something outside it as a thing
/// to launch.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    collect_files_to_depth(dir, 0, out);
}

fn collect_files_to_depth(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth >= MAX_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // symlink_metadata rather than is_dir()/is_file(), both of
        // which resolve the link and report on its target.
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            collect_files_to_depth(&path, depth + 1, out);
        } else if meta.is_file() {
            out.push(path);
        }
    }
}

/// Lowercased and stripped of punctuation, for comparing an executable's
/// name against the game's folder title without tripping over
/// "Moth & Ember" vs. "MothAndEmber" style differences.
fn normalized(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Every .exe in the tree, best first. Mirrors _pick_pc_executable in
/// the server's library.py: something that isn't an installer or
/// bundled runtime beats one that is, then a name matching the game's
/// own title, then the shallowest, then the largest, breaking ties on
/// name so the order is stable across launches.
///
/// Sorting rather than picking a single winner is what lets the UI
/// offer the whole list when a title ships more than one — the head of
/// this list is the automatic choice, and the rest are what someone
/// picks from when the automatic choice is the wrong one. An installer
/// ranks last but is still included: it beats refusing to launch, and
/// for some titles it genuinely is the only executable present.
///
/// The server recorded its own pick when cataloguing, but that says
/// nothing about what actually made it onto this disk, so the ranking
/// is applied again here against the real install directory.
fn ranked_executables(install_dir: &Path, files: &[PathBuf]) -> Vec<PathBuf> {
    let title = normalized(
        &install_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
    );

    let mut exes: Vec<PathBuf> = files
        .iter()
        .filter(|p| has_extension(p, "exe"))
        .cloned()
        .collect();

    exes.sort_by_key(|p| {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let is_non_game = NON_GAME_EXE_MARKERS.iter().any(|m| name.contains(m)) as u8;

        let stem = normalized(&p.file_stem().unwrap_or_default().to_string_lossy());
        let title_match = if stem == title {
            0
        } else if !stem.is_empty() && (title.contains(&stem) || stem.contains(&title)) {
            1
        } else {
            2
        };

        let depth = p.strip_prefix(install_dir).map(|r| r.components().count()).unwrap_or(0);
        let size = p.metadata().map(|m| m.len()).unwrap_or(0);
        (is_non_game, title_match, depth, std::cmp::Reverse(size), name)
    });

    exes
}

/// Everything in the install directory worth offering as a thing to
/// launch, best first. PC titles list their executables; PS1/PS2 list
/// their .cue sheets, of which a multi-disc title has one per disc.
/// Switch titles list nothing — a base game and its updates aren't
/// alternatives to each other, so there's no choice to present.
fn launch_candidates(install_dir: &Path, platform: &str) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = Vec::new();
    collect_files(install_dir, &mut entries);
    entries.sort(); // fs::read_dir order is arbitrary and OS-dependent —
                     // without this, which file gets picked when a Switch
                     // install has more than one (base + update, say) isn't
                     // even stable across runs, let alone predictable.

    match platform {
        "PC" => ranked_executables(install_dir, &entries),
        "PS1" | "PS2" => entries.into_iter().filter(|p| has_extension(p, "cue")).collect(),
        _ => Vec::new(),
    }
}

/// Paths relative to the install directory, POSIX-style — stable
/// identifiers the frontend can show in a list and hand straight back
/// to launch_game.
fn relative_names(install_dir: &Path, paths: Vec<PathBuf>) -> Vec<String> {
    paths
        .into_iter()
        .filter_map(|p| {
            p.strip_prefix(install_dir)
                .ok()
                .map(|r| r.to_string_lossy().replace('\\', "/"))
        })
        .collect()
}

/// Backs the UI's launch picker: the choices for this installed title,
/// best first. One or zero entries means there's nothing to choose
/// between and the picker stays hidden.
#[tauri::command]
pub fn list_launch_candidates(install_dir: String, platform: String) -> Vec<String> {
    let dir = Path::new(&install_dir);
    relative_names(dir, launch_candidates(dir, &platform))
}

/// Resolves a caller-supplied relative path against the install
/// directory, refusing anything that escapes it. This one is worth
/// being strict about: unlike a download, the value here ends up as
/// the program that gets spawned, so an unchecked `../` would turn a
/// dropdown selection into "run an arbitrary binary on this machine".
fn resolve_chosen(install_dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let rel = Path::new(relative);
    if rel.is_absolute() || rel.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("invalid executable path: {relative}"));
    }

    let base = install_dir
        .canonicalize()
        .map_err(|e| format!("{}: {e}", install_dir.display()))?;
    let resolved = install_dir
        .join(rel)
        .canonicalize()
        .map_err(|e| format!("{relative}: {e}"))?;

    // Checked after canonicalising as well, so a symlink pointing out
    // of the install directory can't stand in for a `../`.
    if !resolved.starts_with(&base) {
        return Err(format!("{relative} is outside the install directory"));
    }
    if !resolved.is_file() {
        return Err(format!("{relative} is not a file"));
    }
    Ok(resolved)
}

/// Picks which file in the install directory to hand the emulator when
/// the caller hasn't chosen one. The head of the candidate list, or —
/// for a platform with no candidates, or a title whose layout produced
/// none — whatever file is there.
fn find_local_game_file(install_dir: &Path, platform: &str) -> Option<PathBuf> {
    if let Some(best) = launch_candidates(install_dir, platform).into_iter().next() {
        return Some(best);
    }

    let mut entries: Vec<PathBuf> = Vec::new();
    collect_files(install_dir, &mut entries);
    entries.sort();
    entries.into_iter().next()
}

/// Where a game's own Wine prefix lives. Each PC title gets one rather
/// than sharing the default `~/.wine`, for two reasons: it isolates
/// games from each other's runtime installs and registry, and it makes
/// the prefix's user directory *be* that game's save data — which is
/// what lets cloud saves work for PC at all without a per-game manifest
/// of where each game hides its saves.
///
/// Defaults to `.wine-prefixes` beside the install root, so this needs
/// no configuration; `prefix_root` in settings overrides it.
pub fn prefix_dir(settings: &Settings, platform: &str, game_id: &str) -> Option<PathBuf> {
    if platform != "PC" {
        return None;
    }
    let (_, title) = game_id.split_once('/')?;
    let root = if settings.prefix_root.trim().is_empty() {
        if settings.install_root.trim().is_empty() {
            return None;
        }
        Path::new(&settings.install_root).join(".wine-prefixes")
    } else {
        PathBuf::from(&settings.prefix_root)
    };
    Some(root.join(title))
}

/// Waits for the session to actually finish, then tells the frontend.
///
/// The child is *not* killed or reaped in a way that ties its lifetime
/// to this app — closing Games Galore still leaves a running game
/// running. This only observes.
///
/// For Wine the child process is the wrong thing to wait on: `wine
/// game.exe` frequently returns long before the game does, because the
/// real process is owned by the prefix's wineserver. `wineserver -w`
/// blocks until everything in that prefix has exited, which is the
/// actual end of the session — and is only meaningful *because* each
/// game has its own prefix, since on a shared prefix it would wait for
/// every Wine game at once.
fn supervise(app: AppHandle, mut child: std::process::Child, game_id: String, prefix: Option<PathBuf>) {
    std::thread::spawn(move || {
        let _ = child.wait();
        if let Some(prefix) = prefix {
            let _ = Command::new("wineserver")
                .arg("-w")
                .env("WINEPREFIX", &prefix)
                .status();
        }
        // Best-effort: a missing listener is not worth reporting, and
        // there is nothing to retry against.
        let _ = app.emit("game:exited", &game_id);
    });
}

/// `executable` is a path relative to the install directory, as listed
/// by list_launch_candidates — the UI's picker passes back whichever
/// entry is selected. Left unset, the automatic choice is used, which
/// is what happens for every title that only has one candidate.
#[tauri::command]
pub fn launch_game(
    app: AppHandle,
    install_dir: String,
    platform: String,
    game_id: String,
    executable: Option<String>,
) -> Result<(), String> {
    let settings = get_settings(app.clone());
    let emu = settings
        .emulators
        .get(&platform)
        .ok_or_else(|| format!("no emulator configured for platform \"{platform}\""))?;

    let dir = Path::new(&install_dir);
    let file = match executable.as_deref().filter(|s| !s.is_empty()) {
        Some(chosen) => resolve_chosen(dir, chosen)?,
        None => find_local_game_file(dir, &platform)
            .ok_or_else(|| format!("no game file found in {}", dir.display()))?,
    };
    let file_str = file.to_string_lossy().to_string();

    // Prefix args (e.g. Flatpak's `run <app-id> --`) come first, then
    // the platform's own fullscreen/path args — so a Flatpak PCSX2
    // ends up as: flatpak run net.pcsx2.PCSX2 -- -fullscreen -batch -- <path>
    // where the first `--` is Flatpak's own separator and the second
    // is PCSX2's, each ending a different program's option parsing.
    let mut args = emu.args_prefix.clone();
    args.extend(platform_args(&platform, &file_str));

    eprintln!("launch_game: {} {}", emu.command, shell_quote(&args));

    let mut command = Command::new(&emu.command);
    command.args(&args);

    // Plenty of Windows games resolve their data — and write their
    // saves — relative to the working directory rather than to the
    // executable's own location. Inheriting this app's working
    // directory would scatter those files wherever Games Galore was
    // started from, so the game's own folder is the only sane choice.
    if let Some(parent) = file.parent() {
        command.current_dir(parent);
    }

    let prefix = prefix_dir(&settings, &platform, &game_id);
    if let Some(prefix) = &prefix {
        // Wine creates a missing prefix itself on first run; this just
        // makes sure the parent exists so it has somewhere to do that.
        if let Some(parent) = prefix.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        command.env("WINEPREFIX", prefix);
    }

    // Detached: the emulator's lifetime isn't tied to this app, so
    // closing Games Galore doesn't take a running game down with it.
    let child = command
        .spawn()
        .map_err(|e| format!("failed to launch {}: {e}", emu.command))?;

    supervise(app, child, game_id, prefix);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Builds a throwaway game tree and returns its directory. `files`
    /// are (relative path, byte length) pairs.
    ///
    /// The directory carries a unique counter as well as the game name:
    /// tests run in parallel by default and several of them use the
    /// same game, so keying only on the name lets one test's cleanup
    /// delete a directory another is still reading. The name stays in
    /// the path because the ranking compares executables against the
    /// game's folder title, so it has to be the real one.
    fn fixture(name: &str, files: &[(&str, usize)]) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        let root = std::env::temp_dir()
            .join(format!(
                "gg-launcher-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ))
            .join(name);
        let _ = fs::remove_dir_all(&root);
        for (rel, size) in files {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, vec![0u8; *size]).unwrap();
        }
        root
    }

    #[test]
    fn pc_finds_exe_in_subdirectory_and_skips_installers() {
        let dir = fixture(
            "Hollow Meridian",
            &[
                ("unins000.exe", 900_000),
                ("bin/HollowMeridian.exe", 40_000),
                ("bin/steam_api64.dll", 2_000),
                ("data/pak01.vpk", 9_000_000),
                ("redist/vcredist_x64.exe", 8_000_000),
            ],
        );
        // The old non-recursive pick took the alphabetically first
        // top-level file, which here is the uninstaller.
        assert_eq!(
            find_local_game_file(&dir, "PC").unwrap(),
            dir.join("bin/HollowMeridian.exe")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pc_prefers_shallower_exe_when_none_matches_the_title() {
        let dir = fixture(
            "Moth & Ember",
            &[
                ("Launch.exe", 1_000),
                ("engine/CrashHandler.exe", 50_000),
                ("engine/game_x64.exe", 20_000),
            ],
        );
        assert_eq!(
            find_local_game_file(&dir, "PC").unwrap(),
            dir.join("Launch.exe")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pc_falls_back_to_an_installer_rather_than_refusing_to_launch() {
        let dir = fixture("Setup Only", &[("setup.exe", 10)]);
        assert_eq!(
            find_local_game_file(&dir, "PC").unwrap(),
            dir.join("setup.exe")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ps1_still_resolves_to_the_cue() {
        let dir = fixture(
            "Static Choir",
            &[("Static Choir.bin", 600_000), ("Static Choir.cue", 300)],
        );
        assert_eq!(
            find_local_game_file(&dir, "PS1").unwrap(),
            dir.join("Static Choir.cue")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn switch_pick_is_stable_across_calls() {
        let dir = fixture("198X", &[("update.nsp", 20), ("base.nsp", 10)]);
        let first = find_local_game_file(&dir, "Switch").unwrap();
        assert_eq!(first, dir.join("base.nsp"));
        assert_eq!(find_local_game_file(&dir, "Switch").unwrap(), first);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_install_dir_yields_none() {
        let dir = std::env::temp_dir().join("gg-launcher-test-does-not-exist");
        assert!(find_local_game_file(&dir, "PC").is_none());
    }

    #[test]
    fn candidates_are_listed_best_first_with_installers_last() {
        let dir = fixture(
            "Hollow Meridian",
            &[
                ("unins000.exe", 900_000),
                ("bin/HollowMeridian.exe", 40_000),
                ("bin/steam_api64.dll", 2_000),
                ("data/pak01.vpk", 9_000_000),
                ("redist/vcredist_x64.exe", 8_000_000),
            ],
        );
        let names = relative_names(&dir, launch_candidates(&dir, "PC"));
        assert_eq!(
            names,
            vec!["bin/HollowMeridian.exe", "unins000.exe", "redist/vcredist_x64.exe"]
        );
        // The head of the list is exactly what the automatic pick uses.
        assert_eq!(find_local_game_file(&dir, "PC").unwrap(), dir.join(&names[0]));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_single_executable_gives_nothing_to_choose_between() {
        let dir = fixture("Ferrofluid", &[("Ferrofluid.exe", 10), ("assets.dat", 20)]);
        assert_eq!(launch_candidates(&dir, "PC").len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn multi_disc_ps1_titles_list_every_cue() {
        let dir = fixture(
            "Static Choir",
            &[
                ("Static Choir (Disc 1).cue", 300),
                ("Static Choir (Disc 1).bin", 600_000),
                ("Static Choir (Disc 2).cue", 300),
                ("Static Choir (Disc 2).bin", 600_000),
            ],
        );
        assert_eq!(
            relative_names(&dir, launch_candidates(&dir, "PS1")),
            vec!["Static Choir (Disc 1).cue", "Static Choir (Disc 2).cue"]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn switch_titles_offer_no_choice() {
        let dir = fixture("198X", &[("base.nsp", 10), ("update.nsz", 20)]);
        assert!(launch_candidates(&dir, "Switch").is_empty());
        // Still launchable, just not choosable.
        assert!(find_local_game_file(&dir, "Switch").is_some());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_chosen_executable_resolves_inside_the_install_directory() {
        let dir = fixture("Hollow Meridian", &[("bin/HollowMeridian.exe", 10)]);
        assert_eq!(
            resolve_chosen(&dir, "bin/HollowMeridian.exe").unwrap(),
            dir.join("bin/HollowMeridian.exe").canonicalize().unwrap()
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_chosen_path_escaping_the_install_directory_is_refused() {
        let dir = fixture("Hollow Meridian", &[("game.exe", 10)]);
        for bad in ["../../../bin/sh", "/bin/sh", "bin/../../../../bin/sh"] {
            assert!(
                resolve_chosen(&dir, bad).is_err(),
                "should have refused {bad}"
            );
        }
        // A file that simply isn't there is refused too, rather than
        // being handed to the emulator to fail on later.
        assert!(resolve_chosen(&dir, "nope.exe").is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    fn settings_with(install_root: &str, prefix_root: &str) -> Settings {
        Settings {
            install_root: install_root.to_string(),
            prefix_root: prefix_root.to_string(),
            ..Settings::default()
        }
    }

    #[test]
    fn each_pc_title_gets_its_own_prefix_beside_the_install_root() {
        let settings = settings_with("/games", "");
        assert_eq!(
            prefix_dir(&settings, "PC", "PC/Hollow Meridian"),
            Some(PathBuf::from("/games/.wine-prefixes/Hollow Meridian"))
        );
        // Two titles never share one, which is the whole point.
        assert_ne!(
            prefix_dir(&settings, "PC", "PC/Hollow Meridian"),
            prefix_dir(&settings, "PC", "PC/Moth & Ember")
        );
    }

    #[test]
    fn an_explicit_prefix_root_overrides_the_default_location() {
        let settings = settings_with("/games", "/mnt/ssd/prefixes");
        assert_eq!(
            prefix_dir(&settings, "PC", "PC/Hollow Meridian"),
            Some(PathBuf::from("/mnt/ssd/prefixes/Hollow Meridian"))
        );
    }

    #[test]
    fn only_pc_titles_have_a_prefix() {
        let settings = settings_with("/games", "");
        for platform in ["Switch", "PS1", "PS2"] {
            assert_eq!(prefix_dir(&settings, platform, &format!("{platform}/X")), None);
        }
    }

    #[test]
    fn no_install_root_and_no_prefix_root_means_no_prefix() {
        // Rather than silently composing a path relative to nothing.
        assert_eq!(prefix_dir(&settings_with("", ""), "PC", "PC/X"), None);
    }

    #[test]
    fn flatpak_prefix_args_precede_the_platform_args() {
        let args: Vec<String> = ["run", "net.pcsx2.PCSX2", "--"]
            .iter()
            .map(|s| s.to_string())
            .chain(platform_args("PS2", "/games/x.cue"))
            .collect();
        assert_eq!(
            args,
            vec!["run", "net.pcsx2.PCSX2", "--", "-fullscreen", "-batch", "--", "/games/x.cue"]
        );
    }

    #[test]
    fn shell_quote_escapes_embedded_quotes_and_spaces() {
        assert_eq!(
            shell_quote(&["/games/Moth & Ember/it's.exe".to_string()]),
            r#"'/games/Moth & Ember/it'\''s.exe'"#
        );
    }
}

/// Renders an argument list as a copy-pasteable, POSIX-shell-safe
/// command line — single-quoting each argument (and escaping any
/// literal single quotes within one) so the printed line is directly
/// runnable even when a path contains spaces, rather than needing to
/// be reconstructed or re-quoted by hand before testing it manually.
fn shell_quote(args: &[String]) -> String {
    args.iter()
        .map(|a| format!("'{}'", a.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

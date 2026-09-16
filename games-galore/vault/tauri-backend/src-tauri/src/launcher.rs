// Launches an installed game with the emulator configured for its
// platform, in fullscreen. Command and arguments come from settings
// (settings.rs) rather than being hardcoded here — at least PCSX2 and
// Eden are likely to be Flatpaks, and there's no single "the emulator
// lives here" assumption that holds across native installs and
// Flatpak installs alike.

use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::AppHandle;

use crate::settings::get_settings;

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

/// Collects every file under `dir`, at any depth. A PC game is an
/// installed tree, so a non-recursive listing of the install directory
/// will frequently not contain the executable at all.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else if path.is_file() {
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

/// Picks the .exe to hand Wine. Mirrors _pick_pc_executable in the
/// server's library.py: prefer something that isn't an installer or
/// bundled runtime, then a name matching the game's own title, then the
/// shallowest, then the largest, breaking ties on name so the choice is
/// stable across launches.
///
/// The server already recorded its own pick in the catalog, but that
/// says nothing about what actually made it onto this disk, so the
/// decision is made again here against the real install directory.
fn pick_pc_executable(install_dir: &Path, files: &[PathBuf]) -> Option<PathBuf> {
    let exes: Vec<&PathBuf> = files
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")))
        .collect();
    if exes.is_empty() {
        return None;
    }

    let is_non_game = |p: &PathBuf| {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        NON_GAME_EXE_MARKERS.iter().any(|m| name.contains(m))
    };

    // Everything looking like an installer still beats refusing to
    // launch at all — fall back to the full list rather than giving up.
    let preferred: Vec<&PathBuf> = exes.iter().copied().filter(|p| !is_non_game(p)).collect();
    let candidates = if preferred.is_empty() { exes } else { preferred };

    let title = normalized(&install_dir.file_name()?.to_string_lossy());

    candidates
        .into_iter()
        .min_by_key(|p| {
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
            let name = p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            (title_match, depth, std::cmp::Reverse(size), name)
        })
        .cloned()
}

/// Picks which file in the install directory to hand the emulator.
/// PC titles resolve to their executable, searched for across the whole
/// installed tree. PS1/PS2 titles that came as a .bin/.cue pair resolve
/// to the .cue, mirroring the rule the source library scanner uses.
fn find_local_game_file(install_dir: &Path, platform: &str) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = Vec::new();
    collect_files(install_dir, &mut entries);
    entries.sort(); // fs::read_dir order is arbitrary and OS-dependent —
                     // without this, which file gets picked when a Switch
                     // install has more than one (base + update, say) isn't
                     // even stable across runs, let alone predictable.

    match platform {
        "PC" => {
            if let Some(exe) = pick_pc_executable(install_dir, &entries) {
                return Some(exe);
            }
        }
        "PS1" | "PS2" => {
            if let Some(cue) = entries.iter().find(|p| p.extension().is_some_and(|e| e == "cue")) {
                return Some(cue.clone());
            }
        }
        _ => {}
    }

    entries.into_iter().next()
}

#[tauri::command]
pub fn launch_game(app: AppHandle, install_dir: String, platform: String) -> Result<(), String> {
    let settings = get_settings(app);
    let emu = settings
        .emulators
        .get(&platform)
        .ok_or_else(|| format!("no emulator configured for platform \"{platform}\""))?;

    let dir = Path::new(&install_dir);
    let file = find_local_game_file(dir, &platform)
        .ok_or_else(|| format!("no game file found in {}", dir.display()))?;
    let file_str = file.to_string_lossy().to_string();

    // Prefix args (e.g. Flatpak's `run <app-id> --`) come first, then
    // the platform's own fullscreen/path args — so a Flatpak PCSX2
    // ends up as: flatpak run net.pcsx2.PCSX2 -- -fullscreen -batch -- <path>
    // where the first `--` is Flatpak's own separator and the second
    // is PCSX2's, each ending a different program's option parsing.
    let mut args = emu.args_prefix.clone();
    args.extend(platform_args(&platform, &file_str));

    eprintln!("launch_game: {} {}", emu.command, shell_quote(&args));

    // Detached: the emulator's lifetime isn't tied to this app, so
    // closing Games Galore doesn't take a running game down with it.
    Command::new(&emu.command)
        .args(&args)
        .spawn()
        .map_err(|e| format!("failed to launch {}: {e}", emu.command))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Builds a throwaway game tree and returns its directory.
    /// `files` are (relative path, byte length) pairs.
    fn fixture(name: &str, files: &[(&str, usize)]) -> PathBuf {
        let root = std::env::temp_dir()
            .join(format!("gg-launcher-test-{}-{}", std::process::id(), name));
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

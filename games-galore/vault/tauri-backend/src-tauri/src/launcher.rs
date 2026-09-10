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

/// Picks which file in the install directory to hand the emulator.
/// PS1/PS2 titles that came as a .bin/.cue pair resolve to the .cue,
/// mirroring the same rule the source library scanner uses. Switch and
/// PC titles are expected to have exactly one meaningful file already,
/// since install_game only ever downloads what the catalog listed.
fn find_local_game_file(install_dir: &Path, platform: &str) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(install_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort(); // fs::read_dir order is arbitrary and OS-dependent —
                     // without this, which file gets picked when a Switch
                     // install has more than one (base + update, say) isn't
                     // even stable across runs, let alone predictable.

    if platform == "PS1" || platform == "PS2" {
        if let Some(cue) = entries.iter().find(|p| p.extension().is_some_and(|e| e == "cue")) {
            return Some(cue.clone());
        }
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
    // closing Vault doesn't take a running game down with it.
    Command::new(&emu.command)
        .args(&args)
        .spawn()
        .map_err(|e| format!("failed to launch {}: {e}", emu.command))?;

    Ok(())
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

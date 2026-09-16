// Persists the handful of user-configurable settings — the library
// server's address, where installs land locally, the sound preference,
// and now per-platform emulator launch configuration — to
// settings.json in the app's data directory, right alongside
// installs.json.
//
// Emulator config exists here rather than being hardcoded in
// launcher.rs because at least PCSX2 and Eden are likely to be
// installed as Flatpaks, not native binaries on PATH — a Flatpak
// invocation needs a completely different command shape
// (`flatpak run <app-id> -- <the emulator's own args>`), and there's
// no way to know someone's actual app-id or binary name in advance.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// How to invoke one platform's emulator. `command` is the executable
/// to spawn — a native binary name/path, or "flatpak" for a Flatpak
/// install. `args_prefix` is inserted *before* the game-specific
/// fullscreen/path arguments launcher.rs builds — for a native binary
/// this is normally empty; for a Flatpak it's something like
/// `["run", "net.pcsx2.PCSX2", "--"]`, where that trailing `--` is
/// Flatpak's own separator ending its option parsing, distinct from
/// whatever separator the emulator itself also wants afterward.
/// `version_flag` is used by the dependency check for native binaries
/// only — ignored for Flatpaks, which are checked via `flatpak info`
/// instead (see dependencies.rs).
#[derive(Serialize, Deserialize, Clone)]
pub struct EmulatorConfig {
    pub command: String,
    pub args_prefix: Vec<String>,
    pub version_flag: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Settings {
    pub server_base: String,
    pub install_root: String,
    pub sound_enabled: bool,
    #[serde(default = "default_emulators")]
    pub emulators: HashMap<String, EmulatorConfig>,
    /// Which file to launch for a given game id, when the automatic
    /// choice isn't the right one — keyed by `Game.id`, valued with a
    /// path relative to that game's install directory. Only titles
    /// someone has actually picked for appear here; everything else
    /// resolves through launcher.rs's ranking at launch time.
    ///
    /// Defaulted rather than required, so a settings.json written
    /// before this field existed still loads instead of being silently
    /// discarded and replaced with defaults.
    #[serde(default)]
    pub launch_overrides: HashMap<String, String>,
}

/// Best-known defaults — native binary names and the version flags
/// confirmed against each project's actual CLI docs where possible.
/// PCSX2's exact flag is a reasonable guess, not confirmed the way
/// DuckStation's and Eden's are; all four are meant to be edited once
/// someone's actual install (especially any Flatpak) is known.
///
/// Switch is deliberately left as an obvious placeholder rather than a
/// real binary name. Eden ships multiple builds with genuinely
/// different CLI conventions (a "standard" AppImage taking a bare
/// positional path with -f for fullscreen, vs. a separate eden-cli
/// build using --game/--fullscreen) — platform_args() in launcher.rs
/// is written for the AppImage shape, so defaulting this to a plain
/// command name would silently send the wrong flags to anyone on a
/// different build. An AppImage path is inherently personal and
/// versioned besides, so there's no real default worth hardcoding here.
fn default_emulators() -> HashMap<String, EmulatorConfig> {
    let mut m = HashMap::new();
    m.insert(
        "PS1".to_string(),
        EmulatorConfig { command: "duckstation-qt".to_string(), args_prefix: vec![], version_flag: "-version".to_string() },
    );
    m.insert(
        "PS2".to_string(),
        EmulatorConfig { command: "pcsx2-qt".to_string(), args_prefix: vec![], version_flag: "--version".to_string() },
    );
    m.insert(
        "PC".to_string(),
        EmulatorConfig { command: "wine".to_string(), args_prefix: vec![], version_flag: "--version".to_string() },
    );
    m.insert(
        "Switch".to_string(),
        EmulatorConfig { command: "/path/to/Eden.AppImage".to_string(), args_prefix: vec![], version_flag: "--version".to_string() },
    );
    m
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server_base: String::new(),
            install_root: String::new(),
            sound_enabled: true,
            emulators: default_emulators(),
            launch_overrides: HashMap::new(),
        }
    }
}

fn settings_file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("settings.json"))
}

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Settings {
    let path = match settings_file(&app) {
        Ok(p) => p,
        Err(_) => return Settings::default(),
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[tauri::command]
pub fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    let path = settings_file(&app)?;
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

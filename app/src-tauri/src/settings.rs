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
    /// Where each PC title's own Wine prefix lives. Empty means "a
    /// `.wine-prefixes` directory beside the install root", which is
    /// what makes this work with no configuration at all.
    #[serde(default)]
    pub prefix_root: String,
    #[serde(default)]
    pub save_sync: SaveSyncConfig,
    /// Which order the library is shown in. Kept here rather than in
    /// the frontend's own storage so it survives a restart the same
    /// way every other preference does; `default` so a settings.json
    /// written before the sort control came back still loads.
    #[serde(default = "default_sort")]
    pub sort: String,
    /// RAWG key for the metadata fetcher, sent with each request so the
    /// server can use it without one being written into its config.
    /// `default` so a settings.json written before this still loads.
    #[serde(default)]
    pub rawg_key: String,
}

/// Everything cloud saves need that can't be derived.
///
/// `switch_data_dir` is the emulator's own data directory — saves live
/// under it in a fixed tree, so one path covers every Switch title.
/// `title_ids` maps a `Game.id` to the 16-hex-digit Title ID the
/// emulator files that game's saves under, which is the one thing with
/// no relationship to the library's folder names and so the one thing
/// that has to be recorded per game.
///
/// `device_name` only labels uploads in the version history, so you can
/// tell which machine a save came from when deciding between two.
#[derive(Serialize, Deserialize, Clone)]
pub struct SaveSyncConfig {
    pub enabled: bool,
    pub device_name: String,
    pub switch_data_dir: String,
    #[serde(default)]
    pub title_ids: HashMap<String, String>,
    /// Shared secret for the server's save endpoints, empty unless the
    /// server is configured to want one. `default` because a
    /// settings.json written before this existed must still load.
    #[serde(default)]
    pub token: String,
}

impl Default for SaveSyncConfig {
    fn default() -> Self {
        Self {
            // On by default. It stays inert until there's somewhere to
            // sync from — a Switch title needs its data directory and
            // Title ID mapped, a PC title needs a prefix that exists —
            // so this costs nothing until it can actually work, and
            // then works without anyone having to discover a setting.
            //
            // It is not a destructive default: local saves remain the
            // source of truth, and the only automatic overwrite is
            // restoring onto a machine with no save of its own at all.
            // Anything that could lose progress asks first.
            enabled: true,
            device_name: default_device_name(),
            switch_data_dir: String::new(),
            title_ids: HashMap::new(),
            // Empty unless the server asks for one, which is its own
            // default too.
            token: String::new(),
        }
    }
}

/// The machine's hostname where one is available, since the whole point
/// is telling two machines apart in a version list.
fn default_sort() -> String {
    "alpha".to_string()
}

fn default_device_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this machine".to_string())
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
        EmulatorConfig {
            command: "duckstation-qt".to_string(),
            args_prefix: vec![],
            version_flag: "-version".to_string(),
        },
    );
    m.insert(
        "PS2".to_string(),
        EmulatorConfig {
            command: "pcsx2-qt".to_string(),
            args_prefix: vec![],
            version_flag: "--version".to_string(),
        },
    );
    m.insert(
        "PC".to_string(),
        EmulatorConfig {
            command: "wine".to_string(),
            args_prefix: vec![],
            version_flag: "--version".to_string(),
        },
    );
    m.insert(
        "Switch".to_string(),
        EmulatorConfig {
            command: "/path/to/Eden.AppImage".to_string(),
            args_prefix: vec![],
            version_flag: "--version".to_string(),
        },
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
            prefix_root: String::new(),
            save_sync: SaveSyncConfig::default(),
            sort: default_sort(),
            rawg_key: String::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_saves_default_to_on() {
        assert!(Settings::default().save_sync.enabled);
    }

    #[test]
    fn a_default_config_has_nowhere_to_sync_from_yet() {
        // Why defaulting to on is safe: with no emulator directory and
        // no mapped titles, every lookup fails and the feature does
        // nothing until it's pointed at something real.
        let cfg = SaveSyncConfig::default();
        assert!(cfg.switch_data_dir.is_empty());
        assert!(cfg.title_ids.is_empty());
    }

    #[test]
    fn settings_written_before_cloud_saves_existed_still_load() {
        // The field is serde(default), so an older settings.json is
        // upgraded rather than discarded and replaced wholesale.
        let older = r#"{
            "server_base": "http://host:8420",
            "install_root": "/games",
            "sound_enabled": true
        }"#;
        let settings: Settings = serde_json::from_str(older).unwrap();
        assert_eq!(settings.server_base, "http://host:8420");
        assert!(settings.save_sync.enabled, "should pick up the new default");
        assert!(settings.launch_overrides.is_empty());
        assert!(settings.prefix_root.is_empty());
    }

    #[test]
    fn an_explicit_opt_out_survives_a_reload() {
        // Someone who turned it off must stay opted out — the default
        // applies to absent fields, never to a stored false.
        let stored = r#"{
            "server_base": "", "install_root": "", "sound_enabled": true,
            "save_sync": { "enabled": false, "device_name": "x", "switch_data_dir": "" }
        }"#;
        let settings: Settings = serde_json::from_str(stored).unwrap();
        assert!(!settings.save_sync.enabled);
    }
}

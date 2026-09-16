// Tracks local install status per game and does the actual install —
// which is now just "download every file this game has from the
// server." The server has already resolved any .nsz -> .nsp conversion
// before responding, so this module never sees the difference and
// never shells out to anything: whatever comes back on the wire is
// installable as-is.
//
// Persisted as a flat JSON map keyed by Game.id ("Switch/198X") in the
// app's data directory, read-modify-written on every state change.
// Fine at ~500 titles; would want something less naive well past that.

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter, Manager};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::server::{Game, GameFile};

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum InstallStatus {
    NotInstalled,
    Downloading { file: String, pct: u8 },
    Installed { local_dir: PathBuf },
    Failed { message: String },
}

pub type InstallMap = HashMap<String, InstallStatus>;

/// Game ids currently flagged for cancellation. A running install_game
/// checks this cooperatively (there's no hard task-kill here — the
/// download loop just needs to notice and stop on its own, which it
/// does at least once per chunk).
///
/// A plain process-wide static rather than Tauri-managed State: an
/// async command taking `State<'_, T>` directly runs into a genuine
/// macro limitation (`E0581`, "return type references a lifetime which
/// is not constrained by the fn input types") that naming the lifetime
/// explicitly doesn't resolve either — tried both and both hit the
/// same wall. A static sidesteps the whole class of issue, since
/// `&'static Mutex<...>` has no generic lifetime parameter for the
/// async fn macro to trip over, and nothing here needed per-app-instance
/// state anyway (there's only ever one instance of this app running).
fn cancelled_downloads() -> &'static Mutex<HashSet<String>> {
    static CANCELLED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    CANCELLED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn state_file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("installs.json"))
}

fn load_states(app: &AppHandle) -> InstallMap {
    let path = match state_file(app) {
        Ok(p) => p,
        Err(_) => return InstallMap::new(),
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_states(app: &AppHandle, states: &InstallMap) -> Result<(), String> {
    let path = state_file(app)?;
    let json = serde_json::to_string_pretty(states).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// Persists the new status and pushes it to the frontend in one step,
/// so the UI never has to poll — it just listens for "install:status".
fn set_status(app: &AppHandle, id: &str, status: InstallStatus) -> Result<(), String> {
    let mut states = load_states(app);
    states.insert(id.to_string(), status.clone());
    save_states(app, &states)?;
    app.emit("install:status", (id, &status))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_install_states(app: AppHandle) -> InstallMap {
    load_states(&app)
}

/// Reconstructs where a game's files would live on disk from its id and
/// the configured install root, without needing that path to have been
/// stored anywhere first. This is what makes cancel_install able to
/// clean up a download whose owning install_game call isn't running
/// anymore at all — e.g. the app was closed mid-download and relaunched
/// — since there's no live task left to ask, only the id and settings.
fn install_dir_for(install_root: &str, game_id: &str) -> Option<PathBuf> {
    let (platform, title) = game_id.split_once('/')?;
    Some(Path::new(install_root).join(platform).join(title))
}

#[tauri::command]
pub async fn install_game(
    app: AppHandle,
    game: Game,
    server_base: String,
    install_root: String,
) -> Result<(), String> {
    if game.files.is_empty() {
        let message = "no files found for this game".to_string();
        set_status(&app, &game.id, InstallStatus::Failed { message: message.clone() })?;
        return Err(message);
    }

    let dest_dir = Path::new(&install_root).join(&game.platform).join(&game.title);
    fs::create_dir_all(&dest_dir).await.map_err(|e| e.to_string())?;

    for file in &game.files {
        // Checked between files too, not just inside each file's own
        // streaming loop — a game with several small files could
        // otherwise finish all of them before ever noticing a
        // cancellation requested early on.
        if cancelled_downloads().lock().unwrap().remove(&game.id) {
            return Ok(()); // cancel_install already reset status and cleaned up
        }

        match download_file(&app, &game.id, &server_base, file, &dest_dir).await {
            Ok(true) => return Ok(()), // cancelled mid-file; same as above
            Ok(false) => {}            // this file finished; move to the next
            Err(e) => {
                set_status(&app, &game.id, InstallStatus::Failed { message: e.clone() })?;
                return Err(e);
            }
        }
    }

    set_status(&app, &game.id, InstallStatus::Installed { local_dir: dest_dir })?;
    Ok(())
}

/// Returns Ok(true) if cancelled partway through, Ok(false) if the file
/// completed normally. Cancellation isn't treated as an error — it's a
/// deliberate, successful stop, and the caller shouldn't report it as
/// a failure the way an actual network or server error would be.
async fn download_file(
    app: &AppHandle,
    game_id: &str,
    server_base: &str,
    file: &GameFile,
    dest_dir: &Path,
) -> Result<bool, String> {
    // The filename is encoded per-segment for the same reason the id is:
    // it can carry subdirectories of its own ("bin/game.exe") when a PC
    // game's executable lives inside its tree, and those separators have
    // to survive into the URL for the server's route to split them back
    // out. Encoding it in one pass would turn them into %2F.
    let url = format!(
        "{}/download/{}/{}",
        server_base.trim_end_matches('/'),
        encode_path_segments(game_id),
        encode_path_segments(&file.filename),
    );

    let response = reqwest::get(&url).await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "server returned {status} for {}: {}",
            file.filename,
            extract_error_detail(&body)
        ));
    }
    let total = response.content_length().unwrap_or(0);

    let dest_filename = if file.format == "nsz" {
        file.filename.replace(".nsz", ".nsp")
    } else {
        file.filename.clone()
    };

    // dest_filename can be a relative path rather than a bare name (see
    // the URL comment above), so its parent directories may not exist
    // yet — File::create doesn't make them, it just fails.
    let dest_path = dest_dir.join(&dest_filename);
    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
    }
    let mut out = fs::File::create(&dest_path).await.map_err(|e| e.to_string())?;

    let mut stream = response.bytes_stream();
    let mut downloaded: u64 = 0;
    let mut last_reported: i16 = -1; // guarantees the first chunk always emits

    while let Some(chunk) = stream.next().await {
        if cancelled_downloads().lock().unwrap().remove(game_id) {
            return Ok(true);
        }

        let chunk = chunk.map_err(|e| e.to_string())?;
        out.write_all(&chunk).await.map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;

        let pct = if total > 0 { ((downloaded * 100) / total) as i16 } else { 0 };
        if pct != last_reported {
            last_reported = pct;
            set_status(
                app,
                game_id,
                InstallStatus::Downloading { file: file.filename.clone(), pct: pct as u8 },
            )?;
        }
    }

    Ok(false)
}

#[tauri::command]
pub fn uninstall_game(app: AppHandle, game_id: String) -> Result<(), String> {
    let states = load_states(&app);
    if let Some(InstallStatus::Installed { local_dir }) = states.get(&game_id) {
        if local_dir.exists() {
            std::fs::remove_dir_all(local_dir).map_err(|e| e.to_string())?;
        }
    }
    set_status(&app, &game_id, InstallStatus::NotInstalled)
}

/// Cancels an install in progress, or clears one that's stuck — the
/// same operation covers both, since a "stuck" download (its status
/// still reads "downloading" from before the app was last closed, with
/// no install_game task actually running anymore to make progress or
/// ever notice a cancellation flag) needs its files cleaned up and its
/// status reset regardless of whether anything live picks up the flag
/// below. Setting the flag handles the case where a task *is* still
/// running; the unconditional cleanup that follows handles the case
/// where it isn't.
#[tauri::command]
pub fn cancel_install(
    app: AppHandle,
    game_id: String,
    install_root: String,
) -> Result<(), String> {
    cancelled_downloads().lock().unwrap().insert(game_id.clone());

    if let Some(dir) = install_dir_for(&install_root, &game_id) {
        if dir.exists() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    set_status(&app, &game_id, InstallStatus::NotInstalled)
}

/// game_id ("Switch/198X") has a real path separator in it that must
/// stay a literal `/` in the URL — the server's route relies on that
/// to tell platform and title apart. Encoding the whole id in one pass
/// would turn it into `%2F`, which isn't guaranteed to be treated as a
/// separator by the time it reaches Flask's routing. Encoding each
/// segment on its own and rejoining with `/` avoids that ambiguity.
fn encode_path_segments(id: &str) -> String {
    id.split('/')
        .map(|part| urlencoding::encode(part).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Flask's `abort(status, "message")` — used throughout server.py for
/// every real failure (nsz conversion errors, unknown ids, missing
/// dependencies) — renders that message inside a `<p>` tag of an HTML
/// error page. Confirmed directly against Flask rather than assumed:
/// the message is genuinely in the response body, not suppressed the
/// way some frameworks hide custom detail on 5xx responses. Pulling
/// just that line out means the *actual* cause (e.g. nsz's own stderr
/// output) reaches this error instead of getting silently dropped in
/// favor of a bare status code, which is what made every install
/// failure look identical and undiagnosable before this.
fn extract_error_detail(html_body: &str) -> String {
    if let Some(start) = html_body.find("<p>") {
        if let Some(end) = html_body[start..].find("</p>") {
            return html_body[start + 3..start + end].trim().to_string();
        }
    }
    let trimmed = html_body.trim();
    if trimmed.is_empty() {
        "no error detail returned".to_string()
    } else {
        trimmed.chars().take(300).collect()
    }
}

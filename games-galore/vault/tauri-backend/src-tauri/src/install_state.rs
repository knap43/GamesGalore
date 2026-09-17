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
///
/// Reserved for the transitions actually worth recording: an install
/// starting, finishing, failing, or being cleared. Progress within an
/// install goes through emit_progress instead; see there for why.
fn set_status(app: &AppHandle, id: &str, status: InstallStatus) -> Result<(), String> {
    let mut states = load_states(app);
    states.insert(id.to_string(), status.clone());
    save_states(app, &states)?;
    app.emit("install:status", (id, &status))
        .map_err(|e| e.to_string())
}

/// Pushes a progress update to the frontend without touching the state
/// file. Persisting every tick meant a read-modify-write of the whole
/// installs.json per percent per file — tolerable when a title was one
/// file, but a PC game is a tree of thousands, which would have turned
/// a single install into hundreds of thousands of rewrites of a file
/// that grows with the size of the library.
///
/// Nothing is lost by not persisting: the only durable fact worth
/// keeping mid-install is *that* one is in progress, which the
/// persisted Downloading status at the start of install_game already
/// records. An app closed mid-download still reads as "downloading" on
/// next launch, with no live task behind it, and cancel_install
/// already exists to clear exactly that.
///
/// Best-effort by design — a dropped progress frame is not a reason to
/// fail an install that is otherwise proceeding.
fn emit_progress(app: &AppHandle, id: &str, file: &str, pct: u8) {
    let status = InstallStatus::Downloading { file: file.to_string(), pct };
    let _ = app.emit("install:status", (id, &status));
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
pub fn install_dir_for(install_root: &str, game_id: &str) -> Option<PathBuf> {
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

    // Progress is reported against the whole title, not the file being
    // transferred at the moment. A PC game is a tree of many files of
    // wildly different sizes, so a per-file percentage would race to
    // 100% and reset over and over while telling you nothing about how
    // far along the install actually is.
    let total_bytes: u64 = game.files.iter().map(|f| f.size_bytes).sum();
    let mut progress = Progress { done_bytes: 0, total_bytes, last_pct: -1 };

    // Persisted once, so an install interrupted by the app closing is
    // still recognisable as one on next launch. Everything after this
    // is emitted without touching the state file; see emit_progress.
    set_status(
        &app,
        &game.id,
        InstallStatus::Downloading { file: game.files[0].filename.clone(), pct: 0 },
    )?;

    for file in &game.files {
        // Checked between files too, not just inside each file's own
        // streaming loop — a game with several small files could
        // otherwise finish all of them before ever noticing a
        // cancellation requested early on.
        if cancelled_downloads().lock().unwrap().remove(&game.id) {
            return Ok(()); // cancel_install already reset status and cleaned up
        }

        match download_file(&app, &game.id, &server_base, file, &dest_dir, &mut progress).await {
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

/// Running totals for one install, carried across all of its files.
struct Progress {
    done_bytes: u64,
    total_bytes: u64,
    /// Last percentage actually emitted, so a large file doesn't emit
    /// thousands of identical frames. -1 guarantees the first one does.
    last_pct: i16,
}

impl Progress {
    /// The catalog's sizes are what the files occupy in the source
    /// library, and a .nsz decompresses on the way out, so the bytes
    /// arriving can exceed the total that was advertised. Clamped
    /// rather than left to overshoot into a nonsensical percentage.
    fn pct(&self) -> u8 {
        if self.total_bytes == 0 {
            return 0;
        }
        ((self.done_bytes * 100) / self.total_bytes).min(100) as u8
    }
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
    progress: &mut Progress,
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

    // Emitted once per file regardless of whether the overall
    // percentage moved, so the name on screen keeps up while a long
    // tail of small files goes by.
    emit_progress(app, game_id, &file.filename, progress.pct());

    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        if cancelled_downloads().lock().unwrap().remove(game_id) {
            return Ok(true);
        }

        let chunk = chunk.map_err(|e| e.to_string())?;
        out.write_all(&chunk).await.map_err(|e| e.to_string())?;
        progress.done_bytes += chunk.len() as u64;

        let pct = progress.pct() as i16;
        if pct != progress.last_pct {
            progress.last_pct = pct;
            emit_progress(app, game_id, &file.filename, pct as u8);
        }
    }

    // Flushed explicitly rather than at drop, where an error would be
    // silently swallowed — a truncated file that reports success is
    // exactly the kind of install failure that surfaces much later as
    // an emulator crash.
    out.flush().await.map_err(|e| e.to_string())?;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(done: u64, total: u64) -> Progress {
        Progress { done_bytes: done, total_bytes: total, last_pct: -1 }
    }

    #[test]
    fn percentage_runs_across_the_whole_title() {
        // Three files of 100 bytes each: finishing the first is a third
        // of the install, not 100% of it.
        assert_eq!(progress(100, 300).pct(), 33);
        assert_eq!(progress(300, 300).pct(), 100);
    }

    #[test]
    fn percentage_clamps_when_nsz_decompresses_past_its_listed_size() {
        assert_eq!(progress(250, 100).pct(), 100);
    }

    #[test]
    fn percentage_of_an_empty_total_is_zero_not_a_panic() {
        assert_eq!(progress(0, 0).pct(), 0);
    }

    #[test]
    fn percentage_does_not_overflow_on_a_large_title() {
        // done * 100 overflows a u32 well before this; u64 is required.
        let p = progress(80 * 1024 * 1024 * 1024, 100 * 1024 * 1024 * 1024);
        assert_eq!(p.pct(), 80);
    }

    #[test]
    fn path_segments_are_encoded_without_losing_separators() {
        assert_eq!(encode_path_segments("Switch/198X"), "Switch/198X");
        assert_eq!(encode_path_segments("PC/Moth & Ember"), "PC/Moth%20%26%20Ember");
        // A nested filename has to survive the same way, or the
        // server's route can't split it back apart.
        assert_eq!(encode_path_segments("bin/game data/run.exe"), "bin/game%20data/run.exe");
    }

    #[test]
    fn error_detail_is_pulled_out_of_flasks_html_error_page() {
        let body = "<html><title>500</title><body><h1>Error</h1>\
                    <p>nsz conversion failed: bad header</p></body></html>";
        assert_eq!(extract_error_detail(body), "nsz conversion failed: bad header");
        assert_eq!(extract_error_detail("   "), "no error detail returned");
    }

    #[test]
    fn install_dir_is_reconstructed_from_the_game_id() {
        assert_eq!(
            install_dir_for("/games", "PC/Moth & Ember"),
            Some(PathBuf::from("/games/PC/Moth & Ember"))
        );
        // No platform separator means no directory can be derived.
        assert_eq!(install_dir_for("/games", "bare-id"), None);
    }
}

/// game_id ("Switch/198X") has a real path separator in it that must
/// stay a literal `/` in the URL — the server's route relies on that
/// to tell platform and title apart. Encoding the whole id in one pass
/// would turn it into `%2F`, which isn't guaranteed to be treated as a
/// separator by the time it reaches Flask's routing. Encoding each
/// segment on its own and rejoining with `/` avoids that ambiguity.
pub fn encode_path_segments(id: &str) -> String {
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

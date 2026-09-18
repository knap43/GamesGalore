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
    /// Waiting for a slot. A queued install has made no network
    /// requests and written no files yet, so cancelling one is free.
    Queued,
    Downloading {
        file: String,
        pct: u8,
    },
    Installed {
        local_dir: PathBuf,
    },
    Failed {
        message: String,
    },
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

/// How many installs may transfer at once.
///
/// Every click used to start its own download loop immediately, so six
/// queued-up titles meant six streams competing for one link: each one
/// slower than it needed to be, each reporting progress as though it
/// were alone, and the one you actually wanted first finishing last.
/// Two is a deliberate compromise rather than one — a single stream
/// often can't saturate a LAN link on its own, and the second covers
/// the stalls while the server converts an .nsz.
const MAX_CONCURRENT_INSTALLS: usize = 2;

fn install_slots() -> &'static tokio::sync::Semaphore {
    static SLOTS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_INSTALLS))
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
    let status = InstallStatus::Downloading {
        file: file.to_string(),
        pct,
    };
    let _ = app.emit("install:status", (id, &status));
}

#[tauri::command]
pub fn get_install_states(app: AppHandle) -> InstallMap {
    load_states(&app)
}

/// The ids currently on disk, for the catalog cache to scope itself
/// to. Reads the same installs.json everything else here does, so the
/// two files can never disagree about what is installed.
pub fn installed_ids(app: &AppHandle) -> HashSet<String> {
    load_states(app)
        .into_iter()
        .filter(|(_, status)| matches!(status, InstallStatus::Installed { .. }))
        .map(|(id, _)| id)
        .collect()
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
        set_status(
            &app,
            &game.id,
            InstallStatus::Failed {
                message: message.clone(),
            },
        )?;
        return Err(message);
    }

    // Queued before anything else happens, so a title waiting its turn
    // says so on screen instead of looking like a button that did
    // nothing. Cancelling from here is free: no request has been made
    // and no file written.
    set_status(&app, &game.id, InstallStatus::Queued)?;
    let _slot = install_slots().acquire().await.map_err(|e| e.to_string())?;
    if cancelled_downloads().lock().unwrap().remove(&game.id) {
        return Ok(()); // cancelled while it sat in the queue
    }

    let dest_dir = Path::new(&install_root)
        .join(&game.platform)
        .join(&game.title);
    fs::create_dir_all(&dest_dir)
        .await
        .map_err(|e| e.to_string())?;

    // Checked after the directory exists, so statvfs reports the
    // filesystem the files will actually land on rather than whatever
    // the nearest existing ancestor happens to be. Filling a disk is a
    // slow, noisy failure that takes the rest of the system down with
    // it; refusing up front costs one syscall.
    if let Some(available) = available_bytes(&dest_dir) {
        let needed = required_bytes(&game.files);
        if available < needed {
            let message = format!(
                "not enough room in {}: this needs about {}, and {} is free",
                dest_dir.display(),
                human_bytes(needed),
                human_bytes(available),
            );
            set_status(
                &app,
                &game.id,
                InstallStatus::Failed {
                    message: message.clone(),
                },
            )?;
            return Err(message);
        }
    }

    // Progress is reported against the whole title, not the file being
    // transferred at the moment. A PC game is a tree of many files of
    // wildly different sizes, so a per-file percentage would race to
    // 100% and reset over and over while telling you nothing about how
    // far along the install actually is.
    let total_bytes: u64 = game.files.iter().map(|f| f.size_bytes).sum();
    let mut progress = Progress {
        done_bytes: 0,
        total_bytes,
        last_pct: -1,
    };

    // Persisted once, so an install interrupted by the app closing is
    // still recognisable as one on next launch. Everything after this
    // is emitted without touching the state file; see emit_progress.
    set_status(
        &app,
        &game.id,
        InstallStatus::Downloading {
            file: game.files[0].filename.clone(),
            pct: 0,
        },
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

    set_status(
        &app,
        &game.id,
        InstallStatus::Installed {
            local_dir: dest_dir,
        },
    )?;
    // Now that this title is on disk, its catalog entry belongs in the
    // startup cache — it should be on the shelf at next launch whether
    // or not the library server answers.
    crate::catalog_cache::remember(&app, &game);
    Ok(())
}

/// Free space on the filesystem holding `dir`, or None if it can't be
/// determined — an unreadable path, or a platform without statvfs. A
/// None means the check is skipped, never that the install is refused:
/// this is a guard against a predictable failure, not a gate.
// The conversions below are genuinely redundant on 64-bit glibc, where
// both fields are already u64, and genuinely necessary elsewhere, where
// they are not. Clippy can only see this target, so it is told once
// rather than the code being narrowed to it.
#[cfg(unix)]
#[allow(clippy::useless_conversion)]
fn available_bytes(dir: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: `path` is a valid NUL-terminated string that outlives the
    // call, and `stat` is a zeroed statvfs of the right type. statvfs
    // writes only into it and returns a status we check.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        // f_bavail rather than f_bfree: the blocks available to an
        // ordinary user, excluding the reserve only root can spend.
        Some(u64::from(stat.f_bavail).saturating_mul(u64::from(stat.f_frsize)))
    }
}

#[cfg(not(unix))]
fn available_bytes(_dir: &Path) -> Option<u64> {
    None
}

/// Room this title needs, which is not simply the sum of the catalog's
/// sizes. A .nsz is a compressed .nsp and the server hands back the
/// decompressed file, so its installed size is larger than the number
/// the catalog carries — measured at roughly 1.5x across a sample of
/// real titles, doubled here because refusing an install that would
/// have just fit is a far smaller annoyance than filling the disk.
///
/// The margin on top covers the filesystem's own overhead and leaves
/// the machine somewhere to breathe afterwards.
fn required_bytes(files: &[GameFile]) -> u64 {
    const MARGIN: u64 = 256 * 1024 * 1024;
    files
        .iter()
        .map(|f| {
            if f.needs_conversion {
                f.size_bytes.saturating_mul(2)
            } else {
                f.size_bytes
            }
        })
        .fold(MARGIN, |acc, n| acc.saturating_add(n))
}

/// Sizes for people, not for arithmetic: two significant figures and
/// the unit they'd use themselves.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{:.1} {}", value, UNITS[unit])
    } else {
        format!("{:.0} {}", value, UNITS[unit])
    }
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

    // Captured before the body is consumed. Content-Length is what the
    // server says it is about to send, which is the only number that
    // can be checked against what actually arrives; a body that came
    // back encoded (compressed in transit) is decoded on the way in, so
    // the two legitimately differ and the check is skipped there.
    let declared_length = response.content_length();
    let transfer_encoded = response
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .is_some();

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
        fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let mut out = fs::File::create(&dest_path)
        .await
        .map_err(|e| e.to_string())?;

    // Emitted once per file regardless of whether the overall
    // percentage moved, so the name on screen keeps up while a long
    // tail of small files goes by.
    emit_progress(app, game_id, &file.filename, progress.pct());

    let mut stream = response.bytes_stream();
    let mut written: u64 = 0;

    while let Some(chunk) = stream.next().await {
        if cancelled_downloads().lock().unwrap().remove(game_id) {
            return Ok(true);
        }

        let chunk = chunk.map_err(|e| e.to_string())?;
        out.write_all(&chunk).await.map_err(|e| e.to_string())?;
        written += chunk.len() as u64;
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
    drop(out);

    // And now the other half of that: a stream can also end early
    // without erroring — a dropped connection, a server that died
    // mid-response — which writes a short file and reports success.
    // Left alone, that surfaces weeks later as an emulator crash
    // nobody connects back to this install.
    if let Err(message) = verify_transfer(
        &file.filename,
        written,
        declared_length,
        file.size_bytes,
        file.needs_conversion,
        transfer_encoded,
    ) {
        // The partial file is deleted rather than left in place: a
        // retry would otherwise have to guess whether what's on disk
        // is good, and a truncated game file is worse than none.
        let _ = fs::remove_file(&dest_path).await;
        return Err(message);
    }

    Ok(false)
}

/// Decides whether what landed on disk is what was promised.
///
/// Two independent promises, checked in order of authority:
///
/// 1. Content-Length, when the server sent one and the body wasn't
///    encoded in transit. This is exact and covers every file,
///    including one the server converted on the way out.
/// 2. Failing that, the size the catalog advertised — but only for a
///    file that wasn't converted. A .nsz decompresses into a larger
///    .nsp, so for those the catalog's number is legitimately not the
///    number that arrives, and comparing them would fail every Switch
///    install.
///
/// When neither applies (a chunked response for a converted file)
/// there is nothing honest left to compare, and the transfer is
/// accepted. Reporting a mismatch we cannot actually detect would be
/// worse than admitting the gap.
fn verify_transfer(
    filename: &str,
    written: u64,
    declared_length: Option<u64>,
    catalog_size: u64,
    converted: bool,
    transfer_encoded: bool,
) -> Result<(), String> {
    let expected = match declared_length {
        Some(length) if !transfer_encoded => Some(length),
        _ if !converted => Some(catalog_size),
        _ => None,
    };

    match expected {
        Some(expected) if written != expected => Err(format!(
            "{filename} arrived incomplete: expected {expected} bytes, got {written}"
        )),
        _ => Ok(()),
    }
}

#[tauri::command]
pub fn uninstall_game(app: AppHandle, game_id: String) -> Result<(), String> {
    let states = load_states(&app);
    if let Some(InstallStatus::Installed { local_dir }) = states.get(&game_id) {
        if local_dir.exists() {
            std::fs::remove_dir_all(local_dir).map_err(|e| e.to_string())?;
        }
    }
    crate::catalog_cache::forget(&app, &game_id);
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
pub fn cancel_install(app: AppHandle, game_id: String, install_root: String) -> Result<(), String> {
    cancelled_downloads()
        .lock()
        .unwrap()
        .insert(game_id.clone());

    if let Some(dir) = install_dir_for(&install_root, &game_id) {
        if dir.exists() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    crate::catalog_cache::forget(&app, &game_id);
    set_status(&app, &game_id, InstallStatus::NotInstalled)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(done: u64, total: u64) -> Progress {
        Progress {
            done_bytes: done,
            total_bytes: total,
            last_pct: -1,
        }
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
        assert_eq!(
            encode_path_segments("PC/Moth & Ember"),
            "PC/Moth%20%26%20Ember"
        );
        // A nested filename has to survive the same way, or the
        // server's route can't split it back apart.
        assert_eq!(
            encode_path_segments("bin/game data/run.exe"),
            "bin/game%20data/run.exe"
        );
    }

    #[test]
    fn error_detail_is_pulled_out_of_flasks_html_error_page() {
        let body = "<html><title>500</title><body><h1>Error</h1>\
                    <p>nsz conversion failed: bad header</p></body></html>";
        assert_eq!(
            extract_error_detail(body),
            "nsz conversion failed: bad header"
        );
        assert_eq!(extract_error_detail("   "), "no error detail returned");
    }

    fn file(size: u64, converted: bool) -> GameFile {
        GameFile {
            filename: "game.nsp".to_string(),
            format: if converted { "nsz" } else { "nsp" }.to_string(),
            needs_conversion: converted,
            size_bytes: size,
        }
    }

    #[test]
    fn required_room_allows_for_an_nsz_decompressing() {
        const GB: u64 = 1024 * 1024 * 1024;
        const MARGIN: u64 = 256 * 1024 * 1024;

        // A plain file needs its own size, plus the margin.
        assert_eq!(required_bytes(&[file(2 * GB, false)]), 2 * GB + MARGIN);
        // A compressed one needs room for what it becomes.
        assert_eq!(required_bytes(&[file(2 * GB, true)]), 4 * GB + MARGIN);
        // And a title of several files needs the lot.
        assert_eq!(
            required_bytes(&[file(GB, false), file(GB, true)]),
            3 * GB + MARGIN
        );
    }

    #[test]
    fn required_room_does_not_overflow_on_an_absurd_size() {
        // A corrupt catalog claiming a file of u64::MAX must saturate
        // rather than wrap around to a tiny number that would pass the
        // check it exists to fail.
        assert_eq!(required_bytes(&[file(u64::MAX, true)]), u64::MAX);
    }

    #[test]
    fn sizes_are_rendered_the_way_someone_would_say_them() {
        assert_eq!(human_bytes(512), "512 bytes");
        assert_eq!(
            human_bytes(1024 * 1024 * 1024 * 3 + 1024 * 1024 * 500),
            "3.5 GB"
        );
        assert_eq!(human_bytes(1024 * 1024 * 1024 * 42), "42 GB");
    }

    #[test]
    fn a_short_transfer_is_caught_by_content_length() {
        let err = verify_transfer("base.nsp", 900, Some(1000), 1000, false, false).unwrap_err();
        assert!(err.contains("incomplete"), "{err}");
        assert!(err.contains("expected 1000 bytes, got 900"), "{err}");
    }

    #[test]
    fn a_complete_transfer_passes() {
        assert!(verify_transfer("base.nsp", 1000, Some(1000), 1000, false, false).is_ok());
    }

    #[test]
    fn a_converted_file_is_not_measured_against_its_compressed_size() {
        // The whole point of .nsz is that what arrives is bigger than
        // what the catalog lists. Without Content-Length there is
        // nothing to check, and checking anyway would fail every
        // single Switch install.
        assert!(verify_transfer("base.nsp", 2500, None, 1000, true, false).is_ok());
        // With Content-Length there is, and it still applies.
        assert!(verify_transfer("base.nsp", 2500, Some(3000), 1000, true, false).is_err());
    }

    #[test]
    fn an_uncompressed_file_falls_back_to_the_catalog_size() {
        assert!(verify_transfer("game.exe", 400, None, 1000, false, false).is_err());
        assert!(verify_transfer("game.exe", 1000, None, 1000, false, false).is_ok());
    }

    #[test]
    fn an_encoded_body_is_not_measured_against_its_wire_length() {
        // Content-Length describes the compressed bytes on the wire,
        // not the decoded file, so comparing them would fail a
        // perfectly good transfer. The catalog size still applies.
        assert!(verify_transfer("game.exe", 1000, Some(300), 1000, false, true).is_ok());
        assert!(verify_transfer("game.exe", 900, Some(300), 1000, false, true).is_err());
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

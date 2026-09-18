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
        /// Transfer rate, smoothed; 0 before there is anything to
        /// measure. `default` so an installs.json written before this
        /// existed still loads — a persisted Downloading status is how
        /// an install interrupted by the app closing is recognised.
        #[serde(default)]
        bytes_per_sec: u64,
        /// Seconds remaining at the current rate, or None when that
        /// cannot honestly be said: no rate yet, or nothing left to
        /// predict against.
        #[serde(default)]
        eta_secs: Option<u64>,
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
fn emit_progress(app: &AppHandle, id: &str, file: &str, progress: &Progress) {
    let status = InstallStatus::Downloading {
        file: file.to_string(),
        pct: progress.pct(),
        bytes_per_sec: progress.rate.bytes_per_sec(),
        eta_secs: progress.rate.eta(progress.done_bytes, progress.total_bytes),
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
    let mut progress = Progress::new(total_bytes);

    // Persisted once, so an install interrupted by the app closing is
    // still recognisable as one on next launch. Everything after this
    // is emitted without touching the state file; see emit_progress.
    set_status(
        &app,
        &game.id,
        InstallStatus::Downloading {
            file: game.files[0].filename.clone(),
            pct: 0,
            bytes_per_sec: 0,
            eta_secs: None,
        },
    )?;

    // A title of many files is fetched in one request when there is
    // nothing on disk to resume from. Both conditions matter: the
    // archive is the fast path for a tree of thousands of small files,
    // and re-fetching all of them is exactly the wrong thing to do to
    // an install that was interrupted halfway.
    let resuming = has_partial_install(&dest_dir).await;
    if game.files.len() > ARCHIVE_THRESHOLD && !resuming {
        match download_archive(&app, &game.id, &server_base, &dest_dir, &mut progress).await {
            Ok(true) => {
                set_status(
                    &app,
                    &game.id,
                    InstallStatus::Installed {
                        local_dir: dest_dir.clone(),
                    },
                )?;
                crate::catalog_cache::remember(&app, &game);
                return Ok(());
            }
            // The server has no archive endpoint — an older one, or
            // something in front of it rewriting paths. Fall through
            // to the per-file path rather than failing an install over
            // an optimisation.
            Ok(false) => {}
            Err(e) if e == CANCELLED => return Ok(()),
            Err(e) => {
                set_status(&app, &game.id, InstallStatus::Failed { message: e.clone() })?;
                return Err(e);
            }
        }
        // Whatever the archive attempt counted toward progress is not
        // on disk, so the per-file path below starts from zero again.
        progress = Progress::new(total_bytes);
    }

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
    /// The rate, and what it was measured from. Bytes skipped by resume
    /// are deliberately not counted here — they arrived instantly, on a
    /// previous run, and folding them in would report a speed nobody's
    /// network is achieving.
    rate: Rate,
}

/// A smoothed transfer rate.
///
/// The instantaneous rate between two samples is far too noisy to put
/// on screen — chunk sizes vary, the disk flushes, the server pauses to
/// convert an .nsz — so each sample is blended into a running average.
/// The weight is a compromise between a number that jitters and one
/// that takes ten seconds to notice a stall.
struct Rate {
    /// Bytes actually transferred since the last sample.
    since_sample: u64,
    sampled_at: std::time::Instant,
    /// Smoothed bytes per second, or None until there is a sample.
    smoothed: Option<f64>,
    emitted_at: std::time::Instant,
}

impl Rate {
    /// How much of a new sample is taken; the rest is history.
    const WEIGHT: f64 = 0.35;
    /// A sample shorter than this measures scheduling noise rather than
    /// a transfer rate.
    const MIN_SAMPLE: std::time::Duration = std::time::Duration::from_millis(400);
    /// Speed is worth re-stating even when the percentage hasn't moved
    /// — on a large title a percent can take a minute, and a frozen
    /// number reads as a frozen download.
    const MIN_EMIT: std::time::Duration = std::time::Duration::from_millis(500);

    fn new() -> Self {
        let now = std::time::Instant::now();
        Self {
            since_sample: 0,
            sampled_at: now,
            smoothed: None,
            emitted_at: now,
        }
    }

    fn record(&mut self, bytes: u64) {
        self.since_sample += bytes;
    }

    /// Folds the bytes seen since the last sample into the average, if
    /// enough time has passed to make that meaningful.
    fn sample(&mut self, now: std::time::Instant) {
        let elapsed = now.duration_since(self.sampled_at);
        if elapsed < Self::MIN_SAMPLE {
            return;
        }
        let instant = self.since_sample as f64 / elapsed.as_secs_f64();
        self.smoothed = Some(match self.smoothed {
            Some(previous) => previous * (1.0 - Self::WEIGHT) + instant * Self::WEIGHT,
            None => instant,
        });
        self.since_sample = 0;
        self.sampled_at = now;
    }

    fn bytes_per_sec(&self) -> u64 {
        self.smoothed.unwrap_or(0.0).max(0.0) as u64
    }

    /// Seconds left at the current rate, or None when saying anything
    /// would be a guess: no rate measured yet, or a total that is
    /// already behind us (a .nsz decompressing past its listed size).
    fn eta(&self, done: u64, total: u64) -> Option<u64> {
        let rate = self.smoothed.filter(|r| *r > 1.0)?;
        let remaining = total.checked_sub(done).filter(|r| *r > 0)?;
        Some((remaining as f64 / rate).round() as u64)
    }
}

impl Progress {
    fn new(total_bytes: u64) -> Self {
        Self {
            done_bytes: 0,
            total_bytes,
            last_pct: -1,
            rate: Rate::new(),
        }
    }

    /// Whether the frontend should hear about this chunk: the
    /// percentage moved, or enough time has passed that the speed on
    /// screen is stale.
    fn should_emit(&mut self, now: std::time::Instant) -> bool {
        let pct = self.pct() as i16;
        if pct != self.last_pct || now.duration_since(self.rate.emitted_at) >= Rate::MIN_EMIT {
            self.last_pct = pct;
            self.rate.emitted_at = now;
            return true;
        }
        false
    }

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
/// Whether anything is already in this title's install directory.
///
/// An interrupted install leaves the files it had finished, and the
/// per-file path knows how to continue from them; the archive path
/// does not, and would happily re-fetch several gigabytes that are
/// already on disk.
async fn has_partial_install(dest_dir: &Path) -> bool {
    match fs::read_dir(dest_dir).await {
        Ok(mut entries) => matches!(entries.next_entry().await, Ok(Some(_))),
        Err(_) => false,
    }
}

/// Above this many files, a title is fetched as one archive rather
/// than one request per file. Below it the per-file path is no worse
/// and keeps the properties the archive path cannot offer: per-file
/// progress that names what is transferring, and resume.
///
/// Four is deliberately low. A Switch title is one to three files and
/// stays on the per-file path; a PC game is thousands and does not.
const ARCHIVE_THRESHOLD: usize = 4;

/// Fetches an entire title as one tar and unpacks it as it arrives.
///
/// A PC game is a tree of thousands of files, and installing one meant
/// thousands of HTTP requests — correct, and fine on a LAN, but each
/// one pays for a connection, a route lookup and a catalog hit, and
/// for a tree of small files that dwarfs the transfer itself.
///
/// Returns Ok(false) when the server has no archive endpoint (an older
/// server, or one behind something that rewrites the path), so the
/// caller can fall back to the per-file path rather than failing an
/// install over an optimisation.
async fn download_archive(
    app: &AppHandle,
    game_id: &str,
    server_base: &str,
    dest_dir: &Path,
    progress: &mut Progress,
) -> Result<bool, String> {
    let url = format!(
        "{}/archive/{}",
        server_base.trim_end_matches('/'),
        encode_path_segments(game_id),
    );

    let response = reqwest::get(&url).await.map_err(|e| e.to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false); // no such endpoint here; the caller has another way
    }
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "server returned {status} fetching the archive: {}",
            extract_error_detail(&body)
        ));
    }

    // Buffered rather than unpacked from the stream: tar's reader wants
    // a synchronous Read, and bridging an async byte stream into one
    // means a thread and a channel for no benefit here — the bytes are
    // going to disk in full either way, and this keeps cancellation
    // and progress in one obvious place.
    let mut stream = response.bytes_stream();
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        if cancelled_downloads().lock().unwrap().remove(game_id) {
            return Err(CANCELLED.to_string());
        }
        let chunk = chunk.map_err(|e| e.to_string())?;
        bytes.extend_from_slice(&chunk);
        progress.done_bytes += chunk.len() as u64;
        progress.rate.record(chunk.len() as u64);

        let now = std::time::Instant::now();
        progress.rate.sample(now);
        if progress.should_emit(now) {
            emit_progress(app, game_id, "downloading the whole title", progress);
        }
    }

    let dest = dest_dir.to_path_buf();
    tokio::task::spawn_blocking(move || unpack_archive(&bytes, &dest))
        .await
        .map_err(|e| format!("unpacking failed: {e}"))??;

    Ok(true)
}

/// A sentinel rather than a status: cancellation travels back through
/// the same Result as a real failure, and the caller has to be able to
/// tell them apart without treating a deliberate stop as an error.
const CANCELLED: &str = "__cancelled__";

/// Unpacks the title archive, refusing any entry that would land
/// outside the destination. tar permits absolute paths and `..`
/// components, and this archive came off the network, so unpacking it
/// blindly would let the server write anywhere the app can reach.
fn unpack_archive(bytes: &[u8], dest: &Path) -> Result<(), String> {
    let mut archive = tar::Archive::new(bytes);
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        if path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(format!(
                "the archive contains an unsafe path: {}",
                path.display()
            ));
        }
        let target = dest.join(&path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        entry.unpack(&target).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// What to do about a file that is already partly on disk.
#[derive(Debug, PartialEq)]
enum Resume {
    /// Already complete; don't ask the server for it at all.
    Skip,
    /// Ask for the rest of it from this offset.
    From(u64),
    /// Nothing usable on disk, or more than there should be; start over.
    Restart,
}

/// An interrupted install used to begin again at the first file, which
/// on a PC game of several thousand files meant re-downloading every
/// one of them because the last had failed. The state to avoid that is
/// already on disk — the only question is whether to trust it.
///
/// A file the size the catalog says it should be is complete. A shorter
/// one is worth continuing. A longer one is not a file this install
/// wrote, so it is replaced rather than appended to. Where the expected
/// size isn't known — a `.nsz` the server decompresses on the way out,
/// where the catalog's number describes the compressed original — a
/// partial file is still worth continuing, since the server's range
/// support answers what the catalog can't.
fn resume_plan(existing: Option<u64>, expected: Option<u64>) -> Resume {
    match (existing, expected) {
        (None, _) | (Some(0), _) => Resume::Restart,
        (Some(have), Some(want)) if have == want => Resume::Skip,
        (Some(have), Some(want)) if have > want => Resume::Restart,
        (Some(have), _) => Resume::From(have),
    }
}

async fn request_file(url: &str, from: u64) -> Result<reqwest::Response, String> {
    let request = reqwest::Client::new().get(url);
    let request = if from > 0 {
        request.header(reqwest::header::RANGE, format!("bytes={from}-"))
    } else {
        request
    };
    request.send().await.map_err(|e| e.to_string())
}

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

    // A converted file's final size is not the size the catalog carries
    // — that describes the compressed .nsz — so there is nothing to
    // compare against until the server tells us.
    let expected = if file.needs_conversion {
        None
    } else {
        Some(file.size_bytes)
    };
    let existing = fs::metadata(&dest_path).await.ok().map(|m| m.len());

    let mut from = match resume_plan(existing, expected) {
        Resume::Skip => {
            // Counted toward the title's progress all the same: the
            // bytes are there, and a resumed install that reported 0%
            // while skipping nine-tenths of its files would be lying.
            // Not recorded as transferred: these bytes arrived on a
            // previous run, and folding them into the rate would put a
            // speed on screen that nobody's network is achieving.
            progress.done_bytes += expected.unwrap_or(0);
            emit_progress(app, game_id, &file.filename, progress);
            return Ok(false);
        }
        Resume::From(offset) => offset,
        Resume::Restart => 0,
    };

    let mut response = request_file(&url, from).await?;

    // The server disagrees that there is anything left to send — the
    // file on disk is at least as long as the one it has. Ask for the
    // whole thing instead of guessing which of us is right.
    if from > 0 && response.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        from = 0;
        response = request_file(&url, 0).await?;
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "server returned {status} for {}: {}",
            file.filename,
            extract_error_detail(&body)
        ));
    }

    // A 200 to a range request means the server ignored it and is
    // sending the file from the beginning, so what's on disk has to go.
    let resuming = from > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let start = if resuming { from } else { 0 };

    // Captured before the body is consumed. Content-Length is what the
    // server says it is about to send, which is the only number that
    // can be checked against what actually arrives; on a 206 it
    // describes the remainder, so the total to expect is that plus
    // what was already here. A body that came back encoded (compressed
    // in transit) is decoded on the way in, so the two legitimately
    // differ and the check is skipped there.
    let declared_total = response.content_length().map(|len| len + start);
    let transfer_encoded = response
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .is_some();

    let mut out = if resuming {
        fs::OpenOptions::new()
            .append(true)
            .open(&dest_path)
            .await
            .map_err(|e| e.to_string())?
    } else {
        fs::File::create(&dest_path)
            .await
            .map_err(|e| e.to_string())?
    };

    // Emitted once per file regardless of whether the overall
    // percentage moved, so the name on screen keeps up while a long
    // tail of small files goes by.
    progress.done_bytes += start; // already on disk from an earlier run
    emit_progress(app, game_id, &file.filename, progress);

    let mut stream = response.bytes_stream();
    let mut written: u64 = start;

    while let Some(chunk) = stream.next().await {
        if cancelled_downloads().lock().unwrap().remove(game_id) {
            return Ok(true);
        }

        let chunk = chunk.map_err(|e| e.to_string())?;
        out.write_all(&chunk).await.map_err(|e| e.to_string())?;
        written += chunk.len() as u64;
        progress.done_bytes += chunk.len() as u64;
        progress.rate.record(chunk.len() as u64);

        let now = std::time::Instant::now();
        progress.rate.sample(now);
        if progress.should_emit(now) {
            emit_progress(app, game_id, &file.filename, progress);
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
        declared_total,
        file.size_bytes,
        file.needs_conversion,
        transfer_encoded,
    ) {
        // The partial file is deleted rather than left in place: what
        // is on disk disagrees with what the server said it was
        // sending, so resuming from it later would compound the
        // problem rather than fix it.
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
        let mut p = Progress::new(total);
        p.done_bytes = done;
        p
    }

    /// A Rate with a known smoothed value, for the arithmetic that
    /// doesn't need real elapsed time to test.
    fn rate_of(bytes_per_sec: f64) -> Rate {
        let mut rate = Rate::new();
        rate.smoothed = Some(bytes_per_sec);
        rate
    }

    #[test]
    fn a_rate_is_smoothed_rather_than_reported_raw() {
        use std::time::{Duration, Instant};

        // Two very different seconds: 10 MB then 1 MB. The number on
        // screen should move toward the slower one without leaping to
        // it, or a download over a busy link reads as a fault.
        let start = Instant::now();
        let mut rate = Rate::new();
        rate.sampled_at = start;

        rate.record(10_000_000);
        rate.sample(start + Duration::from_secs(1));
        assert_eq!(rate.bytes_per_sec(), 10_000_000);

        rate.record(1_000_000);
        rate.sample(start + Duration::from_secs(2));
        let after = rate.bytes_per_sec();
        assert!(
            after < 10_000_000 && after > 1_000_000,
            "expected something between the two, got {after}"
        );
    }

    #[test]
    fn a_sample_too_short_to_mean_anything_is_ignored() {
        use std::time::{Duration, Instant};

        let start = Instant::now();
        let mut rate = Rate::new();
        rate.sampled_at = start;
        rate.record(50_000);
        rate.sample(start + Duration::from_millis(20));
        assert_eq!(rate.bytes_per_sec(), 0, "20ms measures scheduling noise");
        // And the bytes are not lost — they count toward the next one.
        rate.sample(start + Duration::from_secs(1));
        assert_eq!(rate.bytes_per_sec(), 50_000);
    }

    #[test]
    fn an_eta_is_the_remainder_at_the_current_rate() {
        let rate = rate_of(2_000_000.0);
        assert_eq!(rate.eta(0, 10_000_000), Some(5));
        assert_eq!(rate.eta(8_000_000, 10_000_000), Some(1));
    }

    #[test]
    fn no_eta_is_offered_when_there_is_nothing_to_base_one_on() {
        // Nothing measured yet.
        assert_eq!(Rate::new().eta(0, 10_000_000), None);
        // Finished, or past a total the catalog under-predicted, which
        // is every .nsz: there is no remainder to divide.
        assert_eq!(rate_of(1_000.0).eta(10_000_000, 10_000_000), None);
        assert_eq!(rate_of(1_000.0).eta(12_000_000, 10_000_000), None);
        // A rate so low it would predict a wait measured in days says
        // nothing rather than something absurd.
        assert_eq!(rate_of(0.5).eta(0, 10_000_000), None);
    }

    #[test]
    fn progress_is_emitted_on_a_clock_as_well_as_on_a_percentage() {
        use std::time::{Duration, Instant};

        let start = Instant::now();
        let mut p = Progress::new(1_000_000);
        p.rate.emitted_at = start;

        assert!(p.should_emit(start), "the first frame always goes");
        assert!(
            !p.should_emit(start),
            "the same percentage a moment later does not"
        );
        // On a large title a single percent can take a minute, and a
        // speed frozen for that long reads as a stalled download.
        assert!(p.should_emit(start + Duration::from_millis(600)));
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
    fn an_archive_unpacks_into_the_destination() {
        let dir = std::env::temp_dir().join("gg-archive-unpack");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // A tar shaped like the server's: a nested tree, built in
        // memory so the test doesn't depend on the tar binary.
        let mut builder = tar::Builder::new(Vec::new());
        for (name, contents) in [
            ("bin/game.exe", &b"executable"[..]),
            ("data/pak01.vpk", &b"assets"[..]),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, contents).unwrap();
        }
        let bytes = builder.into_inner().unwrap();

        unpack_archive(&bytes, &dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("bin/game.exe")).unwrap(),
            "executable"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("data/pak01.vpk")).unwrap(),
            "assets"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_archive_that_climbs_out_of_the_destination_is_refused() {
        // Forged at the byte level. The tar crate refuses to *build* an
        // archive containing `..`, which is exactly why this check
        // matters: a hostile or buggy server is under no such
        // obligation, so the guard has to face a real archive the safe
        // API would never have produced.
        let dir = std::env::temp_dir().join("gg-archive-escape");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        for name in [&b"../escaped.txt"[..], &b"/etc/escaped.txt"[..]] {
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o644);
            header.set_cksum();
            header.as_old_mut().name[..name.len()].copy_from_slice(name);
            header.set_cksum(); // recomputed now the name is in place

            let mut raw = Vec::new();
            raw.extend_from_slice(header.as_bytes());
            let mut block = [0u8; 512];
            block[..4].copy_from_slice(b"evil");
            raw.extend_from_slice(&block);
            raw.extend_from_slice(&[0u8; 1024]); // two empty blocks end a tar

            let err = unpack_archive(&raw, &dir).unwrap_err();
            assert!(
                err.contains("unsafe path"),
                "{}: {err}",
                String::from_utf8_lossy(name)
            );
        }
        assert!(!dir.parent().unwrap().join("escaped.txt").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_complete_file_is_not_downloaded_again() {
        // The point of the whole thing: an install that failed on its
        // last file used to re-fetch the several thousand before it.
        assert_eq!(resume_plan(Some(1000), Some(1000)), Resume::Skip);
    }

    #[test]
    fn a_partial_file_is_continued() {
        assert_eq!(resume_plan(Some(400), Some(1000)), Resume::From(400));
    }

    #[test]
    fn a_file_longer_than_it_should_be_is_replaced() {
        // Not something this install wrote, so appending to it would
        // produce something stranger still.
        assert_eq!(resume_plan(Some(1400), Some(1000)), Resume::Restart);
    }

    #[test]
    fn nothing_on_disk_means_an_ordinary_download() {
        assert_eq!(resume_plan(None, Some(1000)), Resume::Restart);
        // A zero-length file is what File::create leaves behind when a
        // transfer died immediately; there is nothing to resume from.
        assert_eq!(resume_plan(Some(0), Some(1000)), Resume::Restart);
    }

    #[test]
    fn a_converted_file_is_resumed_on_the_servers_word_rather_than_the_catalogs() {
        // The catalog's size describes the .nsz, and what arrives is
        // the decompressed .nsp, so "is it complete?" cannot be
        // answered here — but "is there something to continue?" can.
        assert_eq!(resume_plan(Some(2500), None), Resume::From(2500));
        assert_eq!(resume_plan(None, None), Resume::Restart);
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

// Cloud saves: archive a game's save directory, push it to the Python
// server, and pull it back on another machine.
//
// The hard part of this feature is not the transfer, it's knowing which
// directory on disk belongs to which game. That answer is different per
// platform and lives in save_sources() below:
//
//   Switch — every save sits under the emulator's own data directory in
//     a fixed tree, keyed by the title's 16-hex-digit Title ID rather
//     than by anything resembling the folder name the library uses. The
//     id has to be mapped once per game; settings.rs holds that map and
//     list_switch_title_ids() below lists what's actually present so the
//     UI can offer real options rather than asking someone to type hex.
//
//   PC — each title gets its own Wine prefix (see launcher.rs), so the
//     prefix's user directory *is* that game's save state. No per-game
//     path configuration and no manifest of where games hide saves,
//     which is the whole reason for preferring a prefix per game.
//
// Archives are tar.gz, with paths stored relative to a root that
// restoring puts them back under — so an archive is positionally
// meaningful rather than a bare bag of files, and a save made on one
// machine lands in the right place on another whose emulator directory
// differs.

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

use crate::install_state::encode_path_segments;
use crate::settings::get_settings;

/// One stored version, as the server reports it.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SaveVersion {
    pub version: String,
    pub uploaded_at: String,
    pub size_bytes: u64,
    pub sha256: String,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub saved_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    /// No save here and none on the server — nothing to do.
    None,
    /// Only here. The server has never seen this game.
    LocalOnly,
    /// Only on the server: a fresh machine, or a game just installed.
    RemoteOnly,
    /// Both exist and agree.
    InSync,
    /// Both exist, and the local one is the newer of the two.
    LocalNewer,
    /// Both exist, and the server's is newer.
    RemoteNewer,
}

#[derive(Serialize, Clone, Debug)]
pub struct SaveStatus {
    pub state: SyncState,
    /// Local modification time, seconds since the epoch, or 0 for none.
    pub local_modified: u64,
    pub local_bytes: u64,
    pub latest: Option<SaveVersion>,
    /// Set when the save location isn't configured or doesn't exist —
    /// the UI shows this instead of a sync control, since there is
    /// nothing actionable until it's resolved.
    pub unavailable: Option<String>,
}

/// Where a game's saves live, and the root that paths inside the
/// archive are relative to.
///
/// `root` is the directory an archive's entries are written relative to
/// and extracted back into; `subpaths` are the parts of it that
/// actually belong to this game. Keeping the two separate is what lets
/// a Switch archive carry `nand/user/save/<...>/<title id>/...` — a
/// path meaningful on any machine — rather than an absolute path that
/// only means something on the machine that made it.
struct SaveSource {
    root: PathBuf,
    subpaths: Vec<PathBuf>,
}

fn save_sources(app: &AppHandle, game_id: &str, platform: &str) -> Result<SaveSource, String> {
    let settings = get_settings(app.clone());
    let cfg = &settings.save_sync;

    match platform {
        "Switch" => {
            if cfg.switch_data_dir.trim().is_empty() {
                return Err("the Switch emulator's data directory isn't set in Settings".into());
            }
            let known = cfg.title_ids.get(game_id).cloned().unwrap_or_default();
            let title_id = if known.trim().is_empty() {
                // Nothing recorded yet, so work it out from the game's
                // own installed files and remember it. This is why
                // there is no per-game configuration to fill in: the
                // first time a title needs its id, it gets one.
                let detected = crate::install_state::install_dir_for(&settings.install_root, game_id)
                    .and_then(|dir| detect_switch_title_id(dir.to_string_lossy().to_string()))
                    .ok_or_else(|| {
                        "couldn't work out this game's Title ID from its files — \
                         play it once and it will be identified automatically"
                            .to_string()
                    })?;
                remember_title_id(app, game_id, &detected);
                detected
            } else {
                known
            };

            let root = PathBuf::from(&cfg.switch_data_dir);
            let subpaths = find_switch_save_dirs(&root, &title_id);
            if subpaths.is_empty() {
                return Err(format!("no save directory found for Title ID {title_id}"));
            }
            Ok(SaveSource { root, subpaths })
        }
        "PC" => {
            let prefix = crate::launcher::prefix_dir(&settings, platform, game_id)
                .ok_or_else(|| "this game has no Wine prefix yet".to_string())?;
            // Only the Windows user profile, not the whole prefix: the
            // rest is a generated Windows install that every machine
            // rebuilds for itself and that would dwarf the save.
            let users = PathBuf::from("drive_c").join("users");
            if !prefix.join(&users).is_dir() {
                return Err("this game's Wine prefix has no user directory yet — run it once".into());
            }
            Ok(SaveSource { root: prefix, subpaths: vec![users] })
        }
        _ => Err(format!("cloud saves aren't supported for {platform}")),
    }
}

/// Records a detected Title ID so the work isn't repeated, and so the
/// UI can report how many titles are identified. Best-effort: failing
/// to persist it costs a re-detection next time, not the sync.
fn remember_title_id(app: &AppHandle, game_id: &str, title_id: &str) {
    let mut settings = get_settings(app.clone());
    settings
        .save_sync
        .title_ids
        .insert(game_id.to_string(), title_id.to_string());
    let _ = crate::settings::save_settings(app.clone(), settings);
}

/// Called by the frontend once it has worked out an id by watching what
/// a play session created — the fallback for a title whose files gave
/// nothing away.
#[tauri::command]
pub fn set_switch_title_id(app: AppHandle, game_id: String, title_id: String) {
    remember_title_id(&app, &game_id, &title_id);
}

/// Every directory under the emulator's save tree whose own name is the
/// Title ID. Searched rather than composed from a fixed path because
/// the layout carries an account UUID that differs per machine, and
/// because a single title can have a directory per emulator user.
fn find_switch_save_dirs(data_dir: &Path, title_id: &str) -> Vec<PathBuf> {
    let base = data_dir.join("nand").join("user").join("save");
    let mut found = Vec::new();
    // Bounded depth: the real layout is save/<save id>/<user id>/<title
    // id>, so four levels is enough to find it and shallow enough not to
    // walk an entire emulator data directory if the path is wrong.
    collect_named_dirs(&base, title_id, 4, &mut found);
    found.sort();
    found
        .into_iter()
        .filter_map(|p| p.strip_prefix(data_dir).ok().map(Path::to_path_buf))
        .collect()
}

fn collect_named_dirs(dir: &Path, name: &str, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            out.push(path);
        } else {
            collect_named_dirs(&path, name, depth - 1, out);
        }
    }
}

/// Works out a game's Title ID from the files it installed, so nobody
/// has to look one up or pick it out of a list of near-identical hex
/// strings.
///
/// Two sources, cheapest first:
///
///   1. The filename. Dump tools overwhelmingly name Switch files with
///      the id in brackets — `Bad North [0100C1F0051B4000][v0].nsp` —
///      and reading it costs nothing.
///
///   2. The ticket inside the NSP. An NSP is a PFS0 archive, whose
///      header and filename table are plain, unencrypted bytes. A
///      ticket is named after its rights ID, whose first 16 hex digits
///      *are* the Title ID. So the id can be read without a key file
///      and without decrypting anything — only the archive's table of
///      contents is touched, never its content.
///
/// Whatever turns up is normalised to the base title (see
/// `base_title_id`), because that is what saves are filed under.
#[tauri::command]
pub fn detect_switch_title_id(install_dir: String) -> Option<String> {
    let dir = Path::new(&install_dir);
    let mut found: Vec<u64> = Vec::new();

    let Ok(entries) = fs::read_dir(dir) else {
        return None;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        found.extend(title_ids_in_text(&name));
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("nsp")) {
            found.extend(title_ids_from_nsp(&path));
        }
    }

    // A folder usually holds a base game plus its update and any DLC.
    // Normalised, base and update collapse onto the same value while
    // DLC sits above it, so the lowest is the base — which is the one
    // the emulator files saves under.
    found.iter().map(|id| base_title_id(*id)).min().map(|id| format!("{id:016X}"))
}

/// Updates share their base game's save data and differ only in the
/// low 12 bits, so masking those off turns an update's id into the
/// base id that saves are actually keyed by. DLC ids sit further out
/// and survive this untouched, which is what lets the caller tell them
/// apart by taking the lowest value.
fn base_title_id(id: u64) -> u64 {
    id & !0xFFF
}

/// Every 16-hex-digit run in a string, as numbers. Deliberately loose
/// about delimiters — brackets, parentheses, underscores and bare runs
/// all appear in the wild — but anchored on length, so a longer hash
/// isn't chopped into a false positive.
fn title_ids_in_text(text: &str) -> Vec<u64> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_hexdigit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i].is_ascii_hexdigit() {
            i += 1;
        }
        // Exactly 16, not "at least": a 32-character content id would
        // otherwise yield a bogus id from its first half.
        if i - start == 16 {
            let text: String = chars[start..i].iter().collect();
            if let Ok(id) = u64::from_str_radix(&text, 16) {
                // 01.. is the program-id prefix every retail title
                // uses; anything else is some other kind of hash that
                // happens to be the right length.
                if text.starts_with("01") {
                    out.push(id);
                }
            }
        }
    }
    out
}

/// Reads the filename table out of a PFS0 archive and returns the
/// Title IDs implied by any tickets in it.
///
/// Only the header, entry table and string table are read — a few
/// kilobytes off the front of the file, never the content itself, so
/// this stays cheap on a multi-gigabyte NSP and needs no keys.
fn title_ids_from_nsp(path: &Path) -> Vec<u64> {
    let Ok(names) = pfs0_entry_names(path) else {
        return Vec::new();
    };
    names
        .iter()
        .filter(|n| n.to_ascii_lowercase().ends_with(".tik"))
        // A ticket is named for its rights ID: 32 hex digits, of which
        // the first 16 are the Title ID.
        .filter_map(|n| n.get(..16).and_then(|id| u64::from_str_radix(id, 16).ok()))
        .collect()
}

fn pfs0_entry_names(path: &Path) -> Result<Vec<String>, String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut header = [0u8; 16];
    file.read_exact(&mut header).map_err(|e| e.to_string())?;
    if &header[..4] != b"PFS0" {
        return Err("not a PFS0 archive".into());
    }

    let count = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let string_table_size = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
    // Guard rails against a corrupt or hostile header asking for a
    // gigabyte of allocation before anything has been validated.
    if count > 4096 || string_table_size > 1 << 20 {
        return Err("implausible PFS0 header".into());
    }

    let mut entries = vec![0u8; count * 24];
    file.read_exact(&mut entries).map_err(|e| e.to_string())?;
    let mut strings = vec![0u8; string_table_size];
    file.read_exact(&mut strings).map_err(|e| e.to_string())?;

    let mut names = Vec::with_capacity(count);
    for i in 0..count {
        // Each 0x18 entry is offset, size, then the name's position in
        // the string table at 0x10.
        let at = i * 24 + 16;
        let name_offset = u32::from_le_bytes(entries[at..at + 4].try_into().unwrap()) as usize;
        if name_offset >= strings.len() {
            continue;
        }
        let end = strings[name_offset..]
            .iter()
            .position(|b| *b == 0)
            .map(|p| name_offset + p)
            .unwrap_or(strings.len());
        names.push(String::from_utf8_lossy(&strings[name_offset..end]).to_string());
    }
    let _ = file.seek(SeekFrom::Start(0));
    Ok(names)
}

/// Lists the Title IDs that actually have save data. Used to work out
/// which one a game created by comparing before and after a session —
/// see the frontend's session-based detection — and as a last-resort
/// manual list.
#[tauri::command]
pub fn list_switch_title_ids(switch_data_dir: String) -> Vec<String> {
    let base = Path::new(&switch_data_dir).join("nand").join("user").join("save");
    let mut candidates = Vec::new();
    collect_hex16_dirs(&base, 4, &mut candidates);

    // Matching on "16 hex digits" alone is not enough: the save-data
    // space above the account directory is named 0000000000000000,
    // which fits that description exactly and is emphatically not a
    // title. What separates them is position — a real Title ID has no
    // further Title ID beneath it, whereas the structural directory
    // has every one of them beneath it. Testing that relationship
    // rather than the depth keeps this working if a fork of the
    // emulator adds or removes a level.
    let deepest: Vec<PathBuf> = candidates
        .iter()
        .filter(|c| !candidates.iter().any(|other| other != *c && other.starts_with(c)))
        .cloned()
        .collect();

    let mut ids: Vec<String> = deepest
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Finds the emulator's data directory in the places these emulators
/// actually put it, so the field arrives filled in rather than as a
/// path someone has to go and look up. Eden is a Yuzu fork and the
/// family has used several names, so each is tried; a directory only
/// counts if it actually contains the save tree, which rules out a
/// leftover empty folder from an emulator that was removed.
#[tauri::command]
pub fn detect_switch_data_dir() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let home = Path::new(&home);

    let mut roots = vec![
        home.join(".local/share"),
        home.join(".var/app/dev.eden_emu.eden/data"), // Flatpak Eden
        home.join(".var/app/org.yuzu_emu.yuzu/data"), // Flatpak Yuzu
    ];
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        roots.insert(0, PathBuf::from(xdg));
    }

    for root in roots {
        for name in ["eden", "yuzu", "sudachi", "citron", "suyu"] {
            let candidate = root.join(name);
            if candidate.join("nand").join("user").join("save").is_dir() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
    }
    None
}

fn is_hex16(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.len() == 16 && n.chars().all(|c| c.is_ascii_hexdigit()))
}

fn collect_hex16_dirs(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if is_hex16(&path) {
            out.push(path.clone());
        }
        collect_hex16_dirs(&path, depth - 1, out);
    }
}

fn newest_mtime(root: &Path, subpaths: &[PathBuf]) -> (u64, u64) {
    let mut newest = 0u64;
    let mut bytes = 0u64;
    for sub in subpaths {
        let mut stack = vec![root.join(sub)];
        while let Some(path) = stack.pop() {
            let Ok(meta) = fs::metadata(&path) else { continue };
            if meta.is_dir() {
                if let Ok(entries) = fs::read_dir(&path) {
                    stack.extend(entries.flatten().map(|e| e.path()));
                }
                continue;
            }
            bytes += meta.len();
            if let Ok(modified) = meta.modified() {
                if let Ok(since) = modified.duration_since(UNIX_EPOCH) {
                    newest = newest.max(since.as_secs());
                }
            }
        }
    }
    (newest, bytes)
}

fn saves_url(server_base: &str, game_id: &str) -> String {
    format!(
        "{}/saves/{}",
        server_base.trim_end_matches('/'),
        encode_path_segments(game_id)
    )
}

async fn fetch_versions(server_base: &str, game_id: &str) -> Result<Vec<SaveVersion>, String> {
    #[derive(Deserialize)]
    struct Listing {
        versions: Vec<SaveVersion>,
    }
    let response = reqwest::get(saves_url(server_base, game_id))
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("server returned {}", response.status()));
    }
    Ok(response.json::<Listing>().await.map_err(|e| e.to_string())?.versions)
}

/// What the UI needs to decide whether to offer an upload, a download,
/// or a choice between the two.
#[tauri::command]
pub async fn save_status(
    app: AppHandle,
    game_id: String,
    platform: String,
    server_base: String,
) -> SaveStatus {
    let unavailable = |why: String| SaveStatus {
        state: SyncState::None,
        local_modified: 0,
        local_bytes: 0,
        latest: None,
        unavailable: Some(why),
    };

    let source = match save_sources(&app, &game_id, &platform) {
        Ok(s) => s,
        Err(e) => return unavailable(e),
    };

    let (local_modified, local_bytes) = newest_mtime(&source.root, &source.subpaths);
    let latest = fetch_versions(&server_base, &game_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .next();

    let has_local = local_bytes > 0;
    let state = match (has_local, &latest) {
        (false, None) => SyncState::None,
        (true, None) => SyncState::LocalOnly,
        (false, Some(_)) => SyncState::RemoteOnly,
        (true, Some(version)) => {
            // saved_at is this same local-mtime value as recorded by
            // whichever machine uploaded it, so comparing the two is
            // comparing like with like — no clock-skew guessing between
            // a file time here and a server timestamp there.
            let remote_saved = version.saved_at.parse::<u64>().unwrap_or(0);
            if remote_saved == local_modified {
                SyncState::InSync
            } else if local_modified > remote_saved {
                SyncState::LocalNewer
            } else {
                SyncState::RemoteNewer
            }
        }
    };

    SaveStatus { state, local_modified, local_bytes, latest, unavailable: None }
}

#[tauri::command]
pub async fn upload_save(
    app: AppHandle,
    game_id: String,
    platform: String,
    server_base: String,
) -> Result<SaveVersion, String> {
    let source = save_sources(&app, &game_id, &platform)?;
    let (local_modified, local_bytes) = newest_mtime(&source.root, &source.subpaths);
    if local_bytes == 0 {
        return Err("there is no save data here to upload".into());
    }

    let archive = build_archive(&source)?;
    let device = get_settings(app).save_sync.device_name;

    let url = format!(
        "{}?device={}&saved_at={}",
        saves_url(&server_base, &game_id),
        urlencoding::encode(&device),
        local_modified
    );
    let response = reqwest::Client::new()
        .post(url)
        .body(archive)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("server returned {} storing the save", response.status()));
    }
    response.json::<SaveVersion>().await.map_err(|e| e.to_string())
}

/// Replaces local save data with a stored version. The existing local
/// data is moved aside first rather than deleted, because this is the
/// one operation here that can destroy a playthrough, and an overwrite
/// someone did not mean is exactly the case the backup is for.
#[tauri::command]
pub async fn download_save(
    app: AppHandle,
    game_id: String,
    platform: String,
    server_base: String,
    version: Option<String>,
) -> Result<(), String> {
    let source = save_sources(&app, &game_id, &platform)?;

    let version = match version {
        Some(v) if !v.is_empty() => v,
        _ => fetch_versions(&server_base, &game_id)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| "the server has no save for this game".to_string())?
            .version,
    };

    let url = format!(
        "{}/{}",
        saves_url(&server_base, &game_id),
        urlencoding::encode(&version)
    );
    let response = reqwest::get(url).await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("server returned {} fetching the save", response.status()));
    }
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;

    back_up_existing(&source)?;
    extract_archive(&bytes, &source.root)
}

fn build_archive(source: &SaveSource) -> Result<Vec<u8>, String> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    for sub in &source.subpaths {
        let full = source.root.join(sub);
        if !full.is_dir() {
            continue;
        }
        builder
            .append_dir_all(sub, &full)
            .map_err(|e| format!("archiving {}: {e}", full.display()))?;
    }
    builder
        .into_inner()
        .map_err(|e| e.to_string())?
        .finish()
        .map_err(|e| e.to_string())
}

/// Whether an archive entry would escape the directory it's unpacked
/// into. Split out from extract_archive because it's the part worth
/// testing on its own: the tar crate refuses to *write* a path like
/// this, so the only way to exercise the check end to end is to forge
/// an archive byte by byte.
fn is_unsafe_entry(path: &Path) -> bool {
    path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir))
}

/// Refuses any entry that would land outside the destination. tar
/// permits absolute paths and `..` components, and this archive came
/// off the network, so unpacking it blindly would let the server write
/// anywhere the app can reach.
fn extract_archive(bytes: &[u8], dest: &Path) -> Result<(), String> {
    let mut archive = tar::Archive::new(GzDecoder::new(bytes));
    let entries = archive.entries().map_err(|e| e.to_string())?;
    for entry in entries {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        if is_unsafe_entry(&path) {
            return Err(format!("archive contains an unsafe path: {}", path.display()));
        }
        // Per-entry unpack, unlike Archive::unpack, does not create the
        // directories leading up to an entry — so a nested file in an
        // archive whose parent dirs happen to come later, or not at all,
        // fails outright without this.
        let target = dest.join(&path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        entry
            .unpack(&target)
            .map_err(|e| format!("extracting {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Moves current save data to a sibling `.bak-<timestamp>` directory.
/// Kept local and kept forever: it costs kilobytes, and the moment it
/// matters is the moment someone restored the wrong version.
fn back_up_existing(source: &SaveSource) -> Result<(), String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for sub in &source.subpaths {
        let full = source.root.join(sub);
        if !full.is_dir() {
            continue;
        }
        let mut backup = full.clone();
        backup.as_mut_os_string().push(format!(".bak-{stamp}"));
        fs::rename(&full, &backup)
            .map_err(|e| format!("backing up {}: {e}", full.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(name: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gg-saves-test-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    /// A Switch data directory as the emulator lays one out.
    fn switch_fixture(title_id: &str) -> PathBuf {
        let data = scratch("switch");
        let save = data
            .join("nand/user/save/0000000000000000/7b1c2f9e4a5d6c8b3e0f1a2b3c4d5e6f")
            .join(title_id);
        write(&save.join("progress.dat"), "level 4");
        write(&save.join("options/controls.cfg"), "invert=y");
        // A second title's data, which must never be swept in.
        write(
            &data
                .join("nand/user/save/0000000000000000/7b1c2f9e4a5d6c8b3e0f1a2b3c4d5e6f/0100000000000FFF")
                .join("other.dat"),
            "not mine",
        );
        data
    }

    #[test]
    fn switch_save_dirs_are_found_under_the_account_uuid() {
        let data = switch_fixture("0100AAA000BBB000");
        let found = find_switch_save_dirs(&data, "0100AAA000BBB000");
        assert_eq!(
            found,
            vec![PathBuf::from(
                "nand/user/save/0000000000000000/7b1c2f9e4a5d6c8b3e0f1a2b3c4d5e6f/0100AAA000BBB000"
            )]
        );
        fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn an_unmapped_title_id_finds_nothing() {
        let data = switch_fixture("0100AAA000BBB000");
        assert!(find_switch_save_dirs(&data, "0100CCC000DDD000").is_empty());
        fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn title_ids_are_listed_for_the_mapping_ui() {
        let data = switch_fixture("0100AAA000BBB000");
        let ids = list_switch_title_ids(data.to_string_lossy().to_string());
        // Both real titles, and specifically NOT "0000000000000000" —
        // the save-data space directory sitting above them, which is
        // itself 16 hex digits and would pass a name-only filter.
        assert_eq!(ids, vec!["0100000000000FFF", "0100AAA000BBB000"]);
        fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn an_absolute_or_climbing_entry_is_recognised_as_unsafe() {
        assert!(is_unsafe_entry(Path::new("../../escaped.txt")));
        assert!(is_unsafe_entry(Path::new("nand/../../escaped.txt")));
        assert!(is_unsafe_entry(Path::new("/etc/passwd")));
        assert!(!is_unsafe_entry(Path::new("nand/user/save/x/progress.dat")));
    }

    #[test]
    fn an_archive_round_trips_into_a_different_root() {
        let data = switch_fixture("0100AAA000BBB000");
        let subpaths = find_switch_save_dirs(&data, "0100AAA000BBB000");
        let source = SaveSource { root: data.clone(), subpaths: subpaths.clone() };

        let archive = build_archive(&source).unwrap();

        // Restoring on a "different machine": a fresh root entirely.
        let other = scratch("restore");
        extract_archive(&archive, &other).unwrap();

        let restored = other.join(&subpaths[0]);
        assert_eq!(fs::read_to_string(restored.join("progress.dat")).unwrap(), "level 4");
        assert_eq!(
            fs::read_to_string(restored.join("options/controls.cfg")).unwrap(),
            "invert=y"
        );
        // The other title was never part of this game's save.
        assert!(!other
            .join("nand/user/save/0000000000000000/7b1c2f9e4a5d6c8b3e0f1a2b3c4d5e6f/0100000000000FFF")
            .exists());

        fs::remove_dir_all(&data).unwrap();
        fs::remove_dir_all(&other).unwrap();
    }

    #[test]
    fn an_archive_escaping_its_destination_is_refused() {
        // Forged at the byte level. The tar crate refuses to build an
        // archive containing `..`, which is exactly why the check in
        // extract_archive matters: a hostile or buggy server is under
        // no such obligation, so the guard has to face a real archive
        // the safe API would never have produced.
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o644);
        header.set_cksum();
        let name = b"../../escaped.txt";
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_cksum(); // recomputed now the name is in place

        let mut raw = Vec::new();
        raw.extend_from_slice(header.as_bytes());
        let mut block = [0u8; 512];
        block[..3].copy_from_slice(b"bad");
        raw.extend_from_slice(&block);
        raw.extend_from_slice(&[0u8; 1024]); // two empty blocks end a tar

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        std::io::Write::write_all(&mut encoder, &raw).unwrap();
        let archive = encoder.finish().unwrap();

        let dest = scratch("dest");
        let err = extract_archive(&archive, &dest).unwrap_err();
        assert!(err.contains("unsafe path"), "unexpected error: {err}");
        assert!(!dest.parent().unwrap().join("escaped.txt").exists());
        fs::remove_dir_all(&dest).unwrap();
    }

    #[test]
    fn restoring_moves_the_existing_save_aside_rather_than_deleting_it() {
        let data = switch_fixture("0100AAA000BBB000");
        let subpaths = find_switch_save_dirs(&data, "0100AAA000BBB000");
        let source = SaveSource { root: data.clone(), subpaths: subpaths.clone() };

        back_up_existing(&source).unwrap();

        assert!(!data.join(&subpaths[0]).exists(), "original should have moved");
        let parent = data.join(subpaths[0].parent().unwrap());
        let backups: Vec<_> = fs::read_dir(&parent)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".bak-"))
            .collect();
        assert_eq!(backups.len(), 1, "expected one backup, got {backups:?}");
        // The data itself survived the move.
        assert_eq!(
            fs::read_to_string(parent.join(&backups[0]).join("progress.dat")).unwrap(),
            "level 4"
        );
        fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn size_and_mtime_cover_only_this_games_subpaths() {
        let data = switch_fixture("0100AAA000BBB000");
        let subpaths = find_switch_save_dirs(&data, "0100AAA000BBB000");
        let (modified, bytes) = newest_mtime(&data, &subpaths);
        // "level 4" + "invert=y" — the other title's file is excluded.
        assert_eq!(bytes, 15);
        assert!(modified > 0, "should have found a modification time");
        fs::remove_dir_all(&data).unwrap();
    }

    /// Builds a real PFS0 archive with the given entry names, per the
    /// format: magic, count, string-table size, padding, then a 0x18
    /// entry each, then the NUL-separated names, then the data.
    fn pfs0(names: &[&str]) -> Vec<u8> {
        let mut strings = Vec::new();
        let mut offsets = Vec::new();
        for name in names {
            offsets.push(strings.len() as u32);
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
        }

        let mut out = Vec::new();
        out.extend_from_slice(b"PFS0");
        out.extend_from_slice(&(names.len() as u32).to_le_bytes());
        out.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        for (i, offset) in offsets.iter().enumerate() {
            out.extend_from_slice(&(i as u64).to_le_bytes()); // data offset
            out.extend_from_slice(&1u64.to_le_bytes());       // size
            out.extend_from_slice(&offset.to_le_bytes());     // name position
            out.extend_from_slice(&0u32.to_le_bytes());       // reserved
        }
        out.extend_from_slice(&strings);
        out.extend_from_slice(&vec![0u8; names.len()]); // the content itself
        out
    }

    #[test]
    fn a_title_id_is_read_from_the_filename() {
        let dir = scratch("named");
        fs::write(dir.join("Bad North [0100C1F0051B4000][v0].nsp"), b"not a real nsp").unwrap();
        assert_eq!(
            detect_switch_title_id(dir.to_string_lossy().to_string()),
            Some("0100C1F0051B4000".to_string())
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_title_id_is_read_from_the_tickets_inside_an_nsp() {
        let dir = scratch("ticket");
        // No id in the name at all, so the archive is the only source.
        fs::write(
            dir.join("game.nsp"),
            pfs0(&[
                "0100c1f0051b4000000000000000000b.tik",
                "0100c1f0051b4000000000000000000b.cert",
                "a7f3c9d2e1b04856f0a1b2c3d4e5f607.nca",
            ]),
        )
        .unwrap();
        assert_eq!(
            detect_switch_title_id(dir.to_string_lossy().to_string()),
            Some("0100C1F0051B4000".to_string())
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_update_resolves_to_the_base_game_saves_are_filed_under() {
        let dir = scratch("update");
        fs::write(dir.join("Dorfromantik [0100C1F0051B4800][v65536].nsp"), b"x").unwrap();
        assert_eq!(
            detect_switch_title_id(dir.to_string_lossy().to_string()),
            Some("0100C1F0051B4000".to_string())
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_of_base_update_and_dlc_resolves_to_the_base() {
        let dir = scratch("bundle");
        fs::write(dir.join("Inmost [0100C1F0051B4000].nsp"), b"x").unwrap();
        fs::write(dir.join("Inmost update [0100C1F0051B4800].nsp"), b"x").unwrap();
        fs::write(dir.join("Inmost DLC [0100C1F0051B5000].nsp"), b"x").unwrap();
        assert_eq!(
            detect_switch_title_id(dir.to_string_lossy().to_string()),
            Some("0100C1F0051B4000".to_string())
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_content_hash_is_not_mistaken_for_a_title_id() {
        // 32 hex digits, whose first half looks exactly like an id.
        assert!(title_ids_in_text("0100c1f0051b4000a1b2c3d4e5f60718.nca").is_empty());
        // And a run of the right length that isn't a program id.
        assert!(title_ids_in_text("deadbeefdeadbeef.nsp").is_empty());
    }

    #[test]
    fn a_folder_with_nothing_identifying_yields_nothing() {
        let dir = scratch("anonymous");
        fs::write(dir.join("Moonscars.nsp"), b"definitely not a pfs0").unwrap();
        assert_eq!(detect_switch_title_id(dir.to_string_lossy().to_string()), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_corrupt_archive_header_is_refused_rather_than_trusted() {
        let dir = scratch("corrupt");
        let mut bad = Vec::from(*b"PFS0");
        bad.extend_from_slice(&u32::MAX.to_le_bytes()); // absurd file count
        bad.extend_from_slice(&u32::MAX.to_le_bytes()); // absurd string table
        bad.extend_from_slice(&0u32.to_le_bytes());
        fs::write(dir.join("evil.nsp"), bad).unwrap();
        // No panic, no vast allocation, just no answer.
        assert_eq!(detect_switch_title_id(dir.to_string_lossy().to_string()), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_urls_encode_the_id_without_losing_its_separator() {
        assert_eq!(
            saves_url("http://host:8420/", "Switch/198X"),
            "http://host:8420/saves/Switch/198X"
        );
        assert_eq!(
            saves_url("http://host:8420", "PC/Moth & Ember"),
            "http://host:8420/saves/PC/Moth%20%26%20Ember"
        );
    }
}

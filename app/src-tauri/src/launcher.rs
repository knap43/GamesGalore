// Launches an installed game with the emulator configured for its
// platform, in fullscreen. Command and arguments come from settings
// (settings.rs) rather than being hardcoded here — at least PCSX2 and
// Eden are likely to be Flatpaks, and there's no single "the emulator
// lives here" assumption that holds across native installs and
// Flatpak installs alike.
//
// PC has a further choice, Wine or Proton, which is configuration for
// the same reason. The three places the two differ — the environment a
// prefix wants, how an empty prefix is made, and what it means for a
// session to have ended — are collected around `Runtime` below.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

use crate::settings::{get_settings, EmulatorConfig, Settings};

/// The part of the launch command that's fixed per platform —
/// fullscreen flags and where the game path goes — independent of
/// *how* the emulator itself gets invoked. This is genuinely fixed
/// (DuckStation's `-fullscreen -batch --` isn't going to change based
/// on whether it's native or Flatpak-wrapped), unlike the command and
/// prefix args, which are user configuration.
fn platform_args(platform: &str, path: &str) -> Vec<String> {
    match platform {
        "PS1" | "PS2" => vec![
            "-fullscreen".into(),
            "-batch".into(),
            "--".into(),
            path.into(),
        ],
        // Nothing but the path. shadPS4 takes the game positionally,
        // and everything else about how it runs — fullscreen among
        // them — is its own configuration rather than ours to assert
        // over the top of on every launch. Anyone who does want a flag
        // has Args in Settings, which goes in front of this.
        "PC" | "PS4" => vec![path.into()],
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
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
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
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
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

        let depth = p
            .strip_prefix(install_dir)
            .map(|r| r.components().count())
            .unwrap_or(0);
        let size = p.metadata().map(|m| m.len()).unwrap_or(0);
        (
            is_non_game,
            title_match,
            depth,
            std::cmp::Reverse(size),
            name,
        )
    });

    exes
}

/// Disc formats worth opening, best first, per platform. Mirrors
/// DISC_EXTENSIONS in the server's library.py, and for the same
/// reason: a library is whatever somebody's rips happen to be, and
/// .cue alone — which is all this used to look for — finds nothing at
/// all in a folder of .chd files.
///
/// An .m3u names every disc of a set, so it beats any single disc; a
/// .cue describes the .bin beside it, so it beats that .bin; and a
/// raw .bin comes last, being usually the data half of a pair rather
/// than a thing to open.
const DISC_EXTENSIONS: &[(&str, &[&str])] = &[
    (
        "PS1",
        &[
            "m3u", "cue", "chd", "pbp", "ecm", "iso", "img", "mdf", "bin",
        ],
    ),
    (
        "PS2",
        &[
            "m3u", "iso", "chd", "cso", "zso", "gz", "cue", "mdf", "nrg", "img", "bin",
        ],
    ),
];

fn disc_extensions(platform: &str) -> Option<&'static [&'static str]> {
    DISC_EXTENSIONS
        .iter()
        .find(|(name, _)| *name == platform)
        .map(|(_, extensions)| *extensions)
}

/// Every disc in the install directory, best format first and then by
/// name, so a multi-disc set reads "Disc 1, Disc 2" rather than in
/// whatever order the filesystem hands them over.
///
/// Only the best format present is offered. A .cue/.bin pair is one
/// disc listed twice otherwise, and picking the .bin of a pair gets
/// an emulator a track with no table of contents.
fn ranked_discs(entries: &[PathBuf], extensions: &[&str]) -> Vec<PathBuf> {
    let Some(best) = extensions
        .iter()
        .find(|ext| entries.iter().any(|p| has_extension(p, ext)))
    else {
        return Vec::new();
    };

    let mut discs: Vec<PathBuf> = entries
        .iter()
        .filter(|p| has_extension(p, best))
        .cloned()
        .collect();
    discs.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    discs
}

/// Everything in the install directory worth offering as a thing to
/// launch, best first. PC titles list their executables; PS1/PS2 list
/// their discs, of which a multi-disc title has one per disc; a PS4
/// title lists its eboot.bin. Switch titles list nothing — a base game
/// and its updates aren't alternatives to each other, so there's no
/// choice to present.
fn launch_candidates(install_dir: &Path, platform: &str) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = Vec::new();
    collect_files(install_dir, &mut entries);
    entries.sort(); // fs::read_dir order is arbitrary and OS-dependent —
                    // without this, which file gets picked when a Switch
                    // install has more than one (base + update, say) isn't
                    // even stable across runs, let alone predictable.

    match platform {
        "PC" => ranked_executables(install_dir, &entries),
        "PS4" => {
            let mut boots: Vec<PathBuf> = entries
                .iter()
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.eq_ignore_ascii_case("eboot.bin"))
                })
                .cloned()
                .collect();
            // Shallowest first: a title's own eboot.bin sits at the top
            // of its tree, and anything deeper belongs to an update or
            // an add-on packaged inside it.
            boots.sort_by_key(|p| (p.components().count(), p.to_string_lossy().to_lowercase()));
            boots
        }
        _ => match disc_extensions(platform) {
            Some(extensions) => ranked_discs(&entries, extensions),
            None => Vec::new(),
        },
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

/// Which Windows runtime a platform is launched through.
///
/// Proton here means umu-launcher's `umu-run`, not Steam: umu fetches
/// the Steam Linux Runtime container and a Proton build itself, sets
/// the `STEAM_COMPAT_*` variables Proton insists on, and works with no
/// Steam client installed. What the app has to do differently is
/// small, and all of it is in this file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Runtime {
    Wine,
    Proton,
}

impl Runtime {
    /// Anything unrecognised is Wine. A settings.json naming a runtime
    /// a later build introduced should launch games through the thing
    /// this build does have rather than refuse to launch them at all.
    fn of(emu: &EmulatorConfig) -> Self {
        if emu.runtime.eq_ignore_ascii_case("proton") {
            Runtime::Proton
        } else {
            Runtime::Wine
        }
    }
}

/// The environment a prefix needs, for either runtime.
///
/// umu reads `WINEPREFIX` exactly as Wine does and then sets
/// `STEAM_COMPAT_DATA_PATH` to the same directory — the `pfx` it
/// creates inside is a symlink pointing back at the prefix itself —
/// so `drive_c/users/…` lands where the save sync and the prefix
/// migration already look for it. That is the whole reason Proton
/// costs so little here: the prefix layout does not change.
///
/// `PROTONPATH` is left unset when nothing is configured, which is
/// umu's way of being told to pick and download a build itself.
fn runtime_env(runtime: Runtime, emu: &EmulatorConfig, prefix: &Path) -> Vec<(String, String)> {
    let mut env = vec![(
        "WINEPREFIX".to_string(),
        prefix.to_string_lossy().to_string(),
    )];
    if runtime == Runtime::Proton && !emu.proton_path.trim().is_empty() {
        env.push(("PROTONPATH".to_string(), emu.proton_path.trim().to_string()));
    }
    env
}

/// What to run to create the prefix and stop, for either runtime.
///
/// `wineboot -i` is what Wine runs implicitly on first use.
/// `createprefix` is umu's equivalent: it is the documented way of
/// asking for a prefix with no executable to follow, which Proton
/// would otherwise refuse as a missing program.
///
/// Both go through the configured command and its `args_prefix`, so a
/// Flatpak Wine (`flatpak run … wineboot -i`) works the same way.
fn prefix_init_args(runtime: Runtime, emu: &EmulatorConfig) -> Vec<String> {
    let mut args = emu.args_prefix.clone();
    match runtime {
        Runtime::Wine => {
            args.push("wineboot".to_string());
            args.push("-i".to_string());
        }
        Runtime::Proton => args.push("createprefix".to_string()),
    }
    args
}

/// Creates the prefix up front, so the app decides what is in it
/// rather than discovering afterwards what Wine decided.
///
/// Running it deliberately just means it happens at a moment when the
/// prefix is known to be empty, which is the only moment
/// `isolate_profile_links` can safely do its work.
///
/// Best-effort: if this fails — no Wine, no umu, an unusual wrapper, a
/// permissions problem — the game is launched anyway and the runtime
/// creates the prefix itself, exactly as it did before. The cost is
/// the linked folders coming back, which the UI already reports.
fn initialise_prefix(runtime: Runtime, emu: &EmulatorConfig, prefix: &Path) -> Result<(), String> {
    let mut command = Command::new(&emu.command);
    command.args(prefix_init_args(runtime, emu));
    for (key, value) in runtime_env(runtime, emu, prefix) {
        command.env(key, value);
    }
    if runtime == Runtime::Wine {
        // Wine asks about installing Mono and Gecko in a dialog that
        // would sit there unanswered behind the launch. Neither is
        // needed to create the profile directories this is here for.
        // Proton brings its own and asks nothing, and telling it to
        // disable mscoree would cut .NET games off from the runtime it
        // ships, so this is Wine's alone.
        command.env("WINEDLLOVERRIDES", "mscoree,mshtml=");
    }

    let status = command
        .status()
        .map_err(|e| format!("preparing the prefix: {e}"))?;

    if !status.success() {
        return Err(format!("preparing the prefix exited with {status}"));
    }
    Ok(())
}

/// Replaces the profile folders Wine links out to the real home
/// directory with real directories inside the prefix.
///
/// Wine's Desktop Integration points `Documents`, `Saved Games` and
/// the rest at `$HOME`, which defeats the entire purpose of a per-game
/// prefix: a game that saves to Documents writes outside the prefix,
/// where the archive deliberately will not follow it. Until now the app
/// could only notice this and tell someone to fix it in winecfg.
///
/// `force` is the whole safety story. On a prefix this app just
/// created there is nothing behind those links yet, so replacing them
/// cannot lose anything. On an existing prefix it is only done for a
/// link whose target is missing or empty — because a game may already
/// have written saves through it, and quietly cutting them off would
/// look exactly like losing them.
///
/// Returns the folders it changed.
pub fn isolate_profile_links(prefix: &Path, force: bool) -> Vec<String> {
    let users = prefix.join("drive_c").join("users");
    let Ok(profiles) = std::fs::read_dir(&users) else {
        return Vec::new();
    };
    let inside = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());

    let mut changed: Vec<String> = Vec::new();
    for profile in profiles.flatten() {
        for name in crate::saves::PROFILE_SAVE_FOLDERS {
            let candidate = profile.path().join(name);
            let Ok(meta) = std::fs::symlink_metadata(&candidate) else {
                continue;
            };
            if !meta.file_type().is_symlink() {
                continue; // already a real directory; nothing to do
            }

            let target = std::fs::canonicalize(&candidate);
            if let Ok(target) = &target {
                if target.starts_with(&inside) {
                    continue; // points back into the prefix, so it is captured anyway
                }
            }

            let nothing_to_lose = match &target {
                Err(_) => true, // dangling: nothing can have been saved through it
                Ok(target) => std::fs::read_dir(target)
                    .map(|mut entries| entries.next().is_none())
                    .unwrap_or(false),
            };
            if !force && !nothing_to_lose {
                continue;
            }

            if std::fs::remove_file(&candidate).is_ok()
                && std::fs::create_dir_all(&candidate).is_ok()
            {
                changed.push(name.to_string());
            }
        }
    }
    changed.sort();
    changed.dedup();
    changed
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
        // The first install directory, even when there are several: a
        // prefix *is* the game's save data, so moving one when the
        // list of drives changes would lose saves. It stays where it
        // was made, and `prefix_root` exists for anyone who wants it
        // somewhere else entirely.
        settings.roots().first()?.join(".wine-prefixes")
    } else {
        PathBuf::from(&settings.prefix_root)
    };
    Some(absolute(&root.join(title)))
}

/// Anchors a relative path to the working directory.
///
/// An install root typed as a relative path was always a little
/// questionable — it would mean something different depending on where
/// the app was started from — but Wine tolerated it. umu does not: it
/// refuses a `WINEPREFIX` that isn't absolute outright, so the prefix
/// is resolved here instead of failing at launch.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
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
///
/// Proton needs none of that: `umu-run` defaults to Proton's
/// `waitforexitandrun` verb and so stays alive for as long as the game
/// does, which makes the child itself the right thing to wait on. The
/// host's `wineserver` is a different Wine from the one inside the
/// Proton build besides, and pointing it at a Proton prefix is not
/// something to do for no gain.
fn supervise(
    app: AppHandle,
    mut child: std::process::Child,
    game_id: String,
    prefix: Option<PathBuf>,
    runtime: Runtime,
    watching: Vec<crate::prefix_migrate::LinkSnapshot>,
    output: Option<PathBuf>,
) {
    let started_at = crate::playtime::now();
    let session_began = crate::prefix_migrate::now_millis();

    // Followed for as long as the session lasts, and read once more
    // after it ends — under Wine the game's own process outlives the
    // command that started it, and it is usually on the way out that a
    // game says why.
    let done = Arc::new(AtomicBool::new(false));
    if let Some(path) = output {
        let label = game_id
            .split_once('/')
            .map(|(_, title)| title.to_string())
            .unwrap_or_else(|| game_id.clone());
        crate::logs::follow(path, label, done.clone());
    }

    std::thread::spawn(move || {
        let _ = child.wait();
        if let (Runtime::Wine, Some(prefix)) = (runtime, &prefix) {
            let _ = Command::new("wineserver")
                .arg("-w")
                .env("WINEPREFIX", prefix)
                .status();
        }
        done.store(true, Ordering::Relaxed);

        // Only ever non-empty for a prefix that already had its save
        // folders linked out to the home directory. Now that the game
        // has run and stopped, the difference between the two pictures
        // of that directory is the folder it writes its saves to —
        // which is the folder to bring inside the prefix, where the
        // sync can actually see it.
        if !watching.is_empty() {
            let migrated = crate::prefix_migrate::migrate_after_session(&watching, session_began);
            if !migrated.moved.is_empty() {
                crate::log_line!("brought {} into the prefix", migrated.moved.join(", "));
                let _ = app.emit("prefix:saves-moved", (&game_id, &migrated));
            }
        }
        // Recorded once the session is genuinely over — after
        // wineserver has gone under Wine, or after umu-run has
        // returned under Proton. Under Wine the emulator process is
        // wine's launcher, which returns long before the game itself
        // does, and stopping the clock there would record every
        // session as a few seconds long.
        crate::playtime::finished(
            &app,
            &game_id,
            crate::playtime::now().saturating_sub(started_at),
        );
        // Best-effort: a missing listener is not worth reporting, and
        // there is nothing to retry against.
        let _ = app.emit("game:exited", &game_id);
    });
}

/// `executable` is a path relative to the install directory, as listed
/// by list_launch_candidates — the UI's picker passes back whichever
/// entry is selected. Left unset, the automatic choice is used, which
/// is what happens for every title that only has one candidate.
/// Preparing a Wine prefix runs `wineboot`, which takes seconds the
/// first time a game is played. A synchronous Tauri command runs on the
/// main thread, so doing that here would freeze the window mid-launch —
/// hence the hop onto a blocking thread. Everything the frontend sees
/// is unchanged: it awaits this the same way it always did.
#[tauri::command]
pub async fn launch_game(
    app: AppHandle,
    install_dir: String,
    platform: String,
    game_id: String,
    executable: Option<String>,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        launch_blocking(app, install_dir, platform, game_id, executable)
    })
    .await
    .map_err(|e| format!("launching failed: {e}"))?
}

fn launch_blocking(
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

    crate::log_line!("launch_game: {} {}", emu.command, shell_quote(&args));

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

    let runtime = Runtime::of(emu);
    let prefix = prefix_dir(&settings, &platform, &game_id);
    let mut watching = Vec::new();
    if let Some(prefix) = &prefix {
        if let Some(parent) = prefix.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }

        // A prefix that doesn't exist yet is created here rather than
        // implicitly by the game, purely so that the moment it exists
        // and is empty is a moment this app is present for. That is
        // the only point at which the linked-out profile folders can
        // be replaced without any chance of cutting a game off from
        // saves it has already written.
        let fresh = !prefix.exists();
        if fresh {
            if let Err(e) = initialise_prefix(runtime, emu, prefix) {
                // Not fatal: the runtime will create the prefix
                // itself, the links will be there, and the UI will say
                // so.
                crate::log_line!("could not prepare {}: {e}", prefix.display());
            }
        }

        let isolated = isolate_profile_links(prefix, fresh);
        if !isolated.is_empty() {
            crate::log_line!("kept {} inside {}", isolated.join(", "), prefix.display());
        }

        // Anything still linked out belongs to a prefix that predates
        // this, and may already hold saves. Rather than cut it — or
        // ask someone to — take a picture of it now and work out from
        // the session itself which folder is this game's. Only worth
        // doing when the saves are being synced at all.
        if settings.save_sync.enabled {
            watching = crate::prefix_migrate::snapshot(prefix);
        }

        for (key, value) in runtime_env(runtime, emu, prefix) {
            command.env(key, value);
        }
    }

    // The game's output goes to a file, which the Logs window then
    // follows. A pipe would have been the obvious way and the wrong
    // one: a pipe nobody is reading kills the writer, so closing Games
    // Galore would start taking running games down with it. A file
    // descriptor onto a file needs nobody alive at all.
    //
    // Every process the emulator starts inherits it too, which is the
    // point — under Wine the thing that prints is the game, several
    // processes below the command that was run here.
    let output = crate::logs::session_file(&app, &game_id);
    if let Some((path, file)) = &output {
        match (file.try_clone(), file.try_clone()) {
            (Ok(out), Ok(err)) => {
                command.stdout(Stdio::from(out));
                command.stderr(Stdio::from(err));
                crate::log_line!("this session's output: {}", path.display());
            }
            // Couldn't duplicate the handle: inherit the streams as
            // before rather than lose the launch over a log.
            _ => crate::log_line!("could not capture output to {}", path.display()),
        }
    }

    // Detached: the emulator's lifetime isn't tied to this app, so
    // closing Games Galore doesn't take a running game down with it.
    let child = command
        .spawn()
        .map_err(|e| format!("failed to launch {}: {e}", emu.command))?;

    supervise(
        app,
        child,
        game_id,
        prefix,
        runtime,
        watching,
        output.map(|(path, _)| path),
    );
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

    /// A prefix with a Windows profile in it, plus a home directory
    /// for its folders to link out to, the way Wine leaves one.
    fn prefix_fixture(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        let base = std::env::temp_dir().join(format!(
            "gg-prefix-test-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&base);
        let prefix = base.join("prefix");
        let users = prefix.join("drive_c/users/you");
        let home = base.join("home");
        fs::create_dir_all(&users).unwrap();
        fs::create_dir_all(&home).unwrap();
        (base, prefix, home)
    }

    fn link(from: &Path, to: &Path) {
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(to, from).unwrap();
    }

    #[test]
    fn a_fresh_prefix_keeps_its_save_folders_to_itself() {
        // The moment a prefix is created is the only moment these can
        // be replaced with certainty that nothing is behind them.
        let (base, prefix, home) = prefix_fixture("fresh");
        let users = prefix.join("drive_c/users/you");
        fs::create_dir_all(home.join("Documents")).unwrap();
        fs::write(home.join("Documents/tax-return.pdf"), "mine").unwrap();
        link(&users.join("Documents"), &home.join("Documents"));
        link(&users.join("Saved Games"), &home);

        let changed = isolate_profile_links(&prefix, true);

        assert_eq!(changed, vec!["Documents", "Saved Games"]);
        for name in ["Documents", "Saved Games"] {
            let path = users.join(name);
            assert!(path.is_dir(), "{name} should be a real directory now");
            assert!(
                !fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "{name} should not be a link"
            );
            assert_eq!(fs::read_dir(&path).unwrap().count(), 0, "{name} is empty");
        }
        // The user's own files are untouched: only the link was removed.
        assert_eq!(
            fs::read_to_string(home.join("Documents/tax-return.pdf")).unwrap(),
            "mine"
        );

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn an_existing_prefix_keeps_a_link_that_has_something_behind_it() {
        // A game may already have saved through this one, and cutting
        // it off silently would look exactly like losing the save.
        let (base, prefix, home) = prefix_fixture("existing-full");
        let users = prefix.join("drive_c/users/you");
        fs::create_dir_all(home.join("Documents/ULTRAKILL")).unwrap();
        fs::write(home.join("Documents/ULTRAKILL/slot1.bepis"), "act III").unwrap();
        link(&users.join("Documents"), &home.join("Documents"));

        assert!(isolate_profile_links(&prefix, false).is_empty());
        assert!(fs::symlink_metadata(users.join("Documents"))
            .unwrap()
            .file_type()
            .is_symlink());

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn an_existing_prefix_reclaims_a_link_with_nothing_behind_it() {
        let (base, prefix, home) = prefix_fixture("existing-empty");
        let users = prefix.join("drive_c/users/you");
        fs::create_dir_all(home.join("Documents")).unwrap();
        link(&users.join("Documents"), &home.join("Documents"));
        // Dangling: the target was never created at all.
        link(&users.join("Saved Games"), &home.join("Saved Games"));

        let changed = isolate_profile_links(&prefix, false);

        assert_eq!(changed, vec!["Documents", "Saved Games"]);
        assert!(users.join("Documents").is_dir());
        assert!(users.join("Saved Games").is_dir());

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_real_directory_and_an_inward_link_are_both_left_alone() {
        let (base, prefix, _home) = prefix_fixture("already-fine");
        let users = prefix.join("drive_c/users/you");
        fs::create_dir_all(users.join("Documents/ULTRAKILL")).unwrap();
        fs::create_dir_all(users.join("AppData/Roaming")).unwrap();
        // A link that stays inside the prefix is captured by the
        // archive anyway, so there is nothing to fix.
        link(&users.join("Saved Games"), &users.join("AppData/Roaming"));

        assert!(isolate_profile_links(&prefix, true).is_empty());
        assert!(users.join("Documents/ULTRAKILL").is_dir());
        assert!(fs::symlink_metadata(users.join("Saved Games"))
            .unwrap()
            .file_type()
            .is_symlink());

        fs::remove_dir_all(&base).unwrap();
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
            vec![
                "bin/HollowMeridian.exe",
                "unins000.exe",
                "redist/vcredist_x64.exe"
            ]
        );
        // The head of the list is exactly what the automatic pick uses.
        assert_eq!(
            find_local_game_file(&dir, "PC").unwrap(),
            dir.join(&names[0])
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_single_executable_gives_nothing_to_choose_between() {
        let dir = fixture("Ferrofluid", &[("Ferrofluid.exe", 10), ("assets.dat", 20)]);
        assert_eq!(launch_candidates(&dir, "PC").len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_of_chds_lists_each_disc() {
        let dir = fixture(
            "Chrono Harbour",
            &[
                ("Chrono Harbour (Disc 1).chd", 400_000),
                ("Chrono Harbour (Disc 2).chd", 380_000),
            ],
        );
        assert_eq!(
            relative_names(&dir, launch_candidates(&dir, "PS1")),
            vec!["Chrono Harbour (Disc 1).chd", "Chrono Harbour (Disc 2).chd"]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_cue_beats_the_bin_it_describes() {
        // Both are discs by extension; only one is a thing to open.
        let dir = fixture(
            "Paper Lantern",
            &[("Paper Lantern.cue", 300), ("Paper Lantern.bin", 500_000)],
        );
        assert_eq!(
            relative_names(&dir, launch_candidates(&dir, "PS1")),
            vec!["Paper Lantern.cue"]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_playlist_beats_the_discs_it_lists() {
        let dir = fixture(
            "Velvet Requiem",
            &[
                ("Velvet Requiem.m3u", 60),
                ("Velvet Requiem (Disc 1).chd", 200_000),
                ("Velvet Requiem (Disc 2).chd", 210_000),
            ],
        );
        assert_eq!(
            relative_names(&dir, launch_candidates(&dir, "PS1")),
            vec!["Velvet Requiem.m3u"]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_ps2_mdf_is_offered_and_its_descriptor_is_not() {
        let dir = fixture(
            "Ashen Circuit",
            &[("Ashen Circuit.mdf", 900_000), ("Ashen Circuit.mds", 400)],
        );
        assert_eq!(
            relative_names(&dir, launch_candidates(&dir, "PS2")),
            vec!["Ashen Circuit.mdf"]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_disc_down_a_subdirectory_is_still_found() {
        let dir = fixture("Gravel Saint", &[("discs/Gravel Saint.iso", 1_200_000)]);
        assert_eq!(
            find_local_game_file(&dir, "PS2").unwrap(),
            dir.join("discs/Gravel Saint.iso")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_ps4_title_launches_its_eboot() {
        let dir = fixture(
            "Cobalt Vein",
            &[
                ("sce_sys/param.sfo", 2_000),
                ("eboot.bin", 30_000),
                ("data/assets.pak", 4_000_000),
                // An add-on packaged inside the game brings its own,
                // which is not the one to start.
                ("addon/eboot.bin", 10_000),
            ],
        );
        assert_eq!(
            relative_names(&dir, launch_candidates(&dir, "PS4")),
            vec!["eboot.bin", "addon/eboot.bin"]
        );
        assert_eq!(
            find_local_game_file(&dir, "PS4").unwrap(),
            dir.join("eboot.bin")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shadps4_is_given_the_game_and_nothing_else() {
        // How it runs is shadPS4's own configuration; a flag asserted
        // here would sit on top of whatever was set there, on every
        // launch.
        assert_eq!(
            platform_args("PS4", "/games/PS4/Cobalt Vein/eboot.bin"),
            vec!["/games/PS4/Cobalt Vein/eboot.bin"]
        );
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
            assert_eq!(
                prefix_dir(&settings, platform, &format!("{platform}/X")),
                None
            );
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
            vec![
                "run",
                "net.pcsx2.PCSX2",
                "--",
                "-fullscreen",
                "-batch",
                "--",
                "/games/x.cue"
            ]
        );
    }

    #[test]
    fn shell_quote_escapes_embedded_quotes_and_spaces() {
        assert_eq!(
            shell_quote(&["/games/Moth & Ember/it's.exe".to_string()]),
            r#"'/games/Moth & Ember/it'\''s.exe'"#
        );
    }

    /// An emulator row as Settings would hand one over.
    fn emu(command: &str, runtime: &str, proton_path: &str) -> EmulatorConfig {
        EmulatorConfig {
            command: command.to_string(),
            args_prefix: vec![],
            version_flag: "--version".to_string(),
            runtime: runtime.to_string(),
            proton_path: proton_path.to_string(),
        }
    }

    #[test]
    fn an_unset_or_unknown_runtime_is_wine() {
        assert_eq!(Runtime::of(&emu("wine", "", "")), Runtime::Wine);
        assert_eq!(Runtime::of(&emu("wine", "wine", "")), Runtime::Wine);
        // A runtime some later build might add: launch it through what
        // this build has rather than not at all.
        assert_eq!(Runtime::of(&emu("wine", "hangover", "")), Runtime::Wine);
    }

    #[test]
    fn proton_is_recognised_however_it_is_capitalised() {
        assert_eq!(Runtime::of(&emu("umu-run", "proton", "")), Runtime::Proton);
        assert_eq!(Runtime::of(&emu("umu-run", "Proton", "")), Runtime::Proton);
    }

    #[test]
    fn both_runtimes_are_pointed_at_the_same_prefix() {
        let prefix = Path::new("/games/.wine-prefixes/Ferrofluid");
        let wine = runtime_env(Runtime::Wine, &emu("wine", "wine", ""), prefix);
        let proton = runtime_env(Runtime::Proton, &emu("umu-run", "proton", ""), prefix);

        // The layout inside the prefix is what the save sync and the
        // prefix migration are built on, so this is the load-bearing
        // assertion of the whole feature: umu takes WINEPREFIX too.
        let expected = (
            "WINEPREFIX".to_string(),
            "/games/.wine-prefixes/Ferrofluid".to_string(),
        );
        assert_eq!(wine, vec![expected.clone()]);
        assert_eq!(proton, vec![expected]);
    }

    #[test]
    fn a_configured_proton_build_is_passed_through_as_protonpath() {
        let env = runtime_env(
            Runtime::Proton,
            &emu("umu-run", "proton", "  GE-Proton  "),
            Path::new("/games/pfx"),
        );
        assert_eq!(
            env,
            vec![
                ("WINEPREFIX".to_string(), "/games/pfx".to_string()),
                ("PROTONPATH".to_string(), "GE-Proton".to_string()),
            ]
        );
    }

    #[test]
    fn a_proton_build_configured_for_wine_is_not_sent_to_wine() {
        let env = runtime_env(
            Runtime::Wine,
            &emu("wine", "wine", "GE-Proton"),
            Path::new("/games/pfx"),
        );
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "WINEPREFIX");
    }

    #[test]
    fn each_runtime_creates_a_prefix_its_own_way() {
        assert_eq!(
            prefix_init_args(Runtime::Wine, &emu("wine", "wine", "")),
            vec!["wineboot", "-i"]
        );
        assert_eq!(
            prefix_init_args(Runtime::Proton, &emu("umu-run", "proton", "")),
            vec!["createprefix"]
        );
    }

    #[test]
    fn a_wrapped_command_keeps_its_own_arguments_first() {
        let mut flatpak = emu("flatpak", "wine", "");
        flatpak.args_prefix = vec!["run".into(), "org.winehq.Wine".into(), "--".into()];
        assert_eq!(
            prefix_init_args(Runtime::Wine, &flatpak),
            vec!["run", "org.winehq.Wine", "--", "wineboot", "-i"]
        );
    }

    #[test]
    fn a_relative_prefix_root_is_resolved_before_it_reaches_the_runtime() {
        // umu refuses a WINEPREFIX that isn't absolute, so a relative
        // install root must not survive this far.
        let settings = Settings {
            install_root: "games".to_string(),
            ..Settings::default()
        };
        let prefix = prefix_dir(&settings, "PC", "PC/Ferrofluid").unwrap();
        assert!(prefix.is_absolute(), "{} is not absolute", prefix.display());
        assert!(prefix.ends_with("games/.wine-prefixes/Ferrofluid"));
    }

    #[test]
    fn an_absolute_prefix_root_is_left_exactly_as_it_was_typed() {
        let settings = Settings {
            prefix_root: "/mnt/slow/prefixes".to_string(),
            ..Settings::default()
        };
        assert_eq!(
            prefix_dir(&settings, "PC", "PC/Ferrofluid").unwrap(),
            PathBuf::from("/mnt/slow/prefixes/Ferrofluid")
        );
    }
}

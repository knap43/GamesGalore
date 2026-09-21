// What this session has printed, kept where the app itself can show
// it.
//
// Everything here used to go to stderr and nowhere else, which is
// fine when the app was started from a terminal and useless when it
// was started from the applications menu — precisely the case where
// "it didn't launch and I don't know why" happens. The lines still go
// to stderr; they are also kept in a ring buffer and pushed to the
// frontend as they happen, so the Logs window shows this session's
// output live.
//
// A game's own output is here too, by a longer route. Piping a game's
// streams into this process would have tied its survival to ours —
// once nothing is reading a pipe, the writer dies of it, and closing
// Games Galore is supposed to leave a running game running. So the
// game's output is redirected into a file instead, which no one has
// to be alive to keep valid, and this follows that file while the
// session lasts. The app gets the output, the terminal still gets it,
// and the game keeps writing into a perfectly good file descriptor
// whether or not the app is still there.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

/// Enough to cover a launch, an install and whatever went wrong after
/// them, and small enough that keeping it costs nothing worth
/// measuring. Older lines fall off the front.
const KEPT: usize = 500;

#[derive(Clone, Serialize)]
pub struct Line {
    /// Milliseconds since the epoch, formatted by whoever displays it.
    pub at: u64,
    pub text: String,
}

fn lines() -> &'static Mutex<VecDeque<Line>> {
    static LINES: OnceLock<Mutex<VecDeque<Line>>> = OnceLock::new();
    LINES.get_or_init(|| Mutex::new(VecDeque::with_capacity(KEPT)))
}

fn handle() -> &'static OnceLock<AppHandle> {
    static HANDLE: OnceLock<AppHandle> = OnceLock::new();
    &HANDLE
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Gives the recorder somewhere to push to, and routes panics through
/// it as well.
///
/// Called once, from setup. Lines recorded before this are kept and
/// simply aren't pushed anywhere, which is what the Logs window's
/// initial read is for; a panic after it is the single most useful
/// thing a log can contain, and the default hook still runs so the
/// terminal sees it exactly as before.
pub fn attach(app: &AppHandle) {
    let _ = handle().set(app.clone());

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record(format!("panic: {info}"));
        previous(info);
    }));
}

/// Prints a line, keeps it, and pushes it to the frontend.
///
/// Best-effort on the pushing: a frontend that isn't listening yet, or
/// a window that has gone away, is not a reason for the app to stop
/// doing whatever it was reporting on.
pub fn record(text: impl Into<String>) {
    let line = Line {
        at: now_millis(),
        text: text.into(),
    };
    eprintln!("{}", line.text);

    if let Ok(mut lines) = lines().lock() {
        if lines.len() == KEPT {
            lines.pop_front();
        }
        lines.push_back(line.clone());
    }

    if let Some(app) = handle().get() {
        let _ = app.emit("log:line", line);
    }
}

/// What the Logs window opens with, before the live lines start
/// arriving.
#[tauri::command]
pub fn get_logs() -> Vec<Line> {
    lines()
        .lock()
        .map(|lines| lines.iter().cloned().collect())
        .unwrap_or_default()
}

/// `eprintln!`, but the app can show it afterwards.
#[macro_export]
macro_rules! log_line {
    ($($arg:tt)*) => {
        $crate::logs::record(format!($($arg)*))
    };
}

// ---------------------------------------------------------------
// A GAME'S OUTPUT
// ---------------------------------------------------------------

/// How often the file is checked for new output. Fast enough that the
/// window feels live, slow enough that a chatty game costs one stat
/// and one read per interval rather than per line.
const POLL: Duration = Duration::from_millis(400);

/// Lines per poll that reach the window. A game logging every frame
/// would otherwise push everything else out of a 500-line buffer in
/// seconds, and flood the IPC doing it. The file keeps all of it; the
/// window says how much it skipped and where the rest is.
const MAX_LINES_PER_POLL: usize = 120;

/// Output files kept. Each is one launch, and the interesting one is
/// almost always the last — but "almost always" is why it is ten and
/// not one.
const RUNS_KEPT: usize = 10;

/// A line this long with no newline in it is something being drawn
/// rather than written — a progress bar redrawing itself with `\r`,
/// usually. Flushed as a line of its own rather than held forever.
const CARRY_LIMIT: usize = 4096;

/// Opens the file a game's output is redirected into, returning it and
/// its path. None if the directory can't be made, in which case the
/// caller launches the game with its streams inherited exactly as
/// before — a log is not worth failing a launch over.
pub fn session_file(app: &AppHandle, game_id: &str) -> Option<(PathBuf, File)> {
    let dir = app.path().app_data_dir().ok()?.join("game-output");
    fs::create_dir_all(&dir).ok()?;
    prune_runs(&dir);
    let path = dir.join(format!("{}-{}.log", file_stem(game_id), now_millis()));
    let file = File::create(&path).ok()?;
    Some((path, file))
}

/// A game id as a filename: "PC/Moth & Ember" -> "PC-Moth---Ember".
fn file_stem(game_id: &str) -> String {
    game_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Deletes all but the most recent few runs.
fn prune_runs(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let modified = entry.metadata().ok()?.modified().ok()?;
            (path.extension()? == "log").then_some((modified, path))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, stale) in files.into_iter().skip(RUNS_KEPT.saturating_sub(1)) {
        let _ = fs::remove_file(stale);
    }
}

/// Follows a game's output file until `done` is set, recording each
/// line under the game's name.
///
/// One last read happens after `done`, so whatever the game wrote on
/// its way out is not lost to the timing of the poll. A file the game
/// never wrote to is deleted rather than kept as evidence of silence.
pub fn follow(path: PathBuf, label: String, done: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut offset = 0u64;
        let mut carry: Vec<u8> = Vec::new();
        loop {
            let finished = done.load(Ordering::Relaxed);
            let (next, lines) = drain(&path, offset, &mut carry, &label);
            offset = next;
            for line in lines {
                record(line);
            }
            if finished {
                if !carry.is_empty() {
                    let text = String::from_utf8_lossy(&carry).trim_end().to_string();
                    if !text.is_empty() {
                        record(format!("[{label}] {text}"));
                    }
                }
                break;
            }
            std::thread::sleep(POLL);
        }

        if fs::metadata(&path).map(|m| m.len() == 0).unwrap_or(false) {
            let _ = fs::remove_file(&path);
        }
    });
}

/// Reads whatever has been appended since `offset` and returns the new
/// offset together with the lines to record. A trailing partial line
/// stays in `carry` for the next read to finish.
///
/// Returning the lines rather than recording them keeps the parsing —
/// which is where the fiddly cases are — testable without a log to
/// read them back out of.
fn drain(path: &Path, offset: u64, carry: &mut Vec<u8>, label: &str) -> (u64, Vec<String>) {
    let Ok(mut file) = File::open(path) else {
        return (offset, Vec::new());
    };
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return (offset, Vec::new());
    }
    let mut fresh = Vec::new();
    if file.read_to_end(&mut fresh).is_err() || fresh.is_empty() {
        return (offset, Vec::new());
    }
    let next = offset + fresh.len() as u64;
    carry.extend_from_slice(&fresh);

    let mut lines: Vec<String> = Vec::new();
    // `\r` counts as a line ending as well as `\n`: a progress bar
    // that redraws itself is otherwise one line of a thousand copies.
    while let Some(at) = carry.iter().position(|b| *b == b'\n' || *b == b'\r') {
        let line: Vec<u8> = carry.drain(..=at).collect();
        let text = String::from_utf8_lossy(&line[..line.len() - 1])
            .trim_end()
            .to_string();
        if !text.is_empty() {
            lines.push(text);
        }
    }
    if carry.len() > CARRY_LIMIT {
        lines.push(String::from_utf8_lossy(carry).trim_end().to_string());
        carry.clear();
    }

    let skipped = lines.len().saturating_sub(MAX_LINES_PER_POLL);
    let mut out = Vec::with_capacity(lines.len().min(MAX_LINES_PER_POLL) + 1);
    if skipped > 0 {
        out.push(format!(
            "[{label}] … {skipped} lines not shown here; all of them are in {}",
            path.display()
        ));
    }
    out.extend(
        lines
            .into_iter()
            .skip(skipped)
            .map(|line| format!("[{label}] {line}")),
    );
    (next, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_kept_and_stamped() {
        record("a test line");
        let kept = get_logs();
        let last = kept.last().expect("the line just recorded");
        assert_eq!(last.text, "a test line");
        assert!(last.at > 0);
    }

    /// A throwaway directory for the output-file tests.
    fn scratch(name: &str) -> PathBuf {
        use std::sync::atomic::AtomicUsize;
        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        let dir = std::env::temp_dir().join(format!(
            "gg-logs-test-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The lines one read of a file produces, as the follower sees
    /// them. Asserted on directly rather than through the shared log,
    /// which every other test in this binary is also writing to.
    fn lines_of(path: &Path) -> Vec<String> {
        let mut carry = Vec::new();
        drain(path, 0, &mut carry, "Ferrofluid").1
    }

    #[test]
    fn a_game_id_becomes_a_filename_nothing_can_escape() {
        assert_eq!(file_stem("PC/Moth & Ember"), "PC-Moth---Ember");
        // The separator and the dots are what matter: a game called
        // "../../etc" must not name a file outside the directory.
        assert_eq!(file_stem("PC/../../etc"), "PC-------etc");
    }

    #[test]
    fn each_line_of_output_is_attributed_to_the_game() {
        let dir = scratch("lines");
        let path = dir.join("session.log");
        fs::write(&path, "wine: created the prefix\nfixme: something\n").unwrap();

        assert_eq!(
            lines_of(&path),
            vec![
                "[Ferrofluid] wine: created the prefix",
                "[Ferrofluid] fixme: something",
            ]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_line_still_being_written_waits_for_the_rest_of_itself() {
        let dir = scratch("partial");
        let path = dir.join("session.log");
        fs::write(&path, "complete\nhalf a li").unwrap();

        let mut carry = Vec::new();
        let (offset, first) = drain(&path, 0, &mut carry, "Ferrofluid");
        assert_eq!(first, vec!["[Ferrofluid] complete"]);

        // The rest arrives, and the line is recorded whole rather than
        // in two halves.
        use std::io::Write;
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "ne").unwrap();
        let (_, second) = drain(&path, offset, &mut carry, "Ferrofluid");
        assert_eq!(second, vec!["[Ferrofluid] half a line"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_progress_bar_redrawing_itself_is_not_one_endless_line() {
        let dir = scratch("carriage");
        let path = dir.join("session.log");
        // `\r` with no `\n`, as anything that redraws in place emits.
        fs::write(&path, "loading 10%\rloading 20%\rloading 30%\r").unwrap();

        assert_eq!(
            lines_of(&path),
            vec![
                "[Ferrofluid] loading 10%",
                "[Ferrofluid] loading 20%",
                "[Ferrofluid] loading 30%",
            ]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_torrent_of_output_is_summarised_rather_than_flooded() {
        let dir = scratch("chatty");
        let path = dir.join("session.log");
        let noise: String = (0..MAX_LINES_PER_POLL + 200)
            .map(|i| format!("frame {i}\n"))
            .collect();
        fs::write(&path, noise).unwrap();

        let seen = lines_of(&path);
        assert_eq!(seen.len(), MAX_LINES_PER_POLL + 1);
        assert!(seen[0].contains("200 lines not shown here"), "{}", seen[0]);
        assert!(seen[0].contains(&path.display().to_string()), "{}", seen[0]);
        // The end is what was kept: a game's last words beat its first.
        assert!(seen.last().unwrap().ends_with("frame 319"), "{seen:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nothing_new_in_the_file_means_nothing_to_record() {
        let dir = scratch("quiet");
        let path = dir.join("session.log");
        fs::write(&path, "one line\n").unwrap();

        let mut carry = Vec::new();
        let (offset, first) = drain(&path, 0, &mut carry, "Ferrofluid");
        assert_eq!(first.len(), 1);
        let (again, second) = drain(&path, offset, &mut carry, "Ferrofluid");
        assert_eq!(again, offset);
        assert!(second.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn following_ends_with_the_session_and_leaves_no_empty_file() {
        let dir = scratch("silent");
        let path = dir.join("session.log");
        File::create(&path).unwrap();

        // Already finished: the follower reads once more, sees an
        // empty file, and tidies it away on its way out — which is
        // also how this asserts that the loop ends at all.
        follow(
            path.clone(),
            "Silent".to_string(),
            Arc::new(AtomicBool::new(true)),
        );
        for _ in 0..40 {
            if !path.exists() {
                break;
            }
            std::thread::sleep(POLL / 4);
        }
        assert!(!path.exists(), "an empty output file should be cleaned up");

        // A file with something in it is kept: it is the full copy the
        // window's summary points at.
        let kept = dir.join("kept.log");
        fs::write(&kept, "something\n").unwrap();
        follow(
            kept.clone(),
            "Talkative".to_string(),
            Arc::new(AtomicBool::new(true)),
        );
        std::thread::sleep(POLL);
        assert!(kept.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_child_keeps_writing_after_this_process_lets_go_of_the_file() {
        // The whole argument for a file rather than a pipe, exercised
        // rather than asserted: a real child, its output redirected
        // the way launch_game redirects it, still writing after every
        // handle on this side has been dropped. A pipe here would have
        // killed it on the next write.
        let dir = scratch("detached");
        let path = dir.join("session.log");
        let file = File::create(&path).unwrap();

        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("echo before; sleep 0.4; echo after")
            .stdout(std::process::Stdio::from(file.try_clone().unwrap()))
            .stderr(std::process::Stdio::from(file.try_clone().unwrap()))
            .spawn()
            .unwrap();
        drop(file); // nothing on this side holds the file any more

        // Waited for rather than assumed: the child needs a moment to
        // exist at all, and this is about what it writes, not when.
        for _ in 0..40 {
            if fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }

        let mut carry = Vec::new();
        let (offset, early) = drain(&path, 0, &mut carry, "Detached");
        assert_eq!(early, vec!["[Detached] before"]);

        let _ = child.wait();
        let (_, late) = drain(&path, offset, &mut carry, "Detached");
        assert_eq!(late, vec!["[Detached] after"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_the_last_few_runs_are_kept() {
        let dir = scratch("prune");
        for i in 0..RUNS_KEPT + 5 {
            fs::write(dir.join(format!("PC-Game-{i}.log")), "x").unwrap();
            // Distinct mtimes, so "newest" means something.
            std::thread::sleep(Duration::from_millis(5));
        }
        prune_runs(&dir);

        let left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        // One fewer than the cap: pruning runs before the new run's
        // own file is created, so the directory settles at RUNS_KEPT.
        assert_eq!(left.len(), RUNS_KEPT - 1, "{left:?}");
        assert!(
            left.contains(&format!("PC-Game-{}.log", RUNS_KEPT + 4)),
            "{left:?}"
        );
        assert!(!left.contains(&"PC-Game-0.log".to_string()), "{left:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_buffer_forgets_the_oldest_rather_than_growing() {
        // Deliberately more than the buffer holds, and asserted
        // against the cap rather than an exact history, because the
        // tests share one buffer and run in parallel.
        for i in 0..KEPT + 50 {
            record(format!("line {i}"));
        }
        let kept = get_logs();
        assert!(kept.len() <= KEPT, "{} lines kept", kept.len());
        assert!(kept.iter().any(|l| l.text == format!("line {}", KEPT + 49)));
        assert!(!kept.iter().any(|l| l.text == "line 0"));
    }
}

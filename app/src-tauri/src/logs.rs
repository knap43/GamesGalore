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
// Deliberately this app's own output, not the emulator's. Games are
// spawned with their streams inherited so that closing Games Galore
// leaves a running game running; piping them here would tie the
// game's survival to this process, which is a bad trade for a log.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

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

// Checks whether a configured emulator (or, for Switch installs, the
// server's `nsz`) is actually available — the frontend calls this
// against whatever's currently in Settings, so a missing tool shows up
// before Install or Play is clicked, not mid-launch.
//
// Two real checks live here, not one, because Flatpak installs need a
// fundamentally different question asked. `flatpak --version` only
// confirms Flatpak itself is installed — it says nothing about whether
// PCSX2's or Eden's specific Flatpak app is present, so checking that
// with the same "run it with a version flag" logic used for native
// binaries would report "found" even when the actual emulator isn't
// installed at all. `flatpak info <app-id>` asks the right question.
//
// "Not found" used to be the answer to every failure, which is the
// wrong answer for the most common one: an AppImage — how Eden and
// shadPS4 are both distributed — that is right there on disk and
// refuses to run. `detail` carries why, since the difference between
// "you typed the path wrong", "chmod +x it" and "this system has no
// libfuse.so.2" is the difference between a fix and an afternoon.

use serde::Serialize;
use std::path::Path;
use std::process::Command;

#[derive(Serialize)]
pub struct DependencyStatus {
    pub name: String,
    pub found: bool,
    pub version: Option<String>,
    /// Why it isn't found, in the program's own words where it had
    /// any, with a hint appended when the words are ones with a known
    /// fix. None when it is found.
    pub detail: Option<String>,
}

impl DependencyStatus {
    fn missing(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            found: false,
            version: None,
            detail: Some(detail.into()),
        }
    }
}

/// An AppImage needs FUSE 2 to mount itself, and a current Arch (or
/// any system that has moved on to fuse3 alone) doesn't have it. The
/// message it prints says "libfuse.so.2" and nothing about what to do,
/// so this says what to do.
const FUSE_HINT: &str = "— an AppImage needs FUSE 2 to mount itself. Either \
install it (`sudo pacman -S fuse2`), or extract the AppImage once \
(`./Whatever.AppImage --appimage-extract`) and point Command at the \
`squashfs-root/AppRun` it leaves behind, which needs no FUSE at all";

fn hint_for(message: &str) -> Option<&'static str> {
    let lower = message.to_lowercase();
    if lower.contains("libfuse") || lower.contains("fuse") && lower.contains("appimage") {
        return Some(FUSE_HINT);
    }
    None
}

/// The first line worth showing out of a failed run: whatever it said
/// on stderr, falling back to stdout.
fn first_line(bytes: &[u8], fallback: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let line = text.lines().find(|l| !l.trim().is_empty());
    match line {
        Some(line) => line.trim().to_string(),
        None => String::from_utf8_lossy(fallback)
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string(),
    }
}

/// Whether a path names something this user could execute. Only asked
/// of a command that is a path — a bare name is looked up on PATH by
/// the OS, and asking about permissions there would mean re-walking
/// PATH to guess which match it meant.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[tauri::command]
pub fn check_dependency(
    command: String,
    args_prefix: Vec<String>,
    version_flag: String,
) -> DependencyStatus {
    if command == "flatpak" {
        return check_flatpak_app(&args_prefix);
    }

    // A command with a separator in it is a path, and a path can be
    // wrong in ways a name cannot: nothing there, or something there
    // that won't run. Both are worth saying rather than rolling into
    // "not found".
    let path = Path::new(&command);
    if command.contains('/') {
        if !path.exists() {
            return DependencyStatus::missing(&command, "there is no file at that path");
        }
        if !is_executable(path) {
            return DependencyStatus::missing(
                &command,
                "the file is there but is not executable — `chmod +x` it",
            );
        }
    }

    match Command::new(&command).arg(&version_flag).output() {
        Ok(out) if out.status.success() || !out.stdout.is_empty() => DependencyStatus {
            name: command,
            found: true,
            version: Some(
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string(),
            ),
            detail: None,
        },
        // It exists and it ran, and it still didn't work: whatever it
        // printed on the way out is the most useful thing anybody has.
        Ok(out) => {
            let message = first_line(&out.stderr, &out.stdout);
            let detail = match hint_for(&message) {
                Some(hint) if !message.is_empty() => format!("{message} {hint}"),
                Some(hint) => hint.trim_start_matches("— ").to_string(),
                None if message.is_empty() => format!("it exited with {}", out.status),
                None => message,
            };
            DependencyStatus::missing(&command, detail)
        }
        Err(e) => DependencyStatus::missing(&command, e.to_string()),
    }
}

/// Expects args_prefix shaped like `["run", "<app-id>", "--"]` — the
/// same value the launch command itself uses — and pulls the app-id
/// out of it rather than requiring it as a separate field, so there's
/// only one place per platform where the Flatpak app-id is typed in.
fn check_flatpak_app(args_prefix: &[String]) -> DependencyStatus {
    let app_id = args_prefix
        .iter()
        .position(|a| a == "run")
        .and_then(|i| args_prefix.get(i + 1));

    let Some(app_id) = app_id else {
        return DependencyStatus::missing(
            "flatpak",
            "no app id in Args — it should read `run <app-id> --`",
        );
    };

    match Command::new("flatpak").arg("info").arg(app_id).output() {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("Version:"))
                .map(|v| v.trim().to_string());
            DependencyStatus {
                name: app_id.clone(),
                found: true,
                version,
                detail: None,
            }
        }
        Ok(out) => DependencyStatus::missing(app_id, first_line(&out.stderr, &out.stdout)),
        Err(e) => DependencyStatus::missing(app_id, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_to_nothing_says_so() {
        let status = check_dependency(
            "/definitely/not/here/shadPS4.AppImage".to_string(),
            vec![],
            "--version".to_string(),
        );
        assert!(!status.found);
        assert_eq!(status.detail.unwrap(), "there is no file at that path");
    }

    #[test]
    fn a_file_without_the_execute_bit_says_which_bit() {
        // How a freshly downloaded AppImage arrives, every time.
        let dir = std::env::temp_dir().join(format!("gg-dep-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shadPS4.AppImage");
        std::fs::write(&path, b"not really an AppImage").unwrap();

        let status = check_dependency(
            path.to_string_lossy().to_string(),
            vec![],
            "--version".to_string(),
        );
        assert!(!status.found);
        assert!(
            status.detail.unwrap().contains("chmod +x"),
            "should name the fix"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_command_that_runs_and_fails_is_quoted_rather_than_summarised() {
        let status = check_dependency(
            "/bin/sh".to_string(),
            vec![],
            "-c 'echo nope >&2; exit 1'".to_string(),
        );
        // sh rejects that as one argument, and whatever it says about
        // it beats "not found".
        assert!(!status.found);
        assert!(status.detail.is_some_and(|d| !d.is_empty()));
    }

    #[test]
    fn a_missing_libfuse_is_answered_with_what_to_install() {
        let hint = hint_for("dlopen(): error loading libfuse.so.2").unwrap();
        assert!(hint.contains("fuse2"));
        assert!(hint.contains("--appimage-extract"));
        assert!(hint_for("command not found").is_none());
    }

    #[test]
    fn a_real_command_still_reports_its_version() {
        // Something that exists, runs, and says something: the
        // ordinary answer, which the paths above must not disturb.
        let status = check_dependency("/bin/echo".to_string(), vec![], "5.1".to_string());
        assert!(status.found);
        assert_eq!(status.version.unwrap(), "5.1");
        assert!(status.detail.is_none());
    }
}

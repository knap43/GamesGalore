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

use serde::Serialize;
use std::process::Command;

#[derive(Serialize)]
pub struct DependencyStatus {
    pub name: String,
    pub found: bool,
    pub version: Option<String>,
}

#[tauri::command]
pub fn check_dependency(command: String, args_prefix: Vec<String>, version_flag: String) -> DependencyStatus {
    if command == "flatpak" {
        return check_flatpak_app(&args_prefix);
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
        },
        _ => DependencyStatus {
            name: command,
            found: false,
            version: None,
        },
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
        return DependencyStatus { name: "flatpak".to_string(), found: false, version: None };
    };

    match Command::new("flatpak").arg("info").arg(app_id).output() {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("Version:"))
                .map(|v| v.trim().to_string());
            DependencyStatus { name: app_id.clone(), found: true, version }
        }
        _ => DependencyStatus { name: app_id.clone(), found: false, version: None },
    }
}

// Talks to the Python library server (the separate vault-server
// project, running on the laptop with the mounted drive) instead of
// touching any filesystem or network share directly. This app has no
// local knowledge of where the library lives — it only knows a base
// URL, passed in from settings.
//
// Both routes here are read-only GETs; the server does all the real
// work (scanning the folder tree, deciding what needs NSZ conversion).

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GameFile {
    pub filename: String,
    pub format: String, // "nsz" | "nsp" | extension without the dot, for other platforms
    pub needs_conversion: bool,
    pub size_bytes: u64, // on-disk size in the source library — compressed
                         // size for .nsz, smaller than the eventual installed size
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Game {
    pub id: String,
    pub title: String,
    pub platform: String,
    pub release_year: Option<u32>,
    pub description: String,
    pub files: Vec<GameFile>,
    pub screenshots: Vec<String>, // absolute URLs, already resolved by the server
    pub cover: Option<String>,    // absolute URL — whichever screenshot has "cover" in its
                                   // filename, or the first screenshot if none does
    pub trailer: Option<String>,  // absolute URL
}

#[tauri::command]
pub async fn fetch_library(server_base: String) -> Result<Vec<Game>, String> {
    let url = format!("{}/library", server_base.trim_end_matches('/'));
    let response = reqwest::get(&url).await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("server returned {} fetching {}", response.status(), url));
    }
    response.json::<Vec<Game>>().await.map_err(|e| e.to_string())
}

/// Surfaces the server's own `nsz` check in the client UI — e.g. to
/// grey out installing a Switch title if the server that would do the
/// conversion doesn't actually have the tool available.
#[tauri::command]
pub async fn fetch_server_status(server_base: String) -> Result<serde_json::Value, String> {
    let url = format!("{}/status", server_base.trim_end_matches('/'));
    let response = reqwest::get(&url).await.map_err(|e| e.to_string())?;
    response.json().await.map_err(|e| e.to_string())
}

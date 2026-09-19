// Talks to the Python library server (the separate server/
// project, running on the laptop with the mounted drive) instead of
// touching any filesystem or network share directly. This app has no
// local knowledge of where the library lives — it only knows a base
// URL, passed in from settings.
//
// Both routes here are read-only GETs; the server does all the real
// work (scanning the folder tree, deciding what needs NSZ conversion).

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

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
    pub trailer: Option<String>, // absolute URL
}

#[tauri::command]
pub async fn fetch_library(app: AppHandle, server_base: String) -> Result<Vec<Game>, String> {
    let url = format!("{}/library", server_base.trim_end_matches('/'));
    let response = reqwest::get(&url).await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "server returned {} fetching {}",
            response.status(),
            url
        ));
    }
    let games = response
        .json::<Vec<Game>>()
        .await
        .map_err(|e| e.to_string())?;

    // Keeps the installed titles' cached entries current — names,
    // covers and blurbs change on the server side, and this is the
    // only moment the client ever hears about it. Best-effort inside;
    // a cache that can't be written doesn't spoil a good fetch.
    crate::catalog_cache::sync(&app, &games);

    Ok(games)
}

/// What one game's metadata fetch did, mirrored from the server's JSON.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FetchedMetadata {
    pub title: String,
    pub matched: Option<String>,
    pub wrote: Vec<String>,
    pub skipped: Vec<String>,
    pub error: Option<String>,
}

/// Asks the server to fill in a game's folder from RAWG — description,
/// cover, screenshots, trailer and the genre sidecar.
///
/// The work happens on the server because that is the machine holding
/// the library; this end only asks, waits, and shows what came back.
/// The catalog it refetches afterwards is the point: the grid and the
/// detail view are reading the very files that just appeared.
#[tauri::command]
pub async fn fetch_metadata(
    app: AppHandle,
    server_base: String,
    game_id: String,
    // `bulk` is true while working through a list: the server then
    // skips its rescan, which would otherwise cost a full library scan
    // per game rather than one at the end.
    bulk: Option<bool>,
) -> Result<FetchedMetadata, String> {
    let url = format!(
        "{}/metadata/{}{}",
        server_base.trim_end_matches('/'),
        crate::install_state::encode_path_segments(&game_id),
        if bulk.unwrap_or(false) {
            "?rescan=0"
        } else {
            ""
        },
    );

    // The same token the save endpoints take: the server guards
    // everything that writes with one key, and this writes into the
    // library itself.
    let settings = crate::settings::get_settings(app);
    let request = reqwest::Client::new().post(url);
    let request = match settings.save_sync.token.trim() {
        "" => request,
        token => request.bearer_auth(token),
    };
    // The RAWG key travels in a header rather than the query string, so
    // it stays out of access logs. Omitted entirely when empty, leaving
    // the server to use whatever is in its own config.
    let request = match settings.rawg_key.trim() {
        "" => request,
        key => request.header("X-RAWG-Key", key),
    };

    let response = request.send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "server returned {status}: {}",
            crate::install_state::extract_error_detail(&body)
        ));
    }
    response
        .json::<FetchedMetadata>()
        .await
        .map_err(|e| e.to_string())
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

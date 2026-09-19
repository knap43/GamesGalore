mod catalog_cache;
mod dependencies;
mod install_state;
mod launcher;
mod migrate;
mod playtime;
mod prefix_migrate;
mod saves;
mod server;
mod settings;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // Before any command can read installs.json or settings.json,
        // so a user whose data still sits under the old identifier
        // doesn't briefly look like a user with no data at all.
        .setup(|app| {
            migrate::move_legacy_app_data(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            server::fetch_library,
            catalog_cache::get_cached_library,
            server::fetch_server_status,
            server::fetch_metadata,
            dependencies::check_dependency,
            install_state::get_install_states,
            install_state::install_game,
            install_state::uninstall_game,
            install_state::cancel_install,
            launcher::launch_game,
            launcher::list_launch_candidates,
            playtime::get_playtime,
            saves::save_status,
            saves::upload_save,
            saves::download_save,
            saves::list_switch_title_ids,
            saves::detect_switch_title_id,
            saves::detect_switch_data_dir,
            saves::set_switch_title_id,
            settings::get_settings,
            settings::save_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

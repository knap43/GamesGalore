mod dependencies;
mod install_state;
mod launcher;
mod server;
mod settings;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            server::fetch_library,
            server::fetch_server_status,
            dependencies::check_dependency,
            install_state::get_install_states,
            install_state::install_game,
            install_state::uninstall_game,
            install_state::cancel_install,
            launcher::launch_game,
            launcher::list_launch_candidates,
            settings::get_settings,
            settings::save_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

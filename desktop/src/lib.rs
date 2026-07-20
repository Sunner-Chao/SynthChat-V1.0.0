mod backend;
mod pet;
mod runtime_config;

use serde::Serialize;
use tauri::Manager;

use backend::{BackendConnection, BackendManager, BackendStatus};
use pet::{
    open_pet_window, pet_window_set_ignore_cursor_events, pet_window_start_dragging,
    toggle_pet_window,
};
use runtime_config::FrontendRuntimeConfig;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppBuildInfo {
    product_name: String,
    version: String,
    identifier: String,
    target: String,
    update_manifest_url: String,
}

#[tauri::command]
fn get_app_build_info(app: tauri::AppHandle) -> AppBuildInfo {
    AppBuildInfo {
        product_name: app.package_info().name.clone(),
        version: app.package_info().version.to_string(),
        identifier: app.config().identifier.clone(),
        target: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        update_manifest_url: option_env!("SYNTHCHAT_UPDATE_MANIFEST_URL")
            .unwrap_or_default()
            .to_owned(),
    }
}

#[tauri::command]
fn get_backend_connection(
    manager: tauri::State<'_, BackendManager>,
) -> Result<BackendConnection, String> {
    manager.connection()
}

#[tauri::command]
fn backend_status(manager: tauri::State<'_, BackendManager>) -> BackendStatus {
    manager.status()
}

#[tauri::command]
fn get_frontend_runtime_config() -> Result<FrontendRuntimeConfig, String> {
    FrontendRuntimeConfig::from_env()
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app.manage(BackendManager::start());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_build_info,
            get_backend_connection,
            backend_status,
            get_frontend_runtime_config,
            open_pet_window,
            toggle_pet_window,
            pet_window_start_dragging,
            pet_window_set_ignore_cursor_events
        ])
        .run(tauri::generate_context!())
        .expect("failed to run SynthChat desktop shell");
}

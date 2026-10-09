mod audio;
mod cleanup;
mod commands;
mod hotkey;
mod login;
mod model;
mod paste;
mod permissions;
mod pill;
mod pipeline;
mod settings;
mod stt;
mod tray;

use tauri::WindowEvent;

pub const SETTINGS_WINDOW: &str = "main";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(hotkey::plugin())
        .manage(pipeline::Pipeline::new())
        .manage(model::ModelManager::new())
        .manage(stt::Stt::default())
        .manage(cleanup::HttpClient::new())
        .manage(settings::ApiKeyStore::default())
        .invoke_handler(tauri::generate_handler![
            commands::get_general,
            commands::save_general,
            commands::suspend_hotkey,
            commands::resume_hotkey,
            commands::app_paths,
            commands::launch_at_login,
            commands::set_launch_at_login,
            commands::permission_status,
            commands::fix_microphone,
            commands::fix_accessibility,
            commands::model_status,
            commands::download_model,
            commands::cancel_model_download,
            commands::get_cleanup,
            commands::save_cleanup,
            commands::set_cleanup_key,
            commands::inspect_base_url,
            commands::test_cleanup_connection,
            commands::try_cleanup,
        ])
        .setup(|app| {
            // Menu-bar only: no dock icon. Bundled builds also get this via
            // LSUIElement in Info.plist; this covers `tauri dev`.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            tray::init(app.handle())?;
            pill::init(app.handle());
            let settings = settings::Settings::load_or_init(app.handle());
            if let Some(path) = settings::Settings::path(app.handle()) {
                println!("[settings] {}", path.display());
            }
            if let Err(e) = hotkey::register(app.handle(), &settings.hotkey) {
                eprintln!("[hotkey] {e}; falling back to {}", hotkey::DEFAULT_HOTKEY);
                if let Err(e) = hotkey::register(app.handle(), hotkey::DEFAULT_HOTKEY) {
                    eprintln!("[hotkey] {e}");
                }
            }
            // No usable model yet: open settings so the user can download it.
            let model_ready = model::init(app.handle());
            // Without Accessibility, Cmd+V can't be sent. The system prompt
            // links to System Settings and adds the app to the list.
            let trusted =
                permissions::accessibility_trusted() || permissions::request_accessibility();
            if !model_ready || !trusted {
                tray::show_settings(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the settings window hides it; the app lives in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == SETTINGS_WINDOW {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, _event| {});
}

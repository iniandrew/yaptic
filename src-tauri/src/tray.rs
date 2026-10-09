use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, Runtime,
};

use crate::SETTINGS_WINDOW;

/// Monochrome bars + caret. As a template image macOS tints it to match a
/// light or dark menu bar. The @2x asset keeps it sharp on Retina.
const TRAY_ICON: &[u8] = include_bytes!("../icons/tray-template@2x.png");

pub fn show_settings<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub fn init<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, Some("Cmd+,"))?;
    let quit = MenuItem::with_id(app, "quit", "Quit Yaptic", true, Some("Cmd+Q"))?;
    let menu = Menu::with_items(app, &[&settings, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(Image::from_bytes(TRAY_ICON)?)
        .icon_as_template(true)
        .tooltip("Yaptic")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "settings" => show_settings(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

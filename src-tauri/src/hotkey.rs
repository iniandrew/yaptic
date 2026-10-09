use tauri::{plugin::TauriPlugin, AppHandle, Runtime};
use tauri_plugin_global_shortcut::{Builder, Code, GlobalShortcutExt, Shortcut, ShortcutState};

use crate::pipeline;

/// Push-to-talk default: Option+Space.
pub const DEFAULT_HOTKEY: &str = "Alt+Space";

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    Builder::new()
        .with_handler(|app, _shortcut, event| match event.state {
            ShortcutState::Pressed => pipeline::on_press(app),
            ShortcutState::Released => pipeline::on_release(app),
        })
        .build()
}

/// Parses a shortcut like "Alt+Space" (browser `KeyboardEvent.code` names
/// work as key tokens) and rejects ones that would swallow a normal key
/// everywhere: no modifier, unless it's a function key.
pub fn parse(hotkey: &str) -> Result<Shortcut, String> {
    let shortcut: Shortcut = hotkey
        .parse()
        .map_err(|e| format!("invalid shortcut \"{hotkey}\": {e}"))?;
    let function_key = matches!(
        shortcut.key,
        Code::F1 | Code::F2 | Code::F3 | Code::F4 | Code::F5 | Code::F6
            | Code::F7 | Code::F8 | Code::F9 | Code::F10 | Code::F11 | Code::F12
            | Code::F13 | Code::F14 | Code::F15 | Code::F16 | Code::F17 | Code::F18
            | Code::F19 | Code::F20
    );
    if shortcut.mods.is_empty() && !function_key {
        return Err("Add a modifier (⌃ ⌥ ⇧ ⌘) or use a function key".into());
    }
    Ok(shortcut)
}

/// Makes `hotkey` the only registered shortcut.
pub fn register<R: Runtime>(app: &AppHandle<R>, hotkey: &str) -> Result<(), String> {
    let shortcut = parse(hotkey)?;
    let shortcuts = app.global_shortcut();
    shortcuts
        .unregister_all()
        .map_err(|e| format!("unregister shortcut: {e}"))?;
    shortcuts
        .register(shortcut)
        .map_err(|e| format!("register {hotkey}: {e} (another app may be using it)"))
}

/// Unregisters the shortcut so the settings window can capture a new one
/// (a registered shortcut never reaches the webview). Cancels a recording
/// in progress, since its release event would never arrive.
pub fn suspend<R: Runtime>(app: &AppHandle<R>) {
    pipeline::cancel(app);
    if let Err(e) = app.global_shortcut().unregister_all() {
        eprintln!("[hotkey] suspend: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_browser_key_codes_with_modifiers() {
        for hotkey in ["Alt+Space", "Control+Alt+KeyD", "Super+Shift+Digit1", "F5", "Alt+Backquote"] {
            assert!(parse(hotkey).is_ok(), "{hotkey}");
        }
    }

    #[test]
    fn rejects_bare_keys_and_garbage() {
        assert!(parse("Space").is_err());
        assert!(parse("KeyA").is_err());
        assert!(parse("Alt+Nope").is_err());
    }
}

use std::{
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, Runtime};

pub const PILL_WINDOW: &str = "pill";
const STATE_EVENT: &str = "pill-state";
const LEVEL_EVENT: &str = "pill-level";

/// Distance from the bottom edge of the screen, in logical pixels.
const BOTTOM_MARGIN: f64 = 96.0;

/// Bumped on every update so a pending auto-hide can't hide a newer state.
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PillState {
    Listening,
    Transcribing,
    Cleaning,
    Done { message: String },
    Info { message: String },
    Warning { message: String },
    Error { message: String },
}

impl PillState {
    pub fn done(message: impl Into<String>) -> Self {
        Self::Done { message: message.into() }
    }
    pub fn info(message: impl Into<String>) -> Self {
        Self::Info { message: message.into() }
    }
    pub fn warning(message: impl Into<String>) -> Self {
        Self::Warning { message: message.into() }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error { message: message.into() }
    }

    /// How long a final state stays up before the pill hides itself.
    fn linger(&self) -> Option<Duration> {
        let ms = match self {
            Self::Listening | Self::Transcribing | Self::Cleaning => return None,
            Self::Done { .. } => 700,
            Self::Info { .. } => 1800,
            Self::Warning { .. } => 3500,
            Self::Error { .. } => 4500,
        };
        Some(Duration::from_millis(ms))
    }
}

/// Makes the pill click-through; it is purely an indicator.
pub fn init<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(PILL_WINDOW) {
        let _ = window.set_ignore_cursor_events(true);

        // macOS keeps plain NSWindows off other apps' fullscreen Spaces; a nonactivating
        // NSPanel at status level is allowed there. Borderless panels never become key,
        // so dropping tao's NSWindow subclass loses nothing the pill uses.
        #[cfg(target_os = "macos")]
        if let Ok(ns_window) = window.ns_window() {
            use objc2::{runtime::AnyObject, ClassType};
            use objc2_app_kit::{
                NSPanel, NSStatusWindowLevel, NSWindow, NSWindowCollectionBehavior,
                NSWindowStyleMask,
            };
            let ns_window: &NSWindow = unsafe { &*ns_window.cast() };
            unsafe { AnyObject::set_class(ns_window, NSPanel::class()) };
            ns_window.setStyleMask(ns_window.styleMask() | NSWindowStyleMask::NonactivatingPanel);
            ns_window.setHidesOnDeactivate(false);
            ns_window.setLevel(NSStatusWindowLevel);
            ns_window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
        }
    }
}

/// Shows `state`; final states hide themselves after a moment.
pub fn set<R: Runtime>(app: &AppHandle<R>, state: PillState) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let linger = state.linger();
    let _ = app.emit_to(PILL_WINDOW, STATE_EVENT, &state);
    show(app);

    if let Some(delay) = linger {
        let app = app.clone();
        thread::spawn(move || {
            thread::sleep(delay);
            if GENERATION.load(Ordering::SeqCst) == generation {
                hide(&app);
            }
        });
    }
}

pub fn hide<R: Runtime>(app: &AppHandle<R>) {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    if let Some(window) = app.get_webview_window(PILL_WINDOW) {
        let _ = window.hide();
    }
}

/// Feeds the live mic level (RMS) to the pill's bars while listening.
pub fn set_level<R: Runtime>(app: &AppHandle<R>, level: f32) {
    let _ = app.emit_to(PILL_WINDOW, LEVEL_EVENT, level);
}

/// Shows the pill centered near the bottom of the monitor under the cursor.
/// Only positions it when it appears, so it doesn't jump between states.
fn show<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(PILL_WINDOW) else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        return;
    }

    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());

    if let (Some(monitor), Ok(size)) = (monitor, window.outer_size()) {
        let origin = monitor.position();
        let area = monitor.size();
        let margin = (BOTTOM_MARGIN * monitor.scale_factor()) as i32;
        let x = origin.x + (area.width as i32 - size.width as i32) / 2;
        let y = origin.y + area.height as i32 - size.height as i32 - margin;
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }

    let _ = window.show();
}

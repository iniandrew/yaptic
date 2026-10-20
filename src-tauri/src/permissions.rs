use std::{ffi::c_void, process::Command};

use objc2::{class, msg_send, rc::Retained};
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use serde::Serialize;

const MICROPHONE_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";
const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
}

#[link(name = "AVFoundation", kind = "framework")]
extern "C" {
    static AVMediaTypeAudio: &'static NSString;
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MicrophoneStatus {
    /// Not asked yet; opening the mic shows the system prompt.
    Undetermined,
    /// Blocked by device management.
    Restricted,
    Denied,
    Granted,
}

pub fn microphone_status() -> MicrophoneStatus {
    // AVAuthorizationStatus: 0 notDetermined, 1 restricted, 2 denied, 3 authorized.
    let status: isize = unsafe {
        msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: AVMediaTypeAudio]
    };
    match status {
        1 => MicrophoneStatus::Restricted,
        2 => MicrophoneStatus::Denied,
        3 => MicrophoneStatus::Granted,
        _ => MicrophoneStatus::Undetermined,
    }
}

pub fn open_microphone_settings() {
    open_url(MICROPHONE_SETTINGS_URL);
}

/// Whether this process may post synthetic key events (Accessibility).
pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

/// Shows the system prompt that links to System Settings and adds the app
/// to the Accessibility list. Returns the current trust state.
pub fn request_accessibility() -> bool {
    let key = NSString::from_str("AXTrustedCheckOptionPrompt");
    let value = NSNumber::new_bool(true);
    let options = NSDictionary::from_slices(&[&*key], &[&*value]);
    unsafe { AXIsProcessTrustedWithOptions(Retained::as_ptr(&options).cast()) != 0 }
}

pub fn open_accessibility_settings() {
    open_url(ACCESSIBILITY_SETTINGS_URL);
}

pub fn open_url(url: &str) {
    if let Err(e) = Command::new("open").arg(url).spawn() {
        eprintln!("[permissions] open System Settings: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_queries_do_not_panic() {
        // objc2 verifies the selector's return type in debug builds.
        let _ = microphone_status();
        let _ = accessibility_trusted();
    }
}

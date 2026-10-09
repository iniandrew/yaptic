use std::{thread, time::Duration};

use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGKeyCode,
};
use objc2_foundation::{NSArray, NSData, NSString};

use crate::permissions;

/// Virtual key codes (ANSI positions).
const KEY_V: CGKeyCode = 9;
const KEY_COMMAND: CGKeyCode = 55;

/// nspasteboard.org marker: clipboard managers skip content tagged with it.
const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";

/// Pastes `text` at the cursor: swaps it onto the clipboard, sends Cmd+V,
/// then restores the previous clipboard after `restore_after` (some apps
/// read the clipboard late). `on_pasted` runs right after Cmd+V, before that
/// wait. Without Accessibility permission the text is left on the clipboard
/// for a manual paste and an error is returned.
pub fn paste(text: &str, restore_after: Duration, on_pasted: impl FnOnce()) -> Result<(), String> {
    let pasteboard = NSPasteboard::generalPasteboard();

    if !permissions::accessibility_trusted() {
        write_text(&pasteboard, text, false);
        return Err(
            "Accessibility permission missing; text copied to clipboard instead".to_string(),
        );
    }

    let saved = snapshot(&pasteboard);
    let our_change = write_text(&pasteboard, text, true);
    post_cmd_v()?;
    on_pasted();

    thread::sleep(restore_after);
    // Don't clobber something the user copied in the meantime.
    if pasteboard.changeCount() == our_change {
        restore(&pasteboard, saved);
    }
    Ok(())
}

type ItemData = Vec<(Retained<NSString>, Retained<NSData>)>;

/// Copies every item and type on the clipboard (text, images, files, ...).
fn snapshot(pasteboard: &NSPasteboard) -> Vec<ItemData> {
    let Some(items) = pasteboard.pasteboardItems() else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            item.types()
                .iter()
                .filter_map(|ty| item.dataForType(&ty).map(|data| (ty, data)))
                .collect()
        })
        .collect()
}

fn restore(pasteboard: &NSPasteboard, saved: Vec<ItemData>) {
    pasteboard.clearContents();
    if saved.is_empty() {
        return;
    }
    let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = saved
        .into_iter()
        .map(|types| {
            let item = NSPasteboardItem::new();
            for (ty, data) in types {
                item.setData_forType(&data, &ty);
            }
            ProtocolObject::from_retained(item)
        })
        .collect();
    pasteboard.writeObjects(&NSArray::from_retained_slice(&items));
}

/// Replaces the clipboard with `text`; returns the resulting change count.
fn write_text(pasteboard: &NSPasteboard, text: &str, transient: bool) -> isize {
    pasteboard.clearContents();
    let item = NSPasteboardItem::new();
    item.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
    if transient {
        item.setData_forType(&NSData::new(), &NSString::from_str(TRANSIENT_TYPE));
    }
    let item = ProtocolObject::from_retained(item);
    pasteboard.writeObjects(&NSArray::from_retained_slice(&[item]));
    pasteboard.changeCount()
}

/// Sends Cmd+V. Flags are set explicitly on every event so a modifier the
/// user is still holding (e.g. Option from the hotkey) doesn't leak in.
/// Key code 9 is V on QWERTY/QWERTZ/AZERTY; Dvorak would need a layout lookup.
fn post_cmd_v() -> Result<(), String> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let keys = [
        (KEY_COMMAND, true),
        (KEY_V, true),
        (KEY_V, false),
        (KEY_COMMAND, false),
    ];
    for (key, down) in keys {
        let event = CGEvent::new_keyboard_event(source.as_deref(), key, down)
            .ok_or("failed to create key event")?;
        let flags = if key == KEY_COMMAND && !down {
            CGEventFlags::empty()
        } else {
            CGEventFlags::MaskCommand
        };
        CGEvent::set_flags(Some(&event), flags);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types_and_bytes(pasteboard: &NSPasteboard) -> Vec<Vec<(String, Vec<u8>)>> {
        snapshot(pasteboard)
            .into_iter()
            .map(|item| {
                item.into_iter()
                    .map(|(ty, data)| (ty.to_string(), data.to_vec()))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn restores_every_item_and_type() {
        // A private pasteboard, so the user's clipboard is untouched.
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        let png = NSString::from_str("public.png");
        let item = NSPasteboardItem::new();
        item.setData_forType(&NSData::with_bytes(&[0x89, b'P', b'N', b'G']), &png);
        item.setString_forType(&NSString::from_str("caption"), unsafe {
            NSPasteboardTypeString
        });
        pasteboard.clearContents();
        pasteboard.writeObjects(&NSArray::from_retained_slice(&[
            ProtocolObject::from_retained(item),
        ]));
        let before = types_and_bytes(&pasteboard);

        let saved = snapshot(&pasteboard);
        let change = write_text(&pasteboard, "dictated", true);
        assert_eq!(pasteboard.changeCount(), change);
        let during = types_and_bytes(&pasteboard);
        assert!(during[0].iter().any(|(ty, _)| ty == TRANSIENT_TYPE));

        restore(&pasteboard, saved);
        assert_eq!(types_and_bytes(&pasteboard), before);
    }
}

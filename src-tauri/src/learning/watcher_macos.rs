//! macOS Accessibility adapter: reads the focused text box for the watch and
//! writes a swap into it.
//!
//! Every call here blocks on another process, so none may run on the main
//! thread. The watch thread and the swap command call them.
//!
//! Needs the Accessibility permission (`AXIsProcessTrusted`). Without it every
//! function says "no": no watch starts and a swap falls back to the clipboard.

use std::ptr::{null, NonNull};
use std::time::Duration;

use log::{debug, warn};
use objc2_app_kit::NSWorkspace;
use objc2_application_services::{AXError, AXIsProcessTrusted, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{kCFBooleanTrue, CFRange, CFRetained, CFString, CFType};

use super::watch::{adjust_caret, locate_swap, utf16_range, FieldReader, NoField};

/// How long one Accessibility call may wait for the target app.
const MESSAGING_TIMEOUT_SECS: f32 = 0.5;
/// How long a swap waits for the target app to show the new text.
const VERIFY_WAIT: Duration = Duration::from_millis(120);

const ATTR_FOCUSED: &str = "AXFocusedUIElement";
const ATTR_ROLE: &str = "AXRole";
const ATTR_SUBROLE: &str = "AXSubrole";
const ATTR_VALUE: &str = "AXValue";
const ATTR_SELECTED_RANGE: &str = "AXSelectedTextRange";
const ATTR_SELECTED_TEXT: &str = "AXSelectedText";
const SECURE_ROLE: &str = "AXSecureTextField";
const ATTR_MANUAL_ACCESSIBILITY: &str = "AXManualAccessibility";

/// Whether this process may use the Accessibility API.
pub fn is_trusted() -> bool {
    // SAFETY: plain query with no arguments.
    unsafe { AXIsProcessTrusted() }
}

fn copy_attribute(element: &AXUIElement, name: &'static str) -> Option<CFRetained<CFType>> {
    let mut out: *const CFType = null();
    let attribute = CFString::from_static_str(name);
    // SAFETY: `out` is a valid out-pointer. On success the call hands over one
    // retained reference, which `from_raw` takes.
    let error = unsafe { element.copy_attribute_value(&attribute, NonNull::from(&mut out)) };
    if error != AXError::Success {
        return None;
    }
    let pointer = NonNull::new(out as *mut CFType)?;
    // SAFETY: a "Copy" call returns a +1 reference.
    Some(unsafe { CFRetained::from_raw(pointer) })
}

fn string_attribute(element: &AXUIElement, name: &'static str) -> Option<String> {
    let value = copy_attribute(element, name)?;
    value
        .downcast::<CFString>()
        .ok()
        .map(|string| string.to_string())
}

fn range_attribute(element: &AXUIElement, name: &'static str) -> Option<(usize, usize)> {
    let value = copy_attribute(element, name)?.downcast::<AXValue>().ok()?;
    let mut range = CFRange {
        location: 0,
        length: 0,
    };
    // SAFETY: `range` is a valid CFRange for the CFRange value type.
    let ok = unsafe {
        value.value(
            AXValueType::CFRange,
            NonNull::from(&mut range).cast::<std::ffi::c_void>(),
        )
    };
    (ok && range.location >= 0 && range.length >= 0)
        .then_some((range.location as usize, range.length as usize))
}

fn set_range(element: &AXUIElement, location: usize, length: usize) -> bool {
    let mut range = CFRange {
        location: location as isize,
        length: length as isize,
    };
    // SAFETY: `range` is a valid CFRange for the CFRange value type.
    let Some(value) = (unsafe {
        AXValue::new(
            AXValueType::CFRange,
            NonNull::from(&mut range).cast::<std::ffi::c_void>(),
        )
    }) else {
        return false;
    };
    let attribute = CFString::from_static_str(ATTR_SELECTED_RANGE);
    // SAFETY: both arguments are live CF objects.
    unsafe { element.set_attribute_value(&attribute, (*value).as_ref()) == AXError::Success }
}

fn set_string(element: &AXUIElement, name: &'static str, text: &str) -> bool {
    let attribute = CFString::from_static_str(name);
    let value = CFString::from_str(text);
    // SAFETY: both arguments are live CF objects.
    unsafe { element.set_attribute_value(&attribute, value.as_ref()) == AXError::Success }
}

/// The app in front, found by its process id.
///
/// NSWorkspace knows the frontmost app without asking the app itself. The
/// system-wide `AXFocusedApplication` route asks the app, and Electron and
/// Chromium apps do not answer it until their accessibility support is on.
struct FrontApp {
    element: CFRetained<AXUIElement>,
    bundle_id: String,
}

fn front_app() -> Option<FrontApp> {
    let running = NSWorkspace::sharedWorkspace().frontmostApplication()?;
    let pid = running.processIdentifier();
    if pid <= 0 {
        return None;
    }
    let bundle_id = running
        .bundleIdentifier()
        .map(|id| id.to_string())
        .unwrap_or_else(|| format!("pid {pid}"));
    // SAFETY: `pid` is the id of a running process; the call returns a
    // retained element and does not contact the app.
    let element = unsafe { AXUIElement::new_application(pid) };
    // SAFETY: a plain setter on a live element.
    unsafe { element.set_messaging_timeout(MESSAGING_TIMEOUT_SECS) };
    Some(FrontApp { element, bundle_id })
}

/// The bundle id of the app in front, for log lines. Not user text.
pub fn front_app_name() -> String {
    front_app()
        .map(|app| app.bundle_id)
        .unwrap_or_else(|| "unknown app".to_string())
}

/// Asks the focused app to show its text to Accessibility clients.
///
/// Electron and Chromium apps (Slack, Claude, VS Code, browsers) keep their
/// text hidden from Accessibility until a client sets `AXManualAccessibility`
/// on the app. Without it the watch reads no text in those apps. A native app
/// rejects the attribute, which changes nothing.
///
/// The app builds its text tree some time after the request, so this runs when
/// a recording starts, well before the paste.
pub fn expose_focused_app_text() {
    if !is_trusted() {
        debug!("Accessibility is not allowed: the app was not asked to expose its text");
        return;
    }
    let Some(app) = front_app() else {
        debug!("No app is in front: none was asked to expose its text");
        return;
    };
    // SAFETY: reads a constant that CoreFoundation defines.
    let Some(yes) = (unsafe { kCFBooleanTrue }) else {
        return;
    };
    let attribute = CFString::from_static_str(ATTR_MANUAL_ACCESSIBILITY);
    // SAFETY: both arguments are live CF objects.
    let error = unsafe { app.element.set_attribute_value(&attribute, yes.as_ref()) };
    debug!("Asked {} to expose its text: {:?}", app.bundle_id, error);
}

/// The element that has keyboard focus.
///
/// The front app's own element answers first: an Electron or Chromium app
/// answers it as soon as its accessibility support is on, while the
/// system-wide element can still return nothing. The system-wide element is
/// the fallback.
fn focused_element() -> Option<CFRetained<AXUIElement>> {
    let from_app = front_app().and_then(|app| {
        copy_attribute(&app.element, ATTR_FOCUSED)?
            .downcast::<AXUIElement>()
            .ok()
    });
    let focused = match from_app {
        Some(element) => element,
        None => {
            // SAFETY: no arguments; returns a retained system-wide element.
            let system = unsafe { AXUIElement::new_system_wide() };
            copy_attribute(&system, ATTR_FOCUSED)?
                .downcast::<AXUIElement>()
                .ok()?
        }
    };
    // SAFETY: a plain setter on a live element.
    unsafe { focused.set_messaging_timeout(MESSAGING_TIMEOUT_SECS) };
    Some(focused)
}

fn is_secure_element(element: &AXUIElement) -> bool {
    string_attribute(element, ATTR_ROLE).as_deref() == Some(SECURE_ROLE)
        || string_attribute(element, ATTR_SUBROLE).as_deref() == Some(SECURE_ROLE)
}

/// The text box that had focus when this was called, captured for the watch.
struct AxField {
    element: CFRetained<AXUIElement>,
}

impl FieldReader for AxField {
    fn read(&self) -> Option<String> {
        // The box is gone for the watch when focus moved to another element.
        let focused = focused_element()?;
        if *focused != *self.element {
            return None;
        }
        string_attribute(&self.element, ATTR_VALUE)
    }

    fn is_secure(&self) -> bool {
        is_secure_element(&self.element)
    }
}

/// How long the watch waits for an app that is still building its text tree
/// (an Electron or Chromium app just after it was asked to expose its text).
const FIELD_RETRIES: u32 = 4;
const FIELD_RETRY_WAIT: Duration = Duration::from_millis(150);

/// The focused text box, or why there is none to watch.
pub fn focused_field() -> Result<Box<dyn FieldReader>, NoField> {
    if !is_trusted() {
        return Err(NoField::NotTrusted);
    }
    let mut last = NoField::NoFocusedElement;
    for attempt in 0..=FIELD_RETRIES {
        if attempt > 0 {
            std::thread::sleep(FIELD_RETRY_WAIT);
        }
        let Some(element) = focused_element() else {
            last = NoField::NoFocusedElement;
            continue;
        };
        if is_secure_element(&element) {
            return Err(NoField::SecretField);
        }
        if string_attribute(&element, ATTR_VALUE).is_none() {
            last = NoField::NoReadableText;
            continue;
        }
        return Ok(Box::new(AxField { element }));
    }
    Err(last)
}

/// Replaces the `nth` occurrence of `find` inside the pasted `region` in the
/// focused text box with `replace_with`.
///
/// The word is selected and replaced, so the app records it as typing and the
/// user's own undo works. The caret goes back where it was. The result is
/// checked by reading the box again: an app that ignores the write gives an
/// error, and the caller falls back to the clipboard.
pub fn replace_in_focused(
    region: &str,
    find: &str,
    replace_with: &str,
    nth: usize,
) -> Result<(), String> {
    if !is_trusted() {
        return Err("Accessibility is not allowed for Handy".to_string());
    }
    let element = focused_element().ok_or("No text box has focus")?;
    if is_secure_element(&element) {
        return Err("The focused field is a secret field".to_string());
    }
    let value = string_attribute(&element, ATTR_VALUE).ok_or("The focused box has no text")?;
    let bytes = locate_swap(&value, region, find, nth)
        .ok_or("The pasted text is not in the focused box")?;
    let (location, length) = utf16_range(&value, bytes.clone());
    let caret = range_attribute(&element, ATTR_SELECTED_RANGE);

    let mut expected = value.clone();
    expected.replace_range(bytes, replace_with);

    if !set_range(&element, location, length) {
        return Err("The app did not accept the selection".to_string());
    }
    if !set_string(&element, ATTR_SELECTED_TEXT, replace_with) {
        warn!("The app did not accept the replacement text");
        return Err("The app did not accept the new text".to_string());
    }

    std::thread::sleep(VERIFY_WAIT);
    if string_attribute(&element, ATTR_VALUE).as_deref() != Some(expected.as_str()) {
        return Err("The app did not change the text".to_string());
    }

    if let Some(old) = caret {
        let new_len = replace_with.encode_utf16().count();
        let (at, len) = adjust_caret(old, (location, length), new_len);
        set_range(&element, at, len);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not trusted (the usual state of a test binary) means "no" everywhere,
    /// never a panic and never a hang.
    #[test]
    fn without_accessibility_everything_says_no() {
        if is_trusted() {
            return; // a trusted terminal would read a real window: skip
        }
        assert_eq!(focused_field().err(), Some(NoField::NotTrusted));
        assert!(replace_in_focused("a b", "b", "c", 0).is_err());
    }

    /// Manual: `cargo test --lib ax_smoke -- --ignored --nocapture` from a
    /// terminal that has the Accessibility permission, with a text box focused.
    #[test]
    #[ignore]
    fn ax_smoke() {
        println!("trusted: {}", is_trusted());
        println!("front app: {}", front_app_name());
        match focused_field() {
            Ok(field) => println!(
                "secure: {}, value is {} chars",
                field.is_secure(),
                field.read().map(|v| v.chars().count()).unwrap_or(0)
            ),
            Err(why) => println!("no field: {}", why.reason()),
        }
    }
}

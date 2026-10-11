//! Windows UI Automation adapter: reads the focused text box for the watch and
//! writes a swap into it.
//!
//! Every call here talks to another process over UI Automation, so none may
//! run on the main thread. The watch thread and the swap command call them.
//!
//! Windows asks no permission for this. An app that runs elevated (as
//! administrator) does not answer a normal process, and reads as "no element
//! has focus".

use std::time::Duration;

use enigo::{Enigo, Keyboard, Settings};
use log::{debug, warn};
use windows::core::{IUnknown, Interface, BSTR, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationElement, IUIAutomationTextPattern,
    IUIAutomationTextRange, IUIAutomationValuePattern, TextPatternRangeEndpoint_End,
    TextPatternRangeEndpoint_Start, UIA_DocumentControlTypeId, UIA_EditControlTypeId,
    UIA_TextPatternId, UIA_ValuePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use super::watch::{locate_swap, FieldReader, NoField};

/// How long one UI Automation call may wait for the target app.
const CONNECTION_TIMEOUT_MS: u32 = 1000;
/// How long a swap waits for the target app to show the new text.
const VERIFY_WAIT: Duration = Duration::from_millis(150);
/// How long the watch waits for an app that is still building its text tree
/// (a Chromium or Electron app just after it was asked to expose its text).
const FIELD_RETRIES: u32 = 4;
const FIELD_RETRY_WAIT: Duration = Duration::from_millis(150);

/// Makes COM usable on the calling thread. A thread that already has COM in
/// another mode keeps it: UI Automation works from either apartment.
fn init_com() {
    // SAFETY: no reserved pointer; the call tolerates repeats on one thread.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}

/// A UI Automation client with a short per-call timeout, so an app that does
/// not answer cannot stall the watch.
fn automation() -> Option<IUIAutomation> {
    init_com();
    // SAFETY: in-process activation of the documented UI Automation class.
    let automation: IUIAutomation =
        match unsafe { CoCreateInstance(&CUIAutomation8, None::<&IUnknown>, CLSCTX_INPROC_SERVER) }
        {
            Ok(automation) => automation,
            Err(e) => {
                debug!("UI Automation is not available: {e}");
                return None;
            }
        };
    if let Ok(v2) = automation.cast::<IUIAutomation2>() {
        // SAFETY: a plain setter on a live client.
        if let Err(e) = unsafe { v2.SetConnectionTimeout(CONNECTION_TIMEOUT_MS) } {
            debug!("UI Automation keeps its default timeout: {e}");
        }
    }
    Some(automation)
}

fn focused_element(automation: &IUIAutomation) -> Option<IUIAutomationElement> {
    // SAFETY: a plain query on a live client.
    unsafe { automation.GetFocusedElement() }.ok()
}

fn is_secure_element(element: &IUIAutomationElement) -> bool {
    // SAFETY: a property read on a live element.
    unsafe { element.CurrentIsPassword() }
        .map(|secure| secure.as_bool())
        .unwrap_or(false)
}

fn text_pattern(element: &IUIAutomationElement) -> Option<IUIAutomationTextPattern> {
    // SAFETY: a pattern query on a live element; an error means "unsupported".
    unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }.ok()
}

fn value_pattern(element: &IUIAutomationElement) -> Option<IUIAutomationValuePattern> {
    // SAFETY: a pattern query on a live element; an error means "unsupported".
    unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }.ok()
}

fn range_text(range: &IUIAutomationTextRange) -> Option<String> {
    // SAFETY: a read on a live range; -1 asks for the whole text.
    unsafe { range.GetText(-1) }
        .ok()
        .map(|text| normalise_newlines(text.to_string()))
}

/// Windows boxes store line ends as `\r\n`; Handy's text uses `\n`.
fn normalise_newlines(text: String) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text
    }
}

/// The whole text of an element, read the way the element supports: the text
/// pattern first (edit boxes, documents, Chromium and Electron apps), then the
/// value of a plain edit box.
fn element_text(element: &IUIAutomationElement) -> Option<String> {
    if let Some(text) = text_pattern(element) {
        // SAFETY: a read on a live pattern.
        if let Ok(document) = unsafe { text.DocumentRange() } {
            if let Some(value) = range_text(&document) {
                return Some(value);
            }
        }
    }
    // SAFETY: a property read on a live element.
    let control = unsafe { element.CurrentControlType() }.ok()?;
    if control != UIA_EditControlTypeId && control != UIA_DocumentControlTypeId {
        return None;
    }
    let value = value_pattern(element)?;
    // SAFETY: a read on a live pattern.
    unsafe { value.CurrentValue() }
        .ok()
        .map(|text| normalise_newlines(text.to_string()))
}

/// The name of the program in front (its executable), for log lines. Not
/// user text.
pub fn front_app_name() -> String {
    const UNKNOWN: &str = "unknown app";
    // SAFETY: plain Win32 queries; the buffer outlives the call that fills it
    // and the process handle is closed on every path after it opens.
    unsafe {
        let window = GetForegroundWindow();
        if window.0.is_null() {
            return UNKNOWN.to_string();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, Some(&mut pid as *mut u32));
        if pid == 0 {
            return UNKNOWN.to_string();
        }
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return format!("pid {pid}");
        };
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let path = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
        .ok()
        .map(|_| String::from_utf16_lossy(&buffer[..size as usize]));
        let _ = CloseHandle(process);
        path.and_then(|path| path.rsplit(['\\', '/']).next().map(str::to_string))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("pid {pid}"))
    }
}

/// Asks the app in front to show its text to UI Automation clients.
///
/// A Chromium or Electron app (Slack, Claude, VS Code, browsers) builds its
/// accessibility tree only once a client asks its window for an element. The
/// tree takes a moment to appear, so this runs when a recording starts, well
/// before the paste.
pub fn expose_focused_app_text() {
    let Some(automation) = automation() else {
        return;
    };
    // SAFETY: plain queries on a live client and the window in front.
    unsafe {
        let window = GetForegroundWindow();
        if window.0.is_null() {
            debug!("No window is in front: none was asked to expose its text");
            return;
        }
        let asked = automation.ElementFromHandle(window).is_ok();
        let focused = automation.GetFocusedElement().ok();
        let has_text = focused.as_ref().and_then(text_pattern).is_some();
        debug!(
            "Asked {} to expose its text: window answered {asked}, focused text box {has_text}",
            front_app_name()
        );
    }
}

/// The text box that had focus when this was called, captured for the watch.
struct UiaField {
    automation: IUIAutomation,
    element: IUIAutomationElement,
}

impl FieldReader for UiaField {
    fn read(&self) -> Option<String> {
        // The box is gone for the watch when focus moved to another element.
        let focused = focused_element(&self.automation)?;
        // SAFETY: a comparison of two live elements.
        let same = unsafe { self.automation.CompareElements(&self.element, &focused) }
            .map(|same| same.as_bool())
            .unwrap_or(false);
        if !same {
            return None;
        }
        element_text(&focused)
    }

    fn is_secure(&self) -> bool {
        is_secure_element(&self.element)
    }
}

/// The focused text box, or why there is none to watch.
pub fn focused_field() -> Result<Box<dyn FieldReader>, NoField> {
    let automation = automation().ok_or(NoField::Unavailable)?;
    let mut last = NoField::NoFocusedElement;
    for attempt in 0..=FIELD_RETRIES {
        if attempt > 0 {
            std::thread::sleep(FIELD_RETRY_WAIT);
        }
        let Some(element) = focused_element(&automation) else {
            last = NoField::NoFocusedElement;
            continue;
        };
        if is_secure_element(&element) {
            return Err(NoField::SecretField);
        }
        if element_text(&element).is_none() {
            last = NoField::NoReadableText;
            continue;
        }
        return Ok(Box::new(UiaField {
            automation,
            element,
        }));
    }
    Err(last)
}

/// The text the app has selected in the box, when it says.
fn selection_text(text: &IUIAutomationTextPattern) -> Option<String> {
    // SAFETY: reads on a live pattern and the array it returns.
    unsafe {
        let ranges = text.GetSelection().ok()?;
        if ranges.Length().ok()? < 1 {
            return None;
        }
        range_text(&ranges.GetElement(0).ok()?)
    }
}

/// The range of the `nth` occurrence of `find` inside the last occurrence of
/// `region` in the box, found by the app's own text search so that its line
/// ends and hidden characters do not throw the count off.
fn find_occurrence(
    text: &IUIAutomationTextPattern,
    region: &str,
    find: &str,
    nth: usize,
) -> Result<IUIAutomationTextRange, String> {
    // SAFETY: reads and moves on live ranges; each call checks its result.
    unsafe {
        let document = text
            .DocumentRange()
            .map_err(|e| format!("The focused box has no text range: {e}"))?;
        let region_range = document
            .FindText(&BSTR::from(region), true, false)
            .map_err(|_| "The pasted text is not in the focused box".to_string())?;
        let search = region_range
            .Clone()
            .map_err(|e| format!("The app did not give a search range: {e}"))?;
        let mut found = None;
        for _ in 0..=nth {
            let hit = search
                .FindText(&BSTR::from(find), false, false)
                .map_err(|_| "The word is not in the pasted text".to_string())?;
            search
                .MoveEndpointByRange(
                    TextPatternRangeEndpoint_Start,
                    &hit,
                    TextPatternRangeEndpoint_End,
                )
                .map_err(|e| format!("The app did not move the search range: {e}"))?;
            found = Some(hit);
        }
        found.ok_or_else(|| "The word is not in the pasted text".to_string())
    }
}

/// Replaces the `nth` occurrence of `find` inside the pasted `region` in the
/// focused text box with `replace_with`.
///
/// The word is selected through the text pattern and the new text is typed
/// over it, so the app records it as typing and the user's own undo works.
/// The app's selection is read back before anything is typed: an app that
/// did not select the word gets no keystrokes. The result is checked by
/// reading the box again; an app that ignored the typing gives an error, and
/// the caller falls back to the clipboard. The caret ends after the new word.
pub fn replace_in_focused(
    region: &str,
    find: &str,
    replace_with: &str,
    nth: usize,
) -> Result<(), String> {
    let automation = automation().ok_or("UI Automation is not available")?;
    let element = focused_element(&automation).ok_or("No text box has focus")?;
    if is_secure_element(&element) {
        return Err("The focused field is a secret field".to_string());
    }
    let value = element_text(&element).ok_or("The focused box has no text")?;
    let bytes = locate_swap(&value, region, find, nth)
        .ok_or("The pasted text is not in the focused box")?;
    let mut expected = value.clone();
    expected.replace_range(bytes, replace_with);

    let text = text_pattern(&element).ok_or("The focused box does not support text selection")?;
    let target = find_occurrence(&text, region, find, nth)?;
    // SAFETY: a selection request on a live range.
    unsafe { target.Select() }.map_err(|e| format!("The app did not accept the selection: {e}"))?;
    if selection_text(&text).as_deref() != Some(find) {
        warn!("The app did not select the word to swap");
        return Err("The app did not select the word".to_string());
    }

    let mut keyboard = Enigo::new(&Settings::default())
        .map_err(|e| format!("Keyboard input is not available: {e}"))?;
    keyboard
        .text(replace_with)
        .map_err(|e| format!("Could not type the new text: {e}"))?;

    std::thread::sleep(VERIFY_WAIT);
    if element_text(&element).as_deref() != Some(expected.as_str()) {
        return Err("The app did not change the text".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Manual: `cargo test --lib uia_smoke -- --ignored --nocapture` with a
    /// text box focused in another window.
    #[test]
    #[ignore]
    fn uia_smoke() {
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

    #[test]
    fn newlines_are_normalised() {
        assert_eq!(normalise_newlines("a\r\nb\rc".to_string()), "a\nb\nc");
        assert_eq!(normalise_newlines("a\nb".to_string()), "a\nb");
    }
}

//! The window that shows learning notices (Task 8).
//!
//! A small always-on-top window labelled `learning_notice`, above the
//! recording overlay. It is created on the first notice and reused. On macOS it
//! is a non-activating NSPanel (it never takes focus) that still takes mouse
//! clicks, so Undo and Swap work without pulling the user out of the text box.
//!
//! Who does what:
//! - Rust decides *that* the window shows and where it sits. `dispatch` is
//!   called for every notice.
//! - The page (`src/notice/`) decides *what* shows and for how long. It tells
//!   Rust two things with plain events: `learning-notice-ready` once it listens,
//!   and `learning-notice-idle` (with the number of notices it has received)
//!   when its queue is empty. Rust then hides the window.
//! - A 30 s timer hides the window anyway if the page never says idle.
//!
//! A new window loads its page after `dispatch` returns, so the first notices
//! would be lost. While the page is not ready they wait in `pending` and are
//! sent to the window alone when it says ready. The settings window gets every
//! notice at once.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::Duration;

use log::{debug, warn};
use tauri::{AppHandle, Listener, Manager};
use tauri_specta::Event;

use super::notice::LearningNotice;
use crate::overlay::{get_monitor_with_cursor, OVERLAY_BOTTOM_OFFSET, OVERLAY_HEIGHT};
use crate::settings::{self, OverlayPosition, OverlayStyle};

#[cfg(target_os = "macos")]
use tauri::WebviewUrl;
#[cfg(target_os = "macos")]
use tauri_nspanel::{tauri_panel, CollectionBehavior, PanelBuilder, PanelLevel, StyleMask};

#[cfg(not(target_os = "macos"))]
use tauri::WebviewWindowBuilder;

/// Window label. The capability file `capabilities/default.json` lists it.
pub const NOTICE_LABEL: &str = "learning_notice";
/// The page emits this once its listener for notices is in place, with the
/// number of notices it has received so far.
const READY_EVENT: &str = "learning-notice-ready";
/// The page emits this with its received-notice count when its queue is empty.
const IDLE_EVENT: &str = "learning-notice-idle";

/// Window size in logical points. The card in `Notice.css` fills it.
const NOTICE_WIDTH: f64 = 380.0;
const NOTICE_HEIGHT: f64 = 64.0;
/// Space between the notice and the recording overlay below it.
const GAP_ABOVE_OVERLAY: f64 = 8.0;
/// The window is hidden this long after the last notice even if the page
/// never says it is idle.
const FALLBACK_HIDE: Duration = Duration::from_secs(30);

#[cfg(target_os = "macos")]
tauri_panel! {
    panel!(LearningNoticePanel {
        config: {
            can_become_key_window: false,
            is_floating_panel: true
        }
    })
}

// ---- delivery state ---------------------------------------------------------

/// What the window has been sent. Pure, so tests need no window.
#[derive(Default)]
struct Delivery {
    /// The page listens for notices.
    ready: bool,
    /// Notices sent to the page, or held for it.
    sent: u64,
    /// Notices held until the page is ready.
    pending: Vec<LearningNotice>,
}

impl Delivery {
    const fn new() -> Self {
        Self {
            ready: false,
            sent: 0,
            pending: Vec::new(),
        }
    }

    /// Counts a notice. Returns whether the page can take it now; if not, the
    /// notice is held.
    fn accept(&mut self, notice: &LearningNotice) -> bool {
        self.sent += 1;
        if !self.ready {
            self.pending.push(notice.clone());
        }
        self.ready
    }

    /// The page listens and has already seen `seen` notices (zero for a page
    /// that just loaded). Returns the notices held for it. The count restarts
    /// from what the page has seen, so a page that loads again stays in step.
    fn mark_ready(&mut self, seen: u64) -> Vec<LearningNotice> {
        self.ready = true;
        let pending = std::mem::take(&mut self.pending);
        self.sent = seen + pending.len() as u64;
        pending
    }

    /// The window could not be built: drop what waits for it.
    fn abandon(&mut self) {
        self.ready = false;
        self.sent = 0;
        self.pending.clear();
    }

    /// Whether the page has seen every notice sent to it.
    fn is_idle(&self, seen: u64) -> bool {
        seen >= self.sent
    }
}

static DELIVERY: Mutex<Delivery> = Mutex::new(Delivery::new());
/// Bumped on every show and every hide, so a timer from an earlier show does
/// not hide a later one.
static SHOW_GENERATION: AtomicU64 = AtomicU64::new(0);
static LISTENERS: Once = Once::new();

fn delivery() -> MutexGuard<'static, Delivery> {
    DELIVERY.lock().unwrap_or_else(|e| e.into_inner())
}

// ---- public entry -----------------------------------------------------------

/// Sends a notice to the windows and shows the notice window. Called for every
/// notice, from any thread.
pub fn dispatch(app: &AppHandle, notice: LearningNotice) {
    {
        // The lock covers the send so a page turning ready cannot slip between
        // the decision and the emit.
        let mut d = delivery();
        let ready = d.accept(&notice);
        let sent = if ready {
            notice.emit(app)
        } else {
            // The notice window cannot hear it yet; it gets it from `pending`.
            notice.emit_to(app, "main")
        };
        if let Err(e) = sent {
            warn!("Failed to emit learning notice: {e}");
        }
    }
    show(app);
}

fn show(app: &AppHandle) {
    let generation = SHOW_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    let handle = app.clone();
    if let Err(e) = app.run_on_main_thread(move || place_and_show(&handle)) {
        warn!("Could not queue the notice window: {e}");
    }

    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(FALLBACK_HIDE);
        if SHOW_GENERATION.load(Ordering::SeqCst) == generation {
            debug!("Hiding the notice window: the page never said it was idle");
            hide(&handle);
        }
    });
}

fn hide(app: &AppHandle) {
    SHOW_GENERATION.fetch_add(1, Ordering::SeqCst);
    let handle = app.clone();
    let queued = app.run_on_main_thread(move || {
        if let Some(window) = handle.get_webview_window(NOTICE_LABEL) {
            if let Err(e) = window.hide() {
                warn!("Could not hide the notice window: {e}");
            }
        }
    });
    if let Err(e) = queued {
        warn!("Could not queue hiding the notice window: {e}");
    }
}

// ---- page events ------------------------------------------------------------

fn register_listeners(app: &AppHandle) {
    LISTENERS.call_once(|| {
        let handle = app.clone();
        app.listen(READY_EVENT, move |event| {
            match serde_json::from_str::<u64>(event.payload()) {
                Ok(seen) => on_ready(&handle, seen),
                Err(e) => warn!("Bad payload from the notice window: {e}"),
            }
        });
        let handle = app.clone();
        app.listen(IDLE_EVENT, move |event| {
            match serde_json::from_str::<u64>(event.payload()) {
                Ok(seen) => on_idle(&handle, seen),
                Err(e) => warn!("Bad payload from the notice window: {e}"),
            }
        });
    });
}

fn on_ready(app: &AppHandle, seen: u64) {
    // Held under the lock so a notice that arrives now cannot pass them.
    let mut d = delivery();
    for notice in d.mark_ready(seen) {
        if let Err(e) = notice.emit_to(app, NOTICE_LABEL) {
            warn!("Failed to send a held learning notice: {e}");
        }
    }
}

fn on_idle(app: &AppHandle, seen: u64) {
    let idle = delivery().is_idle(seen);
    if idle {
        hide(app);
    }
}

// ---- window -----------------------------------------------------------------

/// Where the notice window sits: bottom centre of the monitor with the cursor,
/// above the recording overlay when that is at the bottom.
fn notice_position(app: &AppHandle) -> Option<(f64, f64)> {
    let monitor = get_monitor_with_cursor(app)?;
    let scale = monitor.scale_factor();
    let monitor_x = monitor.position().x as f64 / scale;
    let monitor_width = monitor.size().width as f64 / scale;

    #[cfg(target_os = "macos")]
    let bottom = {
        // The work area follows the Dock, like the overlay does.
        let work = monitor.work_area();
        (work.position.y as f64 + work.size.height as f64) / scale
    };
    #[cfg(not(target_os = "macos"))]
    let bottom = (monitor.position().y as f64 + monitor.size().height as f64) / scale;

    let settings = settings::get_settings(app);
    let overlay_below = settings.overlay_position == OverlayPosition::Bottom
        && settings.overlay_style != OverlayStyle::None;
    Some(layout(monitor_x, monitor_width, bottom, overlay_below))
}

/// The top-left corner of the window, in logical points.
fn layout(monitor_x: f64, monitor_width: f64, bottom: f64, overlay_below: bool) -> (f64, f64) {
    let reserved = if overlay_below {
        OVERLAY_HEIGHT + GAP_ABOVE_OVERLAY
    } else {
        0.0
    };
    let x = monitor_x + (monitor_width - NOTICE_WIDTH) / 2.0;
    let y = bottom - OVERLAY_BOTTOM_OFFSET - reserved - NOTICE_HEIGHT;
    (x, y)
}

/// Runs on the main thread.
fn place_and_show(app: &AppHandle) {
    let Some(window) = ensure_window(app) else {
        return;
    };
    if let Some((x, y)) = notice_position(app) {
        let _ = window.set_position(tauri::Position::Logical(tauri::LogicalPosition { x, y }));
    }
    if let Err(e) = window.show() {
        warn!("Could not show the notice window: {e}");
    }
}

fn ensure_window(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    if let Some(window) = app.get_webview_window(NOTICE_LABEL) {
        return Some(window);
    }
    register_listeners(app);
    match create_window(app) {
        Ok(()) => app.get_webview_window(NOTICE_LABEL),
        Err(e) => {
            warn!("Failed to create the notice window: {e}");
            delivery().abandon();
            None
        }
    }
}

/// macOS: a non-activating panel above the overlay level, visible on every
/// space. It takes clicks (`accept_first_mouse`) but can never become key.
#[cfg(target_os = "macos")]
fn create_window(app: &AppHandle) -> tauri::Result<()> {
    let (x, y) = notice_position(app).unwrap_or((0.0, 0.0));
    let panel = PanelBuilder::<_, LearningNoticePanel>::new(app, NOTICE_LABEL)
        .url(WebviewUrl::App("src/notice/index.html".into()))
        .title("Learning notice")
        .position(tauri::Position::Logical(tauri::LogicalPosition { x, y }))
        .level(PanelLevel::Status)
        .size(tauri::Size::Logical(tauri::LogicalSize {
            width: NOTICE_WIDTH,
            height: NOTICE_HEIGHT,
        }))
        .has_shadow(false)
        .transparent(true)
        .no_activate(true)
        .hides_on_deactivate(false)
        .corner_radius(0.0)
        .style_mask(StyleMask::empty().borderless().nonactivating_panel())
        .with_window(|w| {
            let mut w = w
                .decorations(false)
                .transparent(true)
                .focusable(false)
                .resizable(false)
                .accept_first_mouse(true);
            if let Some(data_dir) = crate::portable::data_dir() {
                w = w.data_directory(data_dir.join("webview"));
            }
            w
        })
        .collection_behavior(
            CollectionBehavior::new()
                .can_join_all_spaces()
                .full_screen_auxiliary(),
        )
        .build()?;
    panel.hide();
    Ok(())
}

/// Other platforms: a plain always-on-top window that does not take focus.
#[cfg(not(target_os = "macos"))]
fn create_window(app: &AppHandle) -> tauri::Result<()> {
    let mut builder = WebviewWindowBuilder::new(
        app,
        NOTICE_LABEL,
        tauri::WebviewUrl::App("src/notice/index.html".into()),
    )
    .title("Learning notice")
    .resizable(false)
    .inner_size(NOTICE_WIDTH, NOTICE_HEIGHT)
    .shadow(false)
    .maximizable(false)
    .minimizable(false)
    .closable(false)
    .accept_first_mouse(true)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .transparent(true)
    .focusable(false)
    .focused(false)
    .visible(false);
    if let Some(data_dir) = crate::portable::data_dir() {
        builder = builder.data_directory(data_dir.join("webview"));
    }
    builder.build()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(alternative: &str) -> LearningNotice {
        LearningNotice::SwapFailed {
            alternative: alternative.into(),
        }
    }

    #[test]
    fn notices_wait_until_the_page_is_ready_and_then_arrive_once_in_order() {
        let mut d = Delivery::new();
        assert!(!d.accept(&failed("a")));
        assert!(!d.accept(&failed("b")));
        assert_eq!(d.mark_ready(0), vec![failed("a"), failed("b")]);
        // After that notices go straight out and nothing is held.
        assert!(d.accept(&failed("c")));
        assert!(d.pending.is_empty());
    }

    #[test]
    fn the_window_is_idle_only_when_the_page_has_seen_every_notice() {
        let mut d = Delivery::new();
        d.mark_ready(0);
        d.accept(&failed("a"));
        d.accept(&failed("b"));
        assert!(!d.is_idle(1));
        assert!(d.is_idle(2));
    }

    #[test]
    fn a_page_that_loads_again_restarts_the_count() {
        let mut d = Delivery::new();
        d.mark_ready(0);
        d.accept(&failed("a"));
        d.accept(&failed("b"));
        // The page reloaded: it has seen nothing yet.
        assert!(d.mark_ready(0).is_empty());
        assert!(d.is_idle(0));
        // A page that only re-registered keeps its own count.
        assert!(d.mark_ready(5).is_empty());
        assert!(d.is_idle(5));
        d.accept(&failed("c"));
        assert!(!d.is_idle(5));
        assert!(d.is_idle(6));
    }

    #[test]
    fn a_failed_window_drops_what_waited_for_it() {
        let mut d = Delivery::new();
        d.accept(&failed("a"));
        d.abandon();
        assert!(d.pending.is_empty());
        assert!(d.is_idle(0));
        assert!(!d.ready);
    }

    #[test]
    fn the_notice_sits_above_the_overlay_only_when_the_overlay_is_at_the_bottom() {
        let (x, with_overlay) = layout(0.0, 1440.0, 900.0, true);
        assert_eq!(x, (1440.0 - NOTICE_WIDTH) / 2.0);
        let (_, without_overlay) = layout(0.0, 1440.0, 900.0, false);
        assert_eq!(
            without_overlay - with_overlay,
            OVERLAY_HEIGHT + GAP_ABOVE_OVERLAY
        );
        // The window never overlaps the overlay's top edge.
        let overlay_top = 900.0 - OVERLAY_BOTTOM_OFFSET - OVERLAY_HEIGHT;
        assert!(with_overlay + NOTICE_HEIGHT <= overlay_top);
    }
}

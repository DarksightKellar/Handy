//! The watch loop: after a paste, read the text box now and then and hand the
//! readings to the pure [`Watch`] state machine. This file knows nothing about
//! Accessibility; a [`FieldReader`] supplies the readings, so the loop runs in
//! tests with a fake. Also holds the pure text arithmetic of a swap.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::debug;

use super::core::region::{Watch, WatchStep};

/// Waits this long after a paste before the first reading, so the target app
/// has put the pasted text in the box.
pub const SETTLE: Duration = Duration::from_millis(250);
/// Time between readings.
pub const POLL: Duration = Duration::from_millis(500);

/// A text box that can be read.
pub trait FieldReader {
    /// The whole text of the field. `None` when the element is gone or the
    /// focus moved away from it.
    fn read(&self) -> Option<String>;
    /// True when the field holds secret text (a password box).
    fn is_secure(&self) -> bool;
}

/// How a watch runs. The defaults are the production timings.
#[derive(Clone, Copy, Debug)]
pub struct WatchTiming {
    pub settle: Duration,
    pub poll: Duration,
    pub watch_for: Duration,
}

impl WatchTiming {
    pub fn new(watch_for: Duration) -> Self {
        Self {
            settle: SETTLE,
            poll: POLL,
            watch_for,
        }
    }
}

/// Sleeps for `total`, in short slices, and stops early when `stop` is set.
fn sleep_unless_stopped(total: Duration, stop: &AtomicBool) {
    const SLICE: Duration = Duration::from_millis(25);
    let end = Instant::now() + total;
    while !stop.load(Ordering::SeqCst) {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(SLICE));
    }
}

/// Runs one watch on the calling thread and returns what it learned from.
///
/// `Some(region)` is the last good reading of the pasted text: the text as
/// edited, or exactly `pasted` when it survived unchanged. `None` means there
/// is nothing to learn from: Secure Input or a secret field (the watch stops
/// at once and reads nothing more), a paste that cannot be found in the box,
/// or a box that was never readable.
///
/// Setting `stop` ends the watch early with whatever was last seen; no new
/// reading is taken after it is set.
pub fn run_watch(
    reader: &dyn FieldReader,
    pasted: &str,
    timing: WatchTiming,
    secure_now: &dyn Fn() -> bool,
    stop: &AtomicBool,
) -> Option<String> {
    sleep_unless_stopped(timing.settle, stop);
    if stop.load(Ordering::SeqCst) || secure_now() || reader.is_secure() {
        return None;
    }
    let baseline = reader.read()?;
    let mut watch = Watch::start(&baseline, pasted)?;

    let deadline = Instant::now() + timing.watch_for;
    loop {
        sleep_unless_stopped(timing.poll, stop);
        if stop.load(Ordering::SeqCst) || Instant::now() >= deadline {
            break;
        }
        if secure_now() || reader.is_secure() {
            debug!("Watch stopped: secure input");
            return None;
        }
        let reading = reader.read();
        match watch.feed(reading.as_deref()) {
            WatchStep::Continue => {}
            WatchStep::Finish(region) => return region.or_else(|| unchanged(&watch, pasted)),
        }
    }
    let kept = unchanged(&watch, pasted);
    watch.finish().or(kept)
}

/// `Some(pasted)` when the watch saw the text survive as pasted.
fn unchanged(watch: &Watch, pasted: &str) -> Option<String> {
    watch.is_unchanged().then(|| pasted.to_string())
}

/// A running watch thread.
pub struct WatchHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl WatchHandle {
    /// Asks the watch to end. It returns at once; `done` runs on the watch
    /// thread.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Ends the watch and waits until `done` has run.
    pub fn finish_now(mut self) {
        self.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Starts a watch thread.
///
/// `prepare` runs first on the new thread and returns the field to read, so
/// every Accessibility call stays off the caller's thread, and a reader never
/// has to be `Send`. `done` runs once on the same thread with the result of
/// [`run_watch`].
pub fn spawn_watch_with(
    prepare: impl FnOnce() -> Option<Box<dyn FieldReader>> + Send + 'static,
    pasted: String,
    timing: WatchTiming,
    secure_now: impl Fn() -> bool + Send + 'static,
    done: impl FnOnce(Option<String>) + Send + 'static,
) -> WatchHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let join = std::thread::Builder::new()
        .name("learning-watch".into())
        .spawn(move || {
            let result = prepare().and_then(|reader| {
                run_watch(&*reader, &pasted, timing, &secure_now, &stop_for_thread)
            });
            done(result);
        })
        .ok();
    WatchHandle { stop, join }
}

/// Starts the watch of a new paste after the watch of the paste before it.
///
/// The previous watch is told to stop at once. The new thread captures its
/// field first (the new paste landed there just now), then waits until the
/// previous watch has run its `done`. So the first watch finishes learning
/// before the second reads anything, and the two never mix their texts.
pub fn spawn_watch_after(
    previous: Option<WatchHandle>,
    capture: impl FnOnce() -> Option<Box<dyn FieldReader>> + Send + 'static,
    pasted: String,
    timing: WatchTiming,
    secure_now: impl Fn() -> bool + Send + 'static,
    done: impl FnOnce(Option<String>) + Send + 'static,
) -> WatchHandle {
    if let Some(previous) = &previous {
        previous.stop();
    }
    spawn_watch_with(
        move || {
            let field = capture();
            if let Some(previous) = previous {
                previous.finish_now();
            }
            field
        },
        pasted,
        timing,
        secure_now,
        done,
    )
}

/// Starts a watch on an already captured field. The app captures its field on the
/// watch thread instead (see [`spawn_watch_after`]); this form serves tests and
/// callers that already hold a `Send` reader.
#[cfg_attr(not(test), allow(dead_code))]
pub fn spawn_watch(
    reader: Box<dyn FieldReader + Send>,
    pasted: String,
    timing: WatchTiming,
    secure_now: impl Fn() -> bool + Send + 'static,
    done: impl FnOnce(Option<String>) + Send + 'static,
) -> WatchHandle {
    spawn_watch_with(
        move || Some(reader as Box<dyn FieldReader>),
        pasted,
        timing,
        secure_now,
        done,
    )
}

// ---- swap text arithmetic ---------------------------------------------------

/// Where to write a swap: the byte range of the `nth` occurrence of `find`
/// inside the last occurrence of `region` in `value`. `None` when the pasted
/// region is not in the field any more.
pub fn locate_swap(value: &str, region: &str, find: &str, nth: usize) -> Option<Range<usize>> {
    if find.is_empty() {
        return None;
    }
    let at = value.rfind(region)?;
    let (offset, _) = region.match_indices(find).nth(nth)?;
    let start = at + offset;
    Some(start..start + find.len())
}

/// A byte range of `text` as a UTF-16 `(location, length)`, which is how
/// Accessibility counts characters.
pub fn utf16_range(text: &str, bytes: Range<usize>) -> (usize, usize) {
    let location = text[..bytes.start].encode_utf16().count();
    let length = text[bytes.start..bytes.end].encode_utf16().count();
    (location, length)
}

/// Where the user's selection (or caret) should be after the text at
/// `replaced` (UTF-16 `(location, length)`) became `new_len` units long. All
/// ranges are `(location, length)` in UTF-16 units. A position after the
/// replacement moves with it; one inside it goes to the end of the new text.
pub fn adjust_caret(
    selection: (usize, usize),
    replaced: (usize, usize),
    new_len: usize,
) -> (usize, usize) {
    let (r_start, r_end) = (replaced.0, replaced.0 + replaced.1);
    let map = |p: usize| {
        if p <= r_start {
            p
        } else if p >= r_end {
            p - replaced.1 + new_len
        } else {
            r_start + new_len
        }
    };
    let (start, end) = (map(selection.0), map(selection.0 + selection.1));
    (start, end - start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    /// Gives its readings in order, then reports the element as gone.
    struct Script {
        readings: Vec<Option<String>>,
        next: AtomicUsize,
        secure: bool,
    }

    impl Script {
        fn new(readings: &[&str]) -> Self {
            Self {
                readings: readings.iter().map(|r| Some(r.to_string())).collect(),
                next: AtomicUsize::new(0),
                secure: false,
            }
        }
    }

    impl FieldReader for Script {
        fn read(&self) -> Option<String> {
            let i = self.next.fetch_add(1, Ordering::SeqCst);
            self.readings.get(i).cloned().flatten()
        }
        fn is_secure(&self) -> bool {
            self.secure
        }
    }

    fn fast(watch_for_ms: u64) -> WatchTiming {
        WatchTiming {
            settle: Duration::from_millis(1),
            poll: Duration::from_millis(2),
            watch_for: Duration::from_millis(watch_for_ms),
        }
    }

    fn run(script: &Script, pasted: &str, watch_for_ms: u64) -> Option<String> {
        run_watch(
            script,
            pasted,
            fast(watch_for_ms),
            &|| false,
            &AtomicBool::new(false),
        )
    }

    /// `spawn_watch` end to end, with the plan's readings.
    #[test]
    fn spawned_watch_reports_the_last_good_reading_after_the_box_clears() {
        let script = Script::new(&["send it to Calvin", "send it to Kelvin", "", ""]);
        let (tx, rx) = mpsc::channel();
        let handle = spawn_watch(
            Box::new(script),
            "send it to Calvin".to_string(),
            fast(5_000),
            || false,
            move |result| tx.send(result).unwrap(),
        );
        let result = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        handle.finish_now();
        assert_eq!(result.as_deref(), Some("send it to Kelvin"));
    }

    #[test]
    fn rf5_secure_input_stops_the_watch_without_a_reading() {
        let script = Script::new(&["send it to Calvin", "send it to Kelvin", "", ""]);
        let result = run_watch(
            &script,
            "send it to Calvin",
            fast(5_000),
            &|| true,
            &AtomicBool::new(false),
        );
        assert_eq!(result, None);
        assert_eq!(
            script.next.load(Ordering::SeqCst),
            0,
            "nothing may be read under Secure Input"
        );
    }

    #[test]
    fn rf5_a_secret_field_stops_the_watch() {
        let mut script = Script::new(&["hunter2"]);
        script.secure = true;
        assert_eq!(run(&script, "hunter2", 5_000), None);
        assert_eq!(script.next.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn secure_input_that_starts_mid_watch_discards_what_was_seen() {
        let script = Script::new(&["a b c", "a x c", "a x c", "a x c"]);
        let polls = Cell::new(0);
        let result = run_watch(
            &script,
            "a b c",
            fast(5_000),
            &|| {
                polls.set(polls.get() + 1);
                polls.get() > 3 // secure from the third check on
            },
            &AtomicBool::new(false),
        );
        assert_eq!(result, None);
    }

    #[test]
    fn rf1_send_clears_the_box_but_the_edit_is_learned() {
        let script = Script::new(&["send it to Calvin", "send it to Kelvin", "", ""]);
        assert_eq!(
            run(&script, "send it to Calvin", 5_000).as_deref(),
            Some("send it to Kelvin")
        );
    }

    #[test]
    fn a_text_that_survives_unchanged_is_reported_as_the_pasted_text() {
        let script = Script::new(&["hello Calvin", "hello Calvin", "hello Calvin", "", ""]);
        assert_eq!(
            run(&script, "hello Calvin", 5_000).as_deref(),
            Some("hello Calvin")
        );
    }

    #[test]
    fn the_timeout_reports_the_last_good_reading() {
        let readings: Vec<&str> = std::iter::repeat_n("hi Kelvin", 500).collect();
        let mut all = vec!["hi Calvin"];
        all.extend(readings);
        let script = Script::new(&all);
        assert_eq!(run(&script, "hi Calvin", 40).as_deref(), Some("hi Kelvin"));
    }

    #[test]
    fn losing_focus_ends_the_watch_with_the_last_good_reading() {
        // After the script runs out the reader reports the element as gone.
        let script = Script::new(&["x hello Calvin y", "x hello Kelvin y"]);
        assert_eq!(
            run(&script, "hello Calvin", 5_000).as_deref(),
            Some("hello Kelvin")
        );
    }

    #[test]
    fn a_paste_that_is_not_in_the_box_learns_nothing() {
        let script = Script::new(&["something else entirely"]);
        assert_eq!(run(&script, "hello Calvin", 5_000), None);
    }

    #[test]
    fn an_unreadable_box_learns_nothing() {
        let script = Script::new(&[]);
        assert_eq!(run(&script, "hello", 5_000), None);
    }

    #[test]
    fn stop_ends_the_watch_early_without_a_new_reading() {
        let script = Script::new(&["a b c", "a x c", "a x c"]);
        let stop = AtomicBool::new(false);
        // Stop is set after the first poll: let the baseline and one reading in.
        let timing = WatchTiming {
            settle: Duration::from_millis(1),
            poll: Duration::from_millis(60),
            watch_for: Duration::from_secs(30),
        };
        let started = Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(90));
                stop.store(true, Ordering::SeqCst);
            });
            let result = run_watch(&script, "a b c", timing, &|| false, &stop);
            assert_eq!(result.as_deref(), Some("a x c"));
        });
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            script.next.load(Ordering::SeqCst),
            2,
            "one baseline and one poll"
        );
    }

    #[test]
    fn stop_before_the_first_reading_learns_nothing() {
        let script = Script::new(&["a b c"]);
        let stop = AtomicBool::new(true);
        assert_eq!(
            run_watch(&script, "a b c", fast(100), &|| false, &stop),
            None
        );
        assert_eq!(script.next.load(Ordering::SeqCst), 0);
    }

    /// RF4: two pastes in quick succession.
    #[test]
    fn rf4_the_first_watch_finishes_learning_before_the_second_reads() {
        use std::sync::Mutex;
        let log = Arc::new(Mutex::new(Vec::<String>::new()));

        struct Logging(Arc<Mutex<Vec<String>>>, &'static str, Script);
        impl FieldReader for Logging {
            fn read(&self) -> Option<String> {
                self.0.lock().unwrap().push(format!("{} reads", self.1));
                self.2.read()
            }
            fn is_secure(&self) -> bool {
                false
            }
        }

        let slow = WatchTiming {
            settle: Duration::from_millis(1),
            poll: Duration::from_millis(20),
            watch_for: Duration::from_secs(30),
        };
        let first_log = Arc::clone(&log);
        let first_done_log = Arc::clone(&log);
        let first = spawn_watch_with(
            move || {
                Some(Box::new(Logging(
                    first_log,
                    "first",
                    Script::new(&[
                        "send it to Calvin",
                        "send it to Kelvin",
                        "send it to Kelvin",
                    ]),
                )) as Box<dyn FieldReader>)
            },
            "send it to Calvin".to_string(),
            slow,
            || false,
            move |result| {
                // Learning takes a moment.
                std::thread::sleep(Duration::from_millis(80));
                first_done_log
                    .lock()
                    .unwrap()
                    .push(format!("first learned {result:?}"));
            },
        );
        std::thread::sleep(Duration::from_millis(60)); // the first watch is running

        let second_log = Arc::clone(&log);
        let (tx, rx) = mpsc::channel();
        let second = spawn_watch_after(
            Some(first),
            move || {
                Some(Box::new(Logging(
                    second_log,
                    "second",
                    Script::new(&["hello there", "hello there"]),
                )) as Box<dyn FieldReader>)
            },
            "hello there".to_string(),
            fast(40),
            || false,
            move |result| tx.send(result).unwrap(),
        );
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        second.finish_now();

        let log = log.lock().unwrap().clone();
        let learned = log
            .iter()
            .position(|l| l.starts_with("first learned"))
            .expect("the first watch finished");
        let second_read = log
            .iter()
            .position(|l| l == "second reads")
            .expect("the second watch read");
        assert!(learned < second_read, "{log:?}");
        assert!(log[learned].contains("send it to Kelvin"), "{log:?}");
        // The first watch took no reading after the second paste was announced.
        assert!(log[learned..].iter().all(|l| l != "first reads"), "{log:?}");
    }

    #[test]
    fn finish_now_waits_for_done() {
        let (tx, rx) = mpsc::channel();
        let handle = spawn_watch_with(
            || None,
            "x".to_string(),
            fast(5_000),
            || false,
            move |result| tx.send(result).unwrap(),
        );
        handle.finish_now();
        assert_eq!(rx.try_recv().unwrap(), None);
    }

    // ---- swap arithmetic ----

    #[test]
    fn locate_swap_finds_the_nth_word_inside_the_pasted_region() {
        let value = "Eres here. Eres met Eres and more";
        // The region was pasted second; "Eres" occurs twice inside it.
        let range = locate_swap(value, "Eres met Eres", "Eres", 1).unwrap();
        assert_eq!(&value[range.clone()], "Eres");
        assert_eq!(range.start, "Eres here. Eres met ".len());
        assert!(locate_swap(value, "Eres met Eres", "Eres", 2).is_none());
    }

    #[test]
    fn locate_swap_uses_the_last_copy_of_the_region() {
        let value = "push to Eres. push to Eres";
        let range = locate_swap(value, "push to Eres", "Eres", 0).unwrap();
        assert_eq!(range.start, "push to Eres. push to ".len());
    }

    #[test]
    fn locate_swap_gives_none_when_the_region_is_gone() {
        assert!(locate_swap("something else", "push to Eres", "Eres", 0).is_none());
        assert!(locate_swap("push to Eres", "push to Eres", "", 0).is_none());
    }

    #[test]
    fn the_caret_follows_a_replacement() {
        // "Eres" (4 units at 10) becomes "heiress" (7 units).
        assert_eq!(adjust_caret((20, 0), (10, 4), 7), (23, 0)); // after: moves by +3
        assert_eq!(adjust_caret((5, 0), (10, 4), 7), (5, 0)); // before: stays
        assert_eq!(adjust_caret((12, 0), (10, 4), 7), (17, 0)); // inside: end of new text
        assert_eq!(adjust_caret((14, 0), (10, 4), 7), (17, 0)); // right after: moves
        assert_eq!(adjust_caret((10, 0), (10, 4), 7), (10, 0)); // right before: stays
        assert_eq!(adjust_caret((20, 0), (10, 7), 4), (17, 0)); // shorter text
                                                                // A selection keeps its ends in the same relation to the text.
        assert_eq!(adjust_caret((2, 20), (10, 4), 7), (2, 23));
    }

    #[test]
    fn utf16_ranges_count_code_units_not_bytes() {
        let text = "caf\u{e9} \u{1F600} Eres";
        let start = text.find("Eres").unwrap();
        // "café " is 5 units, the emoji is 2 units (a surrogate pair), then a space.
        assert_eq!(utf16_range(text, start..start + 4), (8, 4));
        assert_eq!(utf16_range("abc", 1..3), (1, 2));
    }
}

//! Counters behind `Performance.getMetrics`.
//!
//! Everything here is O(1) to record and to read: a clock charged when an
//! isolate is entered and left, and counters bumped by the style and layout
//! passes. Nothing walks the document.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

thread_local! {
    /// Isolate entries open on this thread. A render pass that begins while
    /// one is open runs inside script (a forced layout) rather than beside it.
    static OPEN_ENTRIES: Cell<u32> = const { Cell::new(0) };
}

#[cfg(feature = "render")]
fn isolate_entered() -> bool {
    OPEN_ENTRIES.with(|open| open.get() > 0)
}

/// Total time one page's isolate has been entered. Entries on one runtime never
/// overlap, so a plain sum is exact.
#[derive(Default)]
pub(crate) struct IsolateClock {
    nanos: Cell<u64>,
}

impl IsolateClock {
    pub(crate) fn elapsed(&self) -> Duration {
        Duration::from_nanos(self.nanos.get())
    }
}

/// Charges the time an isolate stays entered to its page's clock.
pub(crate) struct EntryTimer {
    clock: Rc<IsolateClock>,
    started: Instant,
}

impl EntryTimer {
    pub(crate) fn start(clock: &Rc<IsolateClock>) -> Self {
        OPEN_ENTRIES.with(|open| open.set(open.get() + 1));
        Self {
            clock: clock.clone(),
            started: Instant::now(),
        }
    }
}

impl Drop for EntryTimer {
    fn drop(&mut self) {
        OPEN_ENTRIES.with(|open| open.set(open.get().saturating_sub(1)));
        let nanos = u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.clock
            .nanos
            .set(self.clock.nanos.get().saturating_add(nanos));
    }
}

/// Style and layout passes one document has run.
#[derive(Clone, Copy, Default)]
pub struct RenderPasses {
    pub layout_count: u64,
    pub recalc_style_count: u64,
    pub layout_time: Duration,
    pub recalc_style_time: Duration,
    /// The part of the two times above that ran while an isolate was entered.
    pub time_in_isolate: Duration,
}

impl RenderPasses {
    /// A full prepare. The cascade and layout run as one pass that cannot be
    /// timed apart, so the pass counts for both and its time is layout time.
    #[cfg(feature = "render")]
    pub(crate) fn record_layout(&mut self, elapsed: Duration) {
        self.layout_count += 1;
        self.recalc_style_count += 1;
        self.layout_time += elapsed;
        self.note_isolate_time(elapsed);
    }

    /// A pass that resolves styles only.
    #[cfg(feature = "render")]
    pub(crate) fn record_style(&mut self, elapsed: Duration) {
        self.recalc_style_count += 1;
        self.recalc_style_time += elapsed;
        self.note_isolate_time(elapsed);
    }

    #[cfg(feature = "render")]
    fn note_isolate_time(&mut self, elapsed: Duration) {
        if isolate_entered() {
            self.time_in_isolate += elapsed;
        }
    }

    pub(crate) fn add(&mut self, other: &RenderPasses) {
        self.layout_count += other.layout_count;
        self.recalc_style_count += other.recalc_style_count;
        self.layout_time += other.layout_time;
        self.recalc_style_time += other.recalc_style_time;
        self.time_in_isolate += other.time_in_isolate;
    }
}

/// What one page reports for `Performance.getMetrics`, across its frames.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PageMetrics {
    /// Live documents: the page's and each child frame's.
    pub documents: usize,
    /// Live frames, the main frame included.
    pub frames: usize,
    /// Live DOM nodes across every document, detached ones included.
    pub nodes: usize,
    /// Event listeners registered through `addEventListener`, in every realm.
    pub js_event_listeners: u64,
    pub layout_count: u64,
    pub recalc_style_count: u64,
    pub layout_duration: Duration,
    pub recalc_style_duration: Duration,
    /// Time in script: the isolate entered, less the passes it forced.
    pub script_duration: Duration,
    /// Script plus every style and layout pass, wherever it ran.
    pub task_duration: Duration,
    pub js_heap_used_size: usize,
    pub js_heap_total_size: usize,
}

impl PageMetrics {
    pub(crate) fn durations(isolate_time: Duration, passes: &RenderPasses) -> (Duration, Duration) {
        let script = isolate_time.saturating_sub(passes.time_in_isolate);
        let task = script + passes.layout_time + passes.recalc_style_time;
        (script, task)
    }
}

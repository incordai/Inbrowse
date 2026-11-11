//! Per-page time zone for V8's `Date` and `Intl`.
//!
//! ICU keeps one default zone per process, and V8 reads it for every local-time
//! conversion and when `Intl.DateTimeFormat()` is built without a `timeZone`.
//! V8 has no per-isolate zone: `Isolate::DateTimeConfigurationChangeNotification`
//! only drops the isolate's own caches, so they are rebuilt from that one
//! default. `inbrowse serve` runs many isolates in one process, on several
//! threads, so a page's zone is applied like this:
//!
//! - an isolate holds a [`ZonePermit`] for as long as it is entered;
//! - the permit makes ICU's default the isolate's zone and keeps it there, by
//!   letting only isolates that want the same zone run at the same time;
//! - the first entry under a new zone drops the isolate's date caches.
//!
//! Pages that share a zone, which is every page unless an override is set, run
//! in parallel as before. Pages with different zones take turns, one entry at
//! a time, and no page ever observes another page's zone.

use std::cell::RefCell;
use std::ffi::{c_char, c_void};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

type UErrorCode = i32;

const UCAL_GREGORIAN: i32 = 1;
const UCAL_ZONE_OFFSET: i32 = 15;
const UCAL_DST_OFFSET: i32 = 16;
const UNKNOWN_ZONE: &str = "Etc/Unknown";
const ZONE_ID_CAPACITY: usize = 128;

// ICU 77 is what the v8 crate links (see `v8::icu::set_common_data_77`); a V8
// upgrade that moves ICU fails here at link time, not silently.
unsafe extern "C" {
    fn ucal_setDefaultTimeZone_77(zone_id: *const u16, status: *mut UErrorCode);
    fn ucal_getDefaultTimeZone_77(
        result: *mut u16,
        capacity: i32,
        status: *mut UErrorCode,
    ) -> i32;
    fn ucal_open_77(
        zone_id: *const u16,
        len: i32,
        locale: *const c_char,
        kind: i32,
        status: *mut UErrorCode,
    ) -> *mut c_void;
    fn ucal_close_77(calendar: *mut c_void);
    fn ucal_getTimeZoneID_77(
        calendar: *const c_void,
        result: *mut u16,
        capacity: i32,
        status: *mut UErrorCode,
    ) -> i32;
    fn ucal_setMillis_77(calendar: *mut c_void, millis: f64, status: *mut UErrorCode);
    fn ucal_get_77(calendar: *const c_void, field: i32, status: *mut UErrorCode) -> i32;
}

/// An ICU calendar in one zone. It never touches ICU's default zone.
struct Calendar(*mut c_void);

impl Calendar {
    fn open(zone: &str) -> Option<Self> {
        let units: Vec<u16> = zone.encode_utf16().collect();
        let len = i32::try_from(units.len()).ok()?;
        let mut status: UErrorCode = 0;
        // SAFETY: `units` is valid for `len` code units, a null locale selects
        // the default one, and ICU copies the id.
        let raw = unsafe {
            ucal_open_77(units.as_ptr(), len, std::ptr::null(), UCAL_GREGORIAN, &mut status)
        };
        if status > 0 || raw.is_null() {
            return None;
        }
        Some(Self(raw))
    }

    fn zone_id(&self) -> Option<String> {
        let mut buffer = [0u16; ZONE_ID_CAPACITY];
        let mut status: UErrorCode = 0;
        // SAFETY: `self.0` is a live calendar and `buffer` holds the capacity
        // passed.
        let len = unsafe {
            ucal_getTimeZoneID_77(self.0, buffer.as_mut_ptr(), ZONE_ID_CAPACITY as i32, &mut status)
        };
        let len = usize::try_from(len).ok().filter(|len| status <= 0 && *len <= ZONE_ID_CAPACITY)?;
        Some(String::from_utf16_lossy(&buffer[..len]))
    }

    fn utc_offset_ms(&self, epoch_ms: f64) -> Option<i64> {
        let mut status: UErrorCode = 0;
        // SAFETY: `self.0` is a live calendar.
        unsafe { ucal_setMillis_77(self.0, epoch_ms, &mut status) };
        // SAFETY: as above.
        let zone = unsafe { ucal_get_77(self.0, UCAL_ZONE_OFFSET, &mut status) };
        // SAFETY: as above.
        let dst = unsafe { ucal_get_77(self.0, UCAL_DST_OFFSET, &mut status) };
        (status <= 0).then(|| i64::from(zone) + i64::from(dst))
    }
}

impl Drop for Calendar {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `ucal_open_77` and is closed once.
        unsafe { ucal_close_77(self.0) };
    }
}

/// A calendar in `zone`, or `None` when ICU does not know the zone. ICU answers
/// an unknown id with its "unknown" zone, not an error.
fn open_known(zone: &str) -> Option<Calendar> {
    Calendar::open(zone).filter(|calendar| calendar.zone_id().is_some_and(|id| id != UNKNOWN_ZONE))
}

/// True when ICU knows `zone`, the test a browser applies to a requested
/// override.
pub fn is_known_zone(zone: &str) -> bool {
    open_known(zone).is_some()
}

/// Offset of `zone` from UTC in milliseconds at `epoch_ms`, daylight saving
/// included. `None` when the zone is unknown or `epoch_ms` is out of range.
pub fn utc_offset_ms(zone: &str, epoch_ms: f64) -> Option<i64> {
    open_known(zone)?.utc_offset_ms(epoch_ms)
}

/// The zone pages use unless they override it: the process `TZ`, else the host.
///
/// The CLI sets `TZ` from `INBROWSE_TIMEZONE`, else `Europe/Berlin`
/// (inbrowse-cli/src/main.rs `run_cli`). ICU reads `TZ` on Unix, but on Windows
/// it asks the OS, so the zone is pinned from `TZ` here before the first isolate
/// reads it. Resolved once; the first call must come before any isolate runs.
pub fn process_default() -> &'static Arc<str> {
    static DEFAULT: OnceLock<Arc<str>> = OnceLock::new();
    DEFAULT.get_or_init(|| {
        #[cfg(windows)]
        if let Some(tz) = std::env::var("TZ").ok().filter(|tz| !tz.trim().is_empty()) {
            set_icu_default(tz.trim());
        }
        let mut buffer = [0u16; ZONE_ID_CAPACITY];
        let mut status: UErrorCode = 0;
        // SAFETY: `buffer` holds the capacity passed.
        let len = unsafe {
            ucal_getDefaultTimeZone_77(buffer.as_mut_ptr(), ZONE_ID_CAPACITY as i32, &mut status)
        };
        match usize::try_from(len).ok().filter(|len| status <= 0 && *len <= ZONE_ID_CAPACITY) {
            Some(len) => Arc::from(String::from_utf16_lossy(&buffer[..len])),
            None => {
                tracing::warn!("could not read the default timezone (ICU status {status})");
                Arc::from(UNKNOWN_ZONE)
            }
        }
    })
}

fn set_icu_default(zone: &str) {
    let units: Vec<u16> = zone.encode_utf16().chain(std::iter::once(0)).collect();
    let mut status: UErrorCode = 0;
    // SAFETY: `units` is NUL-terminated UTF-16 that outlives the call, and ICU
    // copies it. Callers hold the gate with no holders, so no isolate is reading
    // the default.
    unsafe { ucal_setDefaultTimeZone_77(units.as_ptr(), &mut status) };
    if status > 0 {
        tracing::warn!("could not set timezone {zone:?} (ICU status {status})");
    }
}

struct GateState {
    /// The zone ICU's default currently holds.
    zone: Arc<str>,
    /// Threads inside a permit.
    holders: usize,
    /// Threads blocked in `acquire`.
    waiting: usize,
}

struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(|| Gate {
        state: Mutex::new(GateState { zone: process_default().clone(), holders: 0, waiting: 0 }),
        changed: Condvar::new(),
    })
}

fn locked(state: &Mutex<GateState>) -> MutexGuard<'_, GateState> {
    state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

thread_local! {
    /// The zone this thread's permit holds and how many entries share it.
    static HELD: RefCell<Option<(Arc<str>, usize)>> = const { RefCell::new(None) };
}

/// Entry to the zone gate: ICU's default is [`ZonePermit::zone`] until dropped.
///
/// Entries nest on one thread (an operation that evaluates script inside
/// another), and they share the outermost permit. Nesting a different zone is
/// not possible, because a thread only ever enters its own page's isolate; if it
/// happened the inner entry would have to wait for itself, so it runs under the
/// outer zone and is logged.
pub(crate) struct ZonePermit {
    zone: Arc<str>,
    _thread_bound: PhantomData<Rc<()>>,
}

impl ZonePermit {
    fn acquire(wanted: &Arc<str>) -> Self {
        let reentered = HELD.with_borrow_mut(|held| {
            held.as_mut().map(|(zone, depth)| {
                *depth += 1;
                zone.clone()
            })
        });
        if let Some(zone) = reentered {
            if zone != *wanted {
                tracing::error!("nested isolate entry wants zone {wanted} while {zone} is held");
            }
            return Self { zone, _thread_bound: PhantomData };
        }

        let gate = gate();
        let mut state = locked(&gate.state);
        let mut queued = false;
        // A thread joins the zone already running only when no one else is
        // queued, so a thread wanting another zone is not starved.
        while !(state.holders == 0
            || (state.zone == *wanted && state.waiting == usize::from(queued)))
        {
            if !queued {
                state.waiting += 1;
                queued = true;
            }
            state = gate.changed.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        if queued {
            state.waiting -= 1;
        }
        if state.zone != *wanted {
            set_icu_default(wanted);
            state.zone = wanted.clone();
        }
        state.holders += 1;
        if state.waiting > 0 {
            // Threads queued behind a same-zone entry may run now.
            gate.changed.notify_all();
        }
        drop(state);

        HELD.with_borrow_mut(|held| *held = Some((wanted.clone(), 1)));
        Self { zone: wanted.clone(), _thread_bound: PhantomData }
    }

    pub(crate) fn zone(&self) -> &Arc<str> {
        &self.zone
    }
}

impl Drop for ZonePermit {
    fn drop(&mut self) {
        let outermost = HELD
            .try_with(|held| {
                let mut held = held.borrow_mut();
                let finished = match held.as_mut() {
                    Some((_, depth)) if *depth > 1 => {
                        *depth -= 1;
                        false
                    }
                    _ => true,
                };
                if finished {
                    *held = None;
                }
                finished
            })
            .unwrap_or(true);
        if !outermost {
            return;
        }
        let gate = gate();
        let mut state = locked(&gate.state);
        state.holders -= 1;
        if state.waiting > 0 {
            gate.changed.notify_all();
        }
    }
}

/// One isolate's zone: the one it wants and the one its date caches were built
/// for.
pub(crate) struct IsolateZone {
    wanted: RefCell<Arc<str>>,
    applied: RefCell<Option<Arc<str>>>,
}

impl IsolateZone {
    pub(crate) fn new() -> Rc<Self> {
        Rc::new(Self {
            wanted: RefCell::new(process_default().clone()),
            applied: RefCell::new(None),
        })
    }

    /// Override the zone, or restore the process default with `None`.
    pub(crate) fn set(&self, zone: Option<&str>) {
        *self.wanted.borrow_mut() = match zone {
            Some(zone) => Arc::from(zone),
            None => process_default().clone(),
        };
    }

    pub(crate) fn id(&self) -> Arc<str> {
        self.wanted.borrow().clone()
    }

    /// Wait for the gate to let this isolate run under its zone.
    pub(crate) fn acquire(&self) -> ZonePermit {
        let wanted = self.id();
        ZonePermit::acquire(&wanted)
    }

    /// Drop the isolate's date caches when they were built for another zone.
    /// Call with the isolate entered, under `permit`.
    pub(crate) fn sync(&self, isolate: &mut deno_core::v8::Isolate, permit: &ZonePermit) {
        let mut applied = self.applied.borrow_mut();
        if applied.as_ref() == Some(permit.zone()) {
            return;
        }
        isolate
            .date_time_configuration_change_notification(deno_core::v8::TimeZoneDetection::Skip);
        *applied = Some(permit.zone().clone());
    }
}

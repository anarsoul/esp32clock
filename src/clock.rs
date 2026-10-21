//! Wall clock: Embassy uptime plus an offset learned from NTP.
//!
//! The uptime counter runs off the crystal, so between syncs the drift is a
//! few tens of milliseconds per hour at most. The offset is replaced, not
//! slewed, on every sync; a jump of that size is invisible on a HH:MM display.

use core::cell::Cell;

use critical_section::Mutex;
use embassy_time::{Duration, Instant};

use crate::settings;

const SECS_PER_DAY: i64 = 86_400;

/// Microseconds to add to the uptime to get Unix time. `None` until synced.
static OFFSET_US: Mutex<Cell<Option<i64>>> = Mutex::new(Cell::new(None));

/// Local time of day.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LocalTime {
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

fn uptime_us() -> i64 {
    Instant::now().as_micros() as i64
}

/// Record that the Unix time is `unix_us` microseconds right now.
pub fn set_unix_micros(unix_us: i64) {
    let offset = unix_us - uptime_us();
    critical_section::with(|cs| OFFSET_US.borrow(cs).set(Some(offset)));
}

/// Unix time in microseconds, if synced.
pub fn unix_micros() -> Option<i64> {
    let offset = critical_section::with(|cs| OFFSET_US.borrow(cs).get())?;
    Some(uptime_us() + offset)
}

/// Whether a sync has happened.
pub fn is_synced() -> bool {
    critical_section::with(|cs| OFFSET_US.borrow(cs).get()).is_some()
}

/// Local time of day, if synced, in the timezone from the settings.
pub fn now() -> Option<LocalTime> {
    let unix_s = unix_micros()?.div_euclid(1_000_000);
    let tz_minutes = settings::get().tz_offset_minutes as i64;
    let local_s = (unix_s + tz_minutes * 60).rem_euclid(SECS_PER_DAY);
    Some(LocalTime {
        hour: (local_s / 3600) as u8,
        minute: (local_s % 3600 / 60) as u8,
        second: (local_s % 60) as u8,
    })
}

/// How long until the wall clock ticks over to the next whole second. Falls
/// back to a short fixed interval before the first sync.
pub fn until_next_second() -> Duration {
    match unix_micros() {
        Some(us) => Duration::from_micros((1_000_000 - us.rem_euclid(1_000_000)) as u64),
        None => Duration::from_millis(250),
    }
}

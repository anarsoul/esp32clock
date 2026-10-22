//! Wall clock: Embassy uptime plus an offset learned from NTP.
//!
//! The uptime counter runs off the crystal, so between syncs the drift is a
//! few tens of milliseconds per hour at most. The offset is replaced, not
//! slewed, on every sync; a jump of that size is invisible on a HH:MM display.
//!
//! Across a software reset the uptime restarts but the RTC timer does
//! not, so the time is carried in the `carry` record as Unix time minus
//! RTC time (`carry_time`) and set again from it at boot
//! (`restore_carried`). Only the reboot itself, a few seconds, runs on the
//! RTC slow clock; after that the crystal takes over again.

use core::cell::{Cell, RefCell};

use critical_section::Mutex;
use embassy_time::{Duration, Instant};
use esp_hal::rtc_cntl::Rtc;

use crate::{carry, settings};

const SECS_PER_DAY: i64 = 86_400;

/// Microseconds to add to the uptime to get Unix time. `None` until synced.
static OFFSET_US: Mutex<Cell<Option<i64>>> = Mutex::new(Cell::new(None));

/// The RTC timer, for the time carried across a software reset.
static RTC: Mutex<RefCell<Option<Rtc<'static>>>> = Mutex::new(RefCell::new(None));

/// Hand over the RTC timer. Call once at boot, before `restore_carried`.
pub fn init(rtc: Rtc<'static>) {
    critical_section::with(|cs| RTC.borrow(cs).replace(Some(rtc)));
}

/// Microseconds on the RTC timer since power-up, if `init` has run.
fn rtc_micros() -> Option<i64> {
    critical_section::with(|cs| {
        RTC.borrow(cs).borrow().as_ref().map(|rtc| rtc.time_since_power_up().as_micros() as i64)
    })
}

/// Write the current time into the carried record as an offset from the
/// RTC timer, so the next software reset starts with it. Does nothing
/// before the first sync.
pub fn carry_time() {
    if let (Some(unix_us), Some(rtc_us)) = (unix_micros(), rtc_micros()) {
        carry::update(|c| c.time_offset_us = Some(unix_us - rtc_us));
    }
}

/// Set the clock from the offset carried across a software reset, if
/// there is one. Returns whether the time is now known.
pub fn restore_carried() -> bool {
    match (carry::get().time_offset_us, rtc_micros()) {
        (Some(offset), Some(rtc_us)) => {
            set_unix_micros(rtc_us + offset);
            true
        }
        _ => false,
    }
}

/// Local time of day.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LocalTime {
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// 0 is Monday, 6 is Sunday.
    pub weekday: u8,
}

/// Day of the week of a local day count since 1970-01-01, which was a
/// Thursday. 0 is Monday.
pub fn weekday_of_days(days: i64) -> u8 {
    (days + 3).rem_euclid(7) as u8
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

/// Unix time in whole seconds, if synced.
pub fn unix_secs() -> Option<i64> {
    Some(unix_micros()?.div_euclid(1_000_000))
}

/// Local time of day for a Unix time in seconds, in the timezone from the
/// settings.
pub fn local_time(unix_s: i64) -> LocalTime {
    let tz_minutes = settings::get().tz_offset_minutes as i64;
    let local = unix_s + tz_minutes * 60;
    let local_s = local.rem_euclid(SECS_PER_DAY);
    LocalTime {
        hour: (local_s / 3600) as u8,
        minute: (local_s % 3600 / 60) as u8,
        second: (local_s % 60) as u8,
        weekday: weekday_of_days(local.div_euclid(SECS_PER_DAY)),
    }
}

/// Local time of day, if synced, in the timezone from the settings.
pub fn now() -> Option<LocalTime> {
    Some(local_time(unix_secs()?))
}

/// How long until the wall clock ticks over to the next whole second. Falls
/// back to a short fixed interval before the first sync.
pub fn until_next_second() -> Duration {
    match unix_micros() {
        Some(us) => Duration::from_micros((1_000_000 - us.rem_euclid(1_000_000)) as u64),
        None => Duration::from_millis(250),
    }
}

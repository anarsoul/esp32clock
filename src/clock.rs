//! Wall clock: Embassy uptime plus an offset learned from NTP.
//!
//! The uptime counter runs off the crystal, so between syncs the drift is a
//! few tens of milliseconds per hour at most. The offset is replaced, not
//! slewed, on every sync; a jump of that size is invisible on a HH:MM display.
//!
//! Across a software reset the uptime restarts, so the time is carried
//! in the `carry` record as plain Unix time, written every second by the
//! display tick and once more just before a deliberate reset
//! (`carry_time`), and set again from it at boot (`restore_carried`).
//! Nothing measures the reset itself; the restore takes the carried
//! time as the time at uptime zero plus `RESET_PAUSE`, the pause a
//! deliberate reset makes before it fires. The error is the bootloader's
//! share of the uptime, well under a second, or up to a second more
//! after a panic, which cannot write on the way down. The RTC timer is
//! not used: esp-hal recalibrates its slow clock at every boot, and a
//! time stored relative to it drifted with the whole uptime since
//! power-up (see `PLAN.md`, 2026-10-27).

use core::cell::Cell;

use critical_section::Mutex;
use embassy_time::{Duration, Instant};

use crate::{carry, settings};

pub use esp32clock_core::clock::LocalTime;

/// Microseconds to add to the uptime to get Unix time. `None` until synced.
static OFFSET_US: Mutex<Cell<Option<i64>>> = Mutex::new(Cell::new(None));

/// How long a deliberate reset waits after its last log line and
/// `carry_time` before it fires, so the line leaves the serial port.
/// `restore_carried` adds it back, so every deliberate reset path must
/// wait exactly this.
pub const RESET_PAUSE: Duration = Duration::from_millis(200);

/// Write the current time into the carried record, so the next software
/// reset starts with it. Does nothing before the first sync. Cheap: a
/// few words of RTC RAM, so the display tick calls it every second.
pub fn carry_time() {
    if let Some(unix_us) = unix_micros() {
        carry::update(|c| c.unix_us = Some(unix_us));
    }
}

/// Set the clock from the time carried across a software reset, if
/// there is one: it is taken as the time at uptime zero less the pause
/// the reset made. Returns whether the time is now known.
pub fn restore_carried() -> bool {
    match carry::get().unix_us {
        Some(unix_us) => {
            let offset = unix_us + RESET_PAUSE.as_micros() as i64;
            critical_section::with(|cs| OFFSET_US.borrow(cs).set(Some(offset)));
            true
        }
        None => false,
    }
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

/// Unix time in whole seconds, if synced.
pub fn unix_secs() -> Option<i64> {
    Some(unix_micros()?.div_euclid(1_000_000))
}

/// Local time of day for a Unix time in seconds, in the timezone from the
/// settings.
pub fn local_time(unix_s: i64) -> LocalTime {
    esp32clock_core::clock::local_time(unix_s, settings::get().tz_offset_minutes)
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

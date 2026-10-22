//! Deliberate reset as a last resort: the panel is blanked first and the
//! reset is counted in the record `carry` keeps across a software reset,
//! so the boot log, the heartbeat and the page can say it happened even
//! when nobody was watching the serial port at the time.

use embassy_time::{Duration, Timer};
use log::error;

use crate::{carry, clock, scan};

/// Self-heal resets since power-up, from the carried record.
pub fn count() -> u32 {
    carry::get().heals
}

/// Unix time of the last self-heal reset, if there was one and the clock
/// knew the time.
pub fn last_unix() -> Option<i64> {
    carry::get().last_heal_unix
}

/// Log the reason, stop the scan on a blank frame, count the reset and
/// reboot. A reset in the middle of a transfer would leave whatever row
/// the drivers had latched lit at full current until the firmware is
/// back, so the scan is stopped first; see `scan::stop`.
pub async fn reset(reason: core::fmt::Arguments<'_>) -> ! {
    let n = count() + 1;
    error!("heal: {reason}; self-heal reset {n} since power-up: blanking the panel and rebooting");
    scan::stop().await;
    carry::update(|c| {
        c.heals = n;
        c.last_heal_unix = clock::unix_secs();
    });
    // Let the log line leave the serial port.
    Timer::after(Duration::from_millis(200)).await;
    esp_hal::system::software_reset()
}

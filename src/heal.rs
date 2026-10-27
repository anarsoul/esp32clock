//! Deliberate reset as a last resort: the panel is blanked first and the
//! reset is counted in the record `carry` keeps across a software reset,
//! so the boot log, the heartbeat and the page can say it happened even
//! when nobody was watching the serial port at the time. The time goes
//! across in the same record, so a heal does not cost the display.

use embassy_time::{Duration, Instant, Timer};
use log::{error, warn};

use crate::{carry, clock, display, scan, ui};

/// How often the hold for a ringing alarm looks at the flag again.
const HOLD_POLL: Duration = Duration::from_secs(1);

/// The hold cannot outlast a full ring by more than this. A flag stuck
/// for any reason must not keep a heal off for good.
const HOLD_MARGIN: Duration = Duration::from_secs(30);

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
///
/// A ringing alarm is not cut off: the reset waits until the ring ends
/// by a press or its timeout, bounded by `ui::RING_TIMEOUT` plus a
/// margin. The alarm is recorded as handled before the flag clears, so
/// the reboot does not ring it again through the missed-alarm grace.
pub async fn reset(reason: core::fmt::Arguments<'_>) -> ! {
    if display::ringing() {
        warn!("heal: {reason}; alarm ringing, holding the reset until it ends");
        let held = Instant::now();
        while display::ringing() && held.elapsed() < ui::RING_TIMEOUT + HOLD_MARGIN {
            Timer::after(HOLD_POLL).await;
        }
        warn!("heal: alarm {} after {} s, going on with the reset", if display::ringing() { "still ringing" } else { "ended" }, held.elapsed().as_secs());
    }
    let n = count() + 1;
    error!("heal: {reason}; self-heal reset {n} since power-up: blanking the panel and rebooting");
    scan::stop().await;
    carry::update(|c| {
        c.heals = n;
        c.last_heal_unix = clock::unix_secs();
    });
    // The time goes over as an offset from the RTC timer, which keeps
    // running through the reset, so the panel shows it again at once.
    clock::carry_time();
    // Let the log line leave the serial port.
    Timer::after(Duration::from_millis(200)).await;
    esp_hal::system::software_reset()
}

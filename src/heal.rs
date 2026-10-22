//! Deliberate reset as a last resort: the panel is blanked first and the
//! reset is counted in RTC RAM, which a software reset does not clear, so
//! the boot log and the heartbeat can say it happened even when nobody
//! was watching the serial port at the time.

use embassy_time::{Duration, Timer};
use log::error;

use crate::scan;

/// Marks the counter as written by this firmware since power-up. RTC RAM
/// is zeroed on the first boot and otherwise kept, but a reset at the
/// wrong moment can skip the zeroing, so a value without the mark is
/// read as zero.
const MARK: u32 = 0x4845_414c; // "HEAL"

/// `[mark, count]`.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut RESETS: [u32; 2] = [0; 2];

fn read() -> u32 {
    // SAFETY: the static is only read and written from thread mode, one
    // task at a time, through raw pointers.
    let v = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(RESETS)) };
    if v[0] == MARK { v[1] } else { 0 }
}

/// Self-heal resets since power-up.
pub fn count() -> u32 {
    read()
}

/// Log the reason, stop the scan on a blank frame, count the reset and
/// reboot. A reset in the middle of a transfer would leave whatever row
/// the drivers had latched lit at full current until the firmware is
/// back, so the scan is stopped first; see `scan::stop`.
pub async fn reset(reason: core::fmt::Arguments<'_>) -> ! {
    let n = read() + 1;
    error!("heal: {reason}; self-heal reset {n} since power-up: blanking the panel and rebooting");
    scan::stop().await;
    // SAFETY: as in `read`.
    unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(RESETS), [MARK, n]) };
    // Let the log line leave the serial port.
    Timer::after(Duration::from_millis(200)).await;
    esp_hal::system::software_reset()
}

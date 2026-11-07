// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! Deliberate resets: the panel is blanked first and the time goes
//! across in the record `carry` keeps across a software reset, so a
//! reset does not cost the display. A self-heal, the last resort, is
//! also counted there, so the boot log, the heartbeat and the page can
//! say it happened even when nobody was watching the serial port at the
//! time; `POST /reload` and the reboot into a new image take the same
//! path without the count. A switch between station and access point
//! (`switch_mode`) is the one reset that starts the record over.

use embassy_time::{Duration, Instant, Timer};
use log::{error, warn};

use crate::carry::{self, Carried};
use crate::{clock, display, mqtt, scan, ui};

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
    carry::update(|c| {
        c.heals = n;
        c.last_heal_unix = clock::unix_secs();
    });
    reboot().await
}

/// Stop the scan on a blank frame, carry the time and reset. The tail of
/// a self-heal, the whole of `POST /reload`, and the reboot into a new
/// image, which has stopped the scan already (a second `stop` returns at
/// once). Whatever the caller wants carried goes into the record first;
/// the time is written here, and the restore adds the pause back. The
/// MQTT task says goodbye first, so Home Assistant sees the clock go.
pub async fn reboot() -> ! {
    mqtt::goodbye().await;
    scan::stop().await;
    clock::carry_time();
    // Let the last log line leave the serial port.
    Timer::after(clock::RESET_PAUSE).await;
    esp_hal::system::software_reset()
}

/// Reset into the other network mode: the access point when `boot_ap`,
/// the station otherwise. The carry record starts over, because every
/// word in it belongs to the mode that wrote it (the heal count, the
/// time, the alarm state), except the boots of an unconfirmed image,
/// which the rollback counts in either mode. No time is carried: there
/// is none worth having across a stay in the access point, and the
/// record is frozen so the display tick cannot write one back in
/// during the pause.
pub async fn switch_mode(boot_ap: bool) -> ! {
    warn!(
        "heal: resetting into {} mode, carry record started over",
        if boot_ap { "access point" } else { "station" }
    );
    mqtt::goodbye().await;
    scan::stop().await;
    carry::freeze(Carried { ota_boots: carry::get().ota_boots, boot_ap, ..Carried::default() });
    Timer::after(clock::RESET_PAUSE).await;
    esp_hal::system::software_reset()
}

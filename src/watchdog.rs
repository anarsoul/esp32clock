// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The watchdog: the RTC watchdog timer, the task that feeds it and the
//! check-ins it feeds on. The rules, and why there are two layers, are
//! in `core::watchdog`.
//!
//! The feeder is on the thread executor on purpose. That is the lowest
//! context in the firmware, below the interrupt executor, the radio's
//! threads and every critical section, so a feed proves all of those
//! gave the CPU back. Feeding from an interrupt or from the interrupt
//! executor would prove nothing.
//!
//! A reset by the hardware's timer cannot blank the panel first; like a
//! panic's, it leaves the row the drivers last latched lit until the
//! firmware is back. The carry record survives it (`carry::init`), so
//! the time, the count kept here and an unconfirmed image's boots do.

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::peripherals::RTC_TIMER;
use esp_hal::rtc_cntl::{Rtc, RwdtStage, RwdtStageAction, SocResetReason};
use esp_hal::time::Duration as HalDuration;
use log::{error, warn};
use portable_atomic::AtomicU32;

pub use onebitdawn_core::watchdog::{Cause, Party, Trips};
use onebitdawn_core::watchdog::{FEED_PERIOD_MS, HARD_TIMEOUT_SECS, LAST_RESORT_SECS, verdict};

use crate::{carry, clock, scan};

/// When each party last checked in, uptime milliseconds truncated to 32
/// bits, and whether it is expected to. One writer each, the party.
static LAST: [AtomicU32; Party::COUNT] = [const { AtomicU32::new(0) }; Party::COUNT];
static EXPECTED: [AtomicBool; Party::COUNT] = [const { AtomicBool::new(false) }; Party::COUNT];

/// The longest time between two feeds since boot, in milliseconds, for
/// the heartbeat: what the deadlines are judged by. One writer, the
/// feeder.
static LONGEST_GAP_MS: AtomicU32 = AtomicU32::new(0);

/// How long the feeder's own reset waits for the scan to halt. If the
/// scan task is what hung, the reset goes on without the blanking.
const HALT_WAIT: Duration = Duration::from_secs(1);

fn now_ms() -> u32 {
    Instant::now().as_millis() as u32
}

/// `party` is alive. Called from the party's own task only.
pub fn check_in(party: Party) {
    LAST[party as usize].store(now_ms(), Ordering::Relaxed);
}

/// `party` is to check in from now on, or no longer. Called from the
/// party's own task; the time goes in before the flag, so the feeder
/// never sees the flag with a stale time.
pub fn expect(party: Party, expected: bool) {
    if expected {
        check_in(party);
    }
    EXPECTED[party as usize].store(expected, Ordering::Release);
}

/// Whether `reason` is a reset by the RTC watchdog, at any stage.
pub fn was_reset_by(reason: Option<SocResetReason>) -> bool {
    matches!(reason, Some(SocResetReason::CoreRtcWdt | SocResetReason::SysRtcWdt | SocResetReason::Cpu0RtcWdt))
}

/// Start the hardware's timer: a core reset `HARD_TIMEOUT_SECS` after
/// the last feed, which leaves the RTC domain and the carry record in
/// it alone, and a reset of the whole system a little later if that did
/// not bring the firmware back. Call as early as the boot allows; the
/// boot is far shorter than the timeout, so nothing feeds it until the
/// feeder runs.
pub fn start(timer: RTC_TIMER<'static>) -> Rtc<'static> {
    let mut rtc = Rtc::new(timer);
    rtc.rwdt.set_timeout(RwdtStage::Stage0, HalDuration::from_secs(HARD_TIMEOUT_SECS));
    rtc.rwdt.set_timeout(RwdtStage::Stage1, HalDuration::from_secs(LAST_RESORT_SECS));
    rtc.rwdt.enable();
    rtc.rwdt.set_stage_action(RwdtStage::Stage0, RwdtStageAction::ResetCore);
    rtc.rwdt.set_stage_action(RwdtStage::Stage1, RwdtStageAction::ResetSystem);
    rtc.rwdt.feed();
    rtc
}

/// Count a reset by the hardware's timer in the carry record. Call
/// after `carry::init`, with the reason it returned.
pub fn note_boot(reason: Option<SocResetReason>) {
    if was_reset_by(reason) {
        carry::update(|c| c.watchdog = c.watchdog.tripped(Cause::Hardware));
    }
    let trips = carry::get().watchdog;
    if trips.count > 0 {
        warn!("watchdog: {} resets since power-up, the last by {}", trips.count, trips.last.name());
    }
}

/// Watchdog resets since power-up, for the JSON.
pub fn count() -> u32 {
    carry::get().watchdog.count
}

/// Feeds the hardware's timer while every expected party checks in, and
/// resets the clock when one does not. The display task is expected
/// from here on; the clip task says when it is.
#[embassy_executor::task]
pub async fn watchdog_task(mut rtc: Rtc<'static>) -> ! {
    expect(Party::Display, true);
    let mut fed = Instant::now();
    loop {
        Timer::after(Duration::from_millis(FEED_PERIOD_MS)).await;
        let parties = core::array::from_fn(|i| (EXPECTED[i].load(Ordering::Acquire), LAST[i].load(Ordering::Relaxed)));
        if let Some(party) = verdict(&parties, now_ms()) {
            trip(party).await
        }
        rtc.rwdt.feed();
        let gap = fed.elapsed().as_millis() as u32;
        if gap > LONGEST_GAP_MS.load(Ordering::Relaxed) {
            LONGEST_GAP_MS.store(gap, Ordering::Relaxed);
        }
        fed = Instant::now();
    }
}

/// A party missed its check-in: note who, blank the panel if the scan
/// task still answers, carry the time and reset. Nothing feeds the
/// hardware's timer from here, so a hang on the way down ends there. No
/// MQTT goodbye and no wait for a ringing alarm: the firmware is not
/// sound enough to be waited on.
async fn trip(party: Party) -> ! {
    error!("watchdog: {} has not checked in; blanking the panel and rebooting", party.name());
    carry::update(|c| c.watchdog = c.watchdog.tripped(Cause::Missed(party)));
    if with_timeout(HALT_WAIT, scan::halt()).await.is_err() {
        error!("watchdog: the scan did not halt, resetting as it is");
    }
    clock::carry_time();
    Timer::after(clock::RESET_PAUSE).await;
    esp_hal::system::software_reset()
}

/// The heartbeat's line.
pub struct Report;

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let trips = carry::get().watchdog;
        write!(f, "{} resets", trips.count)?;
        if trips.count > 0 {
            write!(f, " (last by {})", trips.last.name())?;
        }
        write!(f, ", longest feed gap {} ms", LONGEST_GAP_MS.load(Ordering::Relaxed))
    }
}

/// Hangs on purpose, for trying the watchdog on the bench. Only in a
/// build with the `bench-hang` feature, never in a release:
///
/// - `POST /hang/<kind>` with the token (or a PIN) hangs the running
///   firmware a second later: `thread` spins in a thread-mode task,
///   `irq` on the interrupt executor, `cs` inside a critical section
///   (the three the hardware's timer ends); `display` and `clip` park
///   that task on an await at its next pass (the feeder's to find; the
///   clip task only while a clip plays).
/// - `BENCH_HANG=boot` at build time spins in `main` right after
///   `ota::init`, on every boot: installed over the air, it is the
///   image that hangs before it is confirmed and must be rolled back.
#[cfg(feature = "bench-hang")]
pub mod bench {
    use core::sync::atomic::{AtomicBool, Ordering};

    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::signal::Signal;
    use embassy_time::{Duration, Timer};
    use log::warn;

    use super::Party;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Hang {
        Thread,
        Irq,
        Cs,
        Display,
        Clip,
    }

    impl Hang {
        pub fn parse(name: &str) -> Option<Hang> {
            Some(match name {
                "thread" => Hang::Thread,
                "irq" => Hang::Irq,
                "cs" => Hang::Cs,
                "display" => Hang::Display,
                "clip" => Hang::Clip,
                _ => return None,
            })
        }
    }

    static THREAD: Signal<CriticalSectionRawMutex, Hang> = Signal::new();
    static IRQ: Signal<CriticalSectionRawMutex, ()> = Signal::new();
    static PARK: [AtomicBool; Party::COUNT] = [const { AtomicBool::new(false) }; Party::COUNT];

    /// Long enough for the answer to the request to leave.
    const GRACE: Duration = Duration::from_secs(1);

    /// Ask for a hang; it happens in the context it is about.
    pub fn request(hang: Hang) {
        warn!("bench: {hang:?} hang requested");
        match hang {
            Hang::Thread | Hang::Cs => THREAD.signal(hang),
            Hang::Irq => IRQ.signal(()),
            Hang::Display => PARK[Party::Display as usize].store(true, Ordering::Relaxed),
            Hang::Clip => PARK[Party::Clip as usize].store(true, Ordering::Relaxed),
        }
    }

    /// Called by a party where it checks in: never returns once that
    /// party's hang was asked for.
    pub async fn park(party: Party) {
        if PARK[party as usize].load(Ordering::Relaxed) {
            warn!("bench: {} parked for good", party.name());
            core::future::pending::<()>().await
        }
    }

    fn spin() -> ! {
        loop {
            core::hint::spin_loop();
        }
    }

    /// The spin in thread mode, plain or with interrupts off.
    #[embassy_executor::task]
    pub async fn thread_task() -> ! {
        let hang = THREAD.wait().await;
        Timer::after(GRACE).await;
        if hang == Hang::Cs {
            warn!("bench: spinning in a critical section");
            critical_section::with(|_| spin())
        }
        warn!("bench: spinning in thread mode");
        spin()
    }

    /// The spin on the interrupt executor.
    #[embassy_executor::task]
    pub async fn irq_task() -> ! {
        IRQ.wait().await;
        Timer::after(GRACE).await;
        warn!("bench: spinning on the interrupt executor");
        spin()
    }

    /// The hang of every boot, if the build asked for it.
    pub fn at_boot() {
        if option_env!("BENCH_HANG") == Some("boot") {
            warn!("bench: BENCH_HANG=boot, spinning in main");
            spin()
        }
    }
}

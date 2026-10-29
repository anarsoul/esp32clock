//! State carried across a software reset in RTC fast RAM: the self-heal
//! count and time, the wall clock as the Unix time last written, the
//! volatile alarm state a reboot would otherwise lose (the skip-next
//! flag, and when the alarm last rang so the missed-alarm grace does not
//! ring it again), how many times a freshly installed firmware has
//! booted without being confirmed good, and whether the next boot is to
//! raise the access point.
//!
//! The record is used only after a software reset, which is what the
//! self-heal and the panic handler do. Any other reset reason, power-on,
//! brown-out, a watchdog, the flasher's reset, discards it: `init` zeroes
//! the record so nothing stale can be read, and the boot line says so.

use core::sync::atomic::{AtomicBool, Ordering};

use esp_hal::rtc_cntl::SocResetReason;
use portable_atomic::AtomicU32;

pub use esp32clock_core::carry::Carried;
use esp32clock_core::carry::{MARK, WORDS, decode, encode};

/// Atomics so the record is a plain `static`: a persistent static must be
/// valid in any bit pattern, which esp-hal asserts, and these are. One
/// task in thread mode touches it; the atomics buy the shared reference,
/// not concurrency.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static RECORD: [AtomicU32; WORDS] = [const { AtomicU32::new(0) }; WORDS];

/// Set by `freeze` once a mode switch has written its record: nothing
/// may change it in the pause before the reset, and the display tick
/// writes the time into it every second.
static FROZEN: AtomicBool = AtomicBool::new(false);

fn read_raw() -> [u32; WORDS] {
    core::array::from_fn(|i| RECORD[i].load(Ordering::Relaxed))
}

fn write_raw(v: [u32; WORDS]) {
    for (slot, word) in RECORD.iter().zip(v) {
        slot.store(word, Ordering::Relaxed);
    }
}

/// Decide once, from the reset reason, whether the record may be used,
/// and clear it when it may not. Call before anything reads it. Returns
/// the reason and whether the record was kept.
pub fn init() -> (Option<SocResetReason>, bool) {
    let reason = esp_hal::system::reset_reason();
    let soft = matches!(reason, Some(SocResetReason::CoreSw | SocResetReason::Cpu0Sw));
    let keep = soft && read_raw()[0] == MARK;
    if !keep {
        write_raw(encode(&Carried::default()));
    }
    (reason, keep)
}

/// The carried state as it stands now.
pub fn get() -> Carried {
    decode(read_raw())
}

/// Change the carried state. Written at once; the next software reset
/// hands it back. Dropped after `freeze`.
pub fn update(f: impl FnOnce(&mut Carried)) {
    if FROZEN.load(Ordering::Relaxed) {
        return;
    }
    let mut c = get();
    f(&mut c);
    write_raw(encode(&c));
}

/// Write `c` as the whole record and refuse every later `update`, for a
/// reset that must hand over exactly this: `heal::switch_mode`.
pub fn freeze(c: Carried) {
    write_raw(encode(&c));
    FROZEN.store(true, Ordering::Relaxed);
}

//! State carried across a software reset in RTC fast RAM: the self-heal
//! count and time, and the volatile alarm state a reboot would otherwise
//! lose (the skip-next flag, and which alarm was last handled so the
//! missed-alarm grace does not ring it again).
//!
//! The record is used only after a software reset, which is what the
//! self-heal and the panic handler do. Any other reset reason, power-on,
//! brown-out, a watchdog, the flasher's reset, discards it: `init` zeroes
//! the record so nothing stale can be read, and the boot line says so.

use core::sync::atomic::{AtomicBool, Ordering};

use esp_hal::rtc_cntl::SocResetReason;

/// Marks a record written by this firmware; a second guard behind the
/// reset reason, against a reset that skipped the persistent-RAM zeroing.
const MARK: u32 = 0x4341_5252; // "CARR"

/// `[mark, heals, last heal unix, skip, handled unix, handled minute]`.
/// A zero time means none.
const WORDS: usize = 6;

#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut RECORD: [u32; WORDS] = [0; WORDS];

/// Whether this boot followed a software reset.
static SOFT: AtomicBool = AtomicBool::new(false);

/// What was carried over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Carried {
    /// Self-heal resets since power-up.
    pub heals: u32,
    /// Unix time of the last self-heal, if one happened with the clock synced.
    pub last_heal_unix: Option<i64>,
    /// The skip-next-alarm flag.
    pub skip_next: bool,
    /// The alarm last rung and ended, or used up by the skip flag: the Unix
    /// time that happened and the alarm's minute of the day.
    pub handled: Option<(i64, u16)>,
}

fn read_raw() -> [u32; WORDS] {
    // SAFETY: the record is read and written from thread mode only, one
    // task at a time, through raw pointers.
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(RECORD)) }
}

fn write_raw(v: [u32; WORDS]) {
    // SAFETY: as in `read_raw`.
    unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(RECORD), v) }
}

fn decode(v: [u32; WORDS]) -> Carried {
    if v[0] != MARK {
        return Carried::default();
    }
    Carried {
        heals: v[1],
        last_heal_unix: (v[2] != 0).then_some(v[2] as i64),
        skip_next: v[3] != 0,
        handled: (v[4] != 0).then_some((v[4] as i64, v[5] as u16)),
    }
}

fn encode(c: &Carried) -> [u32; WORDS] {
    // Seconds fit a u32 until 2106.
    let (handled_at, handled_minute) = c.handled.unwrap_or((0, 0));
    [
        MARK,
        c.heals,
        c.last_heal_unix.unwrap_or(0) as u32,
        c.skip_next as u32,
        handled_at as u32,
        handled_minute as u32,
    ]
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
    SOFT.store(soft, Ordering::Relaxed);
    (reason, keep)
}

/// Whether this boot followed a software reset.
pub fn soft_reset() -> bool {
    SOFT.load(Ordering::Relaxed)
}

/// The carried state as it stands now.
pub fn get() -> Carried {
    decode(read_raw())
}

/// Change the carried state. Written at once; the next software reset
/// hands it back.
pub fn update(f: impl FnOnce(&mut Carried)) {
    let mut c = get();
    f(&mut c);
    write_raw(encode(&c));
}

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

/// Marks a record written by this firmware with this layout; a second
/// guard behind the reset reason, against a reset that skipped the
/// persistent-RAM zeroing. The low byte is the layout version: an
/// `ota::reboot` from a firmware with another layout leaves a record
/// this one must not read, and the mismatch discards it, one time.
/// Version 1 ("CARR") held an RTC-timer offset in words 6 and 7; version
/// 2 held the handled alarm as a Unix time and a minute in words 4 and 5;
/// version 3 had no `boot_ap` word.
const MARK: u32 = 0x4341_5200 | LAYOUT; // "CAR" + version
const LAYOUT: u32 = 4;

/// `[mark, heals, last heal unix, skip, last alarm unix, unix us low,
/// unix us high, ota boots, boot ap]`. A zero time means none.
const WORDS: usize = 9;

/// Atomics so the record is a plain `static`: a persistent static must be
/// valid in any bit pattern, which esp-hal asserts, and these are. One
/// task in thread mode touches it; the atomics buy the shared reference,
/// not concurrency.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static RECORD: [AtomicU32; WORDS] = [const { AtomicU32::new(0) }; WORDS];

/// Whether this boot followed a software reset.
static SOFT: AtomicBool = AtomicBool::new(false);

/// Set by `freeze` once a mode switch has written its record: nothing
/// may change it in the pause before the reset, and the display tick
/// writes the time into it every second.
static FROZEN: AtomicBool = AtomicBool::new(false);

/// What was carried over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Carried {
    /// Self-heal resets since power-up.
    pub heals: u32,
    /// Unix time of the last self-heal, if one happened with the clock synced.
    pub last_heal_unix: Option<i64>,
    /// The skip-next-alarm flag.
    pub skip_next: bool,
    /// Unix time the alarm last rang and ended, or was used up by the skip
    /// flag; `ui::Ui::last_alarm` as the display task carries it.
    pub last_alarm_unix: Option<i64>,
    /// Unix microseconds when the record was last written, from the
    /// display tick every second and from a deliberate reset just before
    /// it fires. See `clock::restore_carried`.
    pub unix_us: Option<i64>,
    /// Boots of an updated firmware that has not yet been confirmed good;
    /// zero when nothing is pending. See `ota::init`.
    pub ota_boots: u32,
    /// Raise the access point at this boot, set by the button hold;
    /// cleared by the reset that follows a provisioning save, and by
    /// any reset that is not a software one.
    pub boot_ap: bool,
}

fn read_raw() -> [u32; WORDS] {
    core::array::from_fn(|i| RECORD[i].load(Ordering::Relaxed))
}

fn write_raw(v: [u32; WORDS]) {
    for (slot, word) in RECORD.iter().zip(v) {
        slot.store(word, Ordering::Relaxed);
    }
}

fn decode(v: [u32; WORDS]) -> Carried {
    if v[0] != MARK {
        return Carried::default();
    }
    Carried {
        heals: v[1],
        last_heal_unix: (v[2] != 0).then_some(v[2] as i64),
        skip_next: v[3] != 0,
        last_alarm_unix: (v[4] != 0).then_some(v[4] as i64),
        unix_us: {
            let us = (v[5] as u64 | (v[6] as u64) << 32) as i64;
            (us != 0).then_some(us)
        },
        ota_boots: v[7],
        boot_ap: v[8] != 0,
    }
}

fn encode(c: &Carried) -> [u32; WORDS] {
    // Seconds fit a u32 until 2106.
    [
        MARK,
        c.heals,
        c.last_heal_unix.unwrap_or(0) as u32,
        c.skip_next as u32,
        c.last_alarm_unix.unwrap_or(0) as u32,
        c.unix_us.unwrap_or(0) as u64 as u32,
        (c.unix_us.unwrap_or(0) as u64 >> 32) as u32,
        c.ota_boots,
        c.boot_ap as u32,
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

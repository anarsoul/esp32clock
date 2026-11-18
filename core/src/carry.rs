// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The record carried across a software reset: its layout and codec.
//! The RTC-RAM static it lives in, and the reset-reason gate, are in the
//! firmware's `carry`.

/// Marks a record written by this firmware; a second guard behind the
/// reset reason, against a reset that skipped the persistent-RAM
/// zeroing. The low byte is the layout version. Version 1 ("CARR") held
/// an RTC-timer offset in words 6 and 7; version 2 held the handled
/// alarm as a Unix time and a minute in words 4 and 5; version 3 had no
/// `boot_ap` word: a record of those is discarded. Version 5 added the
/// watchdog's word after version 4's nine.
///
/// From version 4, the first public release's, the layout is
/// append-only, as the settings record is: a later version adds words
/// after these and never moves or redefines one, and bumps the version.
/// So an `ota::reboot` from a later firmware, or a rollback to this one,
/// hands over a record whose first `WORDS` words are read here
/// (`readable`), `ota_boots` among them, without which an update that
/// changed the layout would never be rolled back. This firmware writes
/// the mark back with its own version, which tells a later one that the
/// words past these are stale.
use crate::watchdog::Trips;

pub const MARK: u32 = TAG | LAYOUT; // "CAR" + version
const TAG: u32 = 0x4341_5200;
const LAYOUT: u32 = 5;

/// The first append-only layout: a record of it or a later one is read.
const OLDEST: u32 = 4;

/// Whether a record under this mark is one `decode` reads: the first
/// append-only layout or a later one. A word the record's own layout
/// did not have is read as zero.
pub const fn readable(mark: u32) -> bool {
    mark & !0xFF == TAG && mark & 0xFF >= OLDEST
}

/// `[mark, heals, last heal unix, skip, last alarm unix, unix us low,
/// unix us high, ota boots, boot ap, watchdog]`. A zero time means none.
pub const WORDS: usize = 10;

/// The layout the watchdog's word came with. A version-4 image writes
/// its nine words and its own mark, so under that mark the tenth is
/// whatever an earlier boot left there.
const WATCHDOG_SINCE: u32 = 5;

/// What was carried over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Carried {
    /// Self-heal resets since power-up.
    pub heals: u32,
    /// Unix time of the last self-heal, if one happened with the clock synced.
    pub last_heal_unix: Option<i64>,
    /// The skip-next-alarm flag.
    pub skip_next: bool,
    /// Local wall-clock seconds (`clock::LocalTime::local_secs`) when the
    /// alarm last rang and ended, or was used up by the skip flag;
    /// `ui::Ui::last_alarm` as the display task carries it. Unix time
    /// until 2026-11-07, with the same codec.
    pub last_alarm_local: Option<i64>,
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
    /// Watchdog resets since power-up and the cause of the last.
    pub watchdog: Trips,
}

pub fn decode(v: [u32; WORDS]) -> Carried {
    if !readable(v[0]) {
        return Carried::default();
    }
    Carried {
        heals: v[1],
        last_heal_unix: (v[2] != 0).then_some(v[2] as i64),
        skip_next: v[3] != 0,
        last_alarm_local: (v[4] != 0).then_some(v[4] as i64),
        unix_us: {
            let us = (v[5] as u64 | (v[6] as u64) << 32) as i64;
            (us != 0).then_some(us)
        },
        ota_boots: v[7],
        boot_ap: v[8] != 0,
        watchdog: if v[0] & 0xFF >= WATCHDOG_SINCE { Trips::from_word(v[9]) } else { Trips::default() },
    }
}

pub fn encode(c: &Carried) -> [u32; WORDS] {
    // Seconds fit a u32 until 2106.
    [
        MARK,
        c.heals,
        c.last_heal_unix.unwrap_or(0) as u32,
        c.skip_next as u32,
        c.last_alarm_local.unwrap_or(0) as u32,
        c.unix_us.unwrap_or(0) as u64 as u32,
        (c.unix_us.unwrap_or(0) as u64 >> 32) as u32,
        c.ota_boots,
        c.boot_ap as u32,
        c.watchdog.to_word(),
    ]
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::watchdog::{Cause, Party};

    #[test]
    fn round_trip() {
        let c = Carried {
            heals: 7,
            last_heal_unix: Some(1_761_000_000),
            skip_next: true,
            last_alarm_local: Some(1_761_000_060),
            unix_us: Some(1_761_000_123_456_789),
            ota_boots: 2,
            boot_ap: true,
            watchdog: Trips { count: 3, last: Cause::Missed(Party::Display) },
        };
        let words = encode(&c);
        assert_eq!(words[0], MARK);
        assert_eq!(words.len(), WORDS);
        assert_eq!(decode(words), c);
        assert_eq!(decode(encode(&Carried::default())), Carried::default());
    }

    #[test]
    fn zero_means_none_and_seconds_fit_until_2106() {
        let c = Carried { last_heal_unix: Some(u32::MAX as i64), last_alarm_local: Some(1), unix_us: None, ..Carried::default() };
        assert_eq!(decode(encode(&c)), c);
        let none = Carried { last_heal_unix: Some(0), last_alarm_local: Some(0), unix_us: Some(0), ..Carried::default() };
        assert_eq!(decode(encode(&none)), Carried::default(), "a zero time reads back as none");
    }

    #[test]
    fn a_wrong_mark_is_an_empty_record() {
        let mut words = encode(&Carried { heals: 3, ..Carried::default() });
        words[0] = TAG | (OLDEST - 1); // before the append-only layouts
        assert_eq!(decode(words), Carried::default());
        words[0] = MARK ^ 0x100;
        assert_eq!(decode(words), Carried::default());
        assert_eq!(decode([0; WORDS]), Carried::default());
        assert_eq!(MARK & 0xFF, LAYOUT);
        assert_eq!(MARK >> 8, 0x43_41_52, "\"CAR\"");
    }

    #[test]
    fn a_later_layout_is_read_by_its_first_words() {
        let c = Carried { heals: 3, ota_boots: 1, skip_next: true, unix_us: Some(1_761_000_123_456_789), ..Carried::default() };
        let mut words = encode(&c);
        for later in [MARK + 1, MARK | 0xFF] {
            words[0] = later;
            assert!(readable(later));
            assert_eq!(decode(words), c);
        }
        // An older image's record: its nine words, the tenth stale.
        words[0] = TAG | OLDEST;
        words[9] = Trips { count: 9, last: Cause::Hardware }.to_word();
        assert!(readable(words[0]));
        assert_eq!(decode(words), c, "the watchdog's word is not read under a mark that had none");
        // Written back, it is this layout's again.
        assert_eq!(encode(&decode(words))[0], MARK);
    }
}

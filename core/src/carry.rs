//! The record carried across a software reset: its layout and codec.
//! The RTC-RAM static it lives in, and the reset-reason gate, are in the
//! firmware's `carry`.

/// Marks a record written by this firmware with this layout; a second
/// guard behind the reset reason, against a reset that skipped the
/// persistent-RAM zeroing. The low byte is the layout version: an
/// `ota::reboot` from a firmware with another layout leaves a record
/// this one must not read, and the mismatch discards it, one time.
/// Version 1 ("CARR") held an RTC-timer offset in words 6 and 7; version
/// 2 held the handled alarm as a Unix time and a minute in words 4 and 5;
/// version 3 had no `boot_ap` word.
pub const MARK: u32 = 0x4341_5200 | LAYOUT; // "CAR" + version
const LAYOUT: u32 = 4;

/// `[mark, heals, last heal unix, skip, last alarm unix, unix us low,
/// unix us high, ota boots, boot ap]`. A zero time means none.
pub const WORDS: usize = 9;

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

pub fn decode(v: [u32; WORDS]) -> Carried {
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

pub fn encode(c: &Carried) -> [u32; WORDS] {
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let c = Carried {
            heals: 7,
            last_heal_unix: Some(1_761_000_000),
            skip_next: true,
            last_alarm_unix: Some(1_761_000_060),
            unix_us: Some(1_761_000_123_456_789),
            ota_boots: 2,
            boot_ap: true,
        };
        let words = encode(&c);
        assert_eq!(words[0], MARK);
        assert_eq!(words.len(), WORDS);
        assert_eq!(decode(words), c);
        assert_eq!(decode(encode(&Carried::default())), Carried::default());
    }

    #[test]
    fn zero_means_none_and_seconds_fit_until_2106() {
        let c = Carried { last_heal_unix: Some(u32::MAX as i64), last_alarm_unix: Some(1), unix_us: None, ..Carried::default() };
        assert_eq!(decode(encode(&c)), c);
        let none = Carried { last_heal_unix: Some(0), last_alarm_unix: Some(0), unix_us: Some(0), ..Carried::default() };
        assert_eq!(decode(encode(&none)), Carried::default(), "a zero time reads back as none");
    }

    #[test]
    fn a_wrong_mark_is_an_empty_record() {
        let mut words = encode(&Carried { heals: 3, ..Carried::default() });
        words[0] = MARK ^ 1; // another layout version
        assert_eq!(decode(words), Carried::default());
        assert_eq!(decode([0; WORDS]), Carried::default());
        assert_eq!(MARK & 0xFF, LAYOUT);
        assert_eq!(MARK >> 8, 0x43_41_52, "\"CAR\"");
    }
}

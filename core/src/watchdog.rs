// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The watchdog's rules: who checks in, how late is too late, and what
//! the carry record says of a reset. The RTC watchdog itself and the
//! task that feeds it are in the firmware's `watchdog`.
//!
//! Two layers. The hardware's timer resets the chip `HARD_TIMEOUT_SECS`
//! after the last feed, whatever the firmware is doing. The feeder, a
//! task on the thread executor and so the lowest context there is,
//! feeds it every `FEED_PERIOD_MS` only while every party that is
//! expected has checked in within `DEADLINE_MS`: that is for a task
//! parked for ever on an await while the rest runs, which starves
//! nobody and would otherwise go unseen.

/// The hardware's timeout. A minute, not a few seconds: a clock in a
/// reset loop still has the time to join its network and say something
/// each boot (the user, 2026-11-17).
pub const HARD_TIMEOUT_SECS: u64 = 60;

/// How long after the first stage's core reset the second stage resets
/// the whole system, RTC domain included, if the first did not help.
pub const LAST_RESORT_SECS: u64 = 5;

/// How late a check-in may be.
pub const DEADLINE_MS: u32 = 10_000;

/// How often the feeder looks and feeds.
pub const FEED_PERIOD_MS: u64 = 1_000;

/// A task that checks in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Party {
    /// The display task: every pass of its loop, at least once a second.
    Display = 0,
    /// The clip task, expected only from a clip's start to its end; the
    /// interrupt executor's sign of life.
    Clip = 1,
}

impl Party {
    pub const ALL: [Party; 2] = [Party::Display, Party::Clip];
    pub const COUNT: usize = Self::ALL.len();

    pub const fn name(self) -> &'static str {
        match self {
            Party::Display => "the display task",
            Party::Clip => "the clip task",
        }
    }
}

/// Whether a party is overdue. The times are uptime milliseconds
/// truncated to 32 bits, which wrap every 49 days: the difference is
/// what counts, and a party is looked at every second while expected.
pub const fn overdue(expected: bool, last_ms: u32, now_ms: u32) -> bool {
    expected && now_ms.wrapping_sub(last_ms) > DEADLINE_MS
}

/// The first overdue party, given each one's `(expected, last check-in)`
/// in `Party::ALL`'s order.
pub fn verdict(parties: &[(bool, u32); Party::COUNT], now_ms: u32) -> Option<Party> {
    Party::ALL.into_iter().zip(parties).find(|(_, (expected, last))| overdue(*expected, *last, now_ms)).map(|(p, _)| p)
}

/// What caused the last watchdog reset, as the carry record keeps it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Cause {
    /// No watchdog reset since power-up.
    #[default]
    None,
    /// The hardware's timer ran out: the feeder itself was starved, or
    /// its own reset hung.
    Hardware,
    /// The feeder reset the clock over a missed check-in.
    Missed(Party),
}

impl Cause {
    pub const fn name(self) -> &'static str {
        match self {
            Cause::None => "none",
            Cause::Hardware => "the hardware timer",
            Cause::Missed(p) => p.name(),
        }
    }
}

/// Watchdog resets since power-up and the cause of the last, one word
/// of the carry record: the count in the low 23 bits, then the
/// distrust of the clips, the cause in the top byte.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Trips {
    pub count: u32,
    pub last: Cause,
    /// The clip task hung in a clip: nothing plays one until
    /// `clips_trusted`. A clip that hangs its player would otherwise
    /// be rung again by the missed-alarm grace after the reset, and
    /// hang again, for as long as the grace lasts: an alarm that
    /// flashes and never sounds. Kept through later resets of any
    /// cause; a power cut clears it with the record.
    pub clips_suspect: bool,
}

const COUNT_MASK: u32 = 0x007F_FFFF;
const SUSPECT_BIT: u32 = 0x0080_0000;
const CAUSE_HARDWARE: u32 = 0xFF;

impl Trips {
    /// One more reset, by `cause`.
    pub const fn tripped(self, cause: Cause) -> Trips {
        Trips {
            count: if self.count < COUNT_MASK { self.count + 1 } else { COUNT_MASK },
            last: cause,
            clips_suspect: self.clips_suspect || matches!(cause, Cause::Missed(Party::Clip)),
        }
    }

    /// The clips are worth trying again: a new pack or a new firmware
    /// is in place.
    pub const fn clips_trusted(self) -> Trips {
        Trips { clips_suspect: false, ..self }
    }

    pub const fn to_word(self) -> u32 {
        let cause = match self.last {
            Cause::None => 0,
            Cause::Hardware => CAUSE_HARDWARE,
            Cause::Missed(p) => p as u32 + 1,
        };
        (if self.count < COUNT_MASK { self.count } else { COUNT_MASK }) | if self.clips_suspect { SUSPECT_BIT } else { 0 } | cause << 24
    }

    /// A cause this firmware does not know (a later one's party) is read
    /// as the hardware's: a reset happened, by whom is not known.
    pub fn from_word(w: u32) -> Trips {
        let last = match w >> 24 {
            0 => Cause::None,
            n => Party::ALL.into_iter().find(|p| *p as u32 + 1 == n).map_or(Cause::Hardware, Cause::Missed),
        };
        Trips { count: w & COUNT_MASK, last, clips_suspect: w & SUSPECT_BIT != 0 }
    }
}

// The software layer must have its say, and its reset the time to
// finish, well inside the hardware's minute.
const _: () = assert!((DEADLINE_MS as u64 + FEED_PERIOD_MS) * 2 < HARD_TIMEOUT_SECS * 1000);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_party_is_overdue_only_while_expected_and_past_the_deadline() {
        assert!(!overdue(true, 1_000, 1_000));
        assert!(!overdue(true, 1_000, 1_000 + DEADLINE_MS));
        assert!(overdue(true, 1_000, 1_001 + DEADLINE_MS));
        assert!(!overdue(false, 1_000, 1_001 + DEADLINE_MS), "not expected, never overdue");
    }

    #[test]
    fn the_millisecond_wrap_is_no_trip() {
        // Checked in just before the 32-bit wrap, looked at just after.
        assert!(!overdue(true, u32::MAX - 500, 400));
        assert!(overdue(true, u32::MAX - 500, DEADLINE_MS));
    }

    #[test]
    fn the_verdict_names_the_first_overdue_party() {
        let now = 100_000;
        let fresh = now - 500;
        let stale = now - DEADLINE_MS - 1;
        assert_eq!(verdict(&[(true, fresh), (false, stale)], now), None, "an idle clip task is not waited for");
        assert_eq!(verdict(&[(true, fresh), (true, fresh)], now), None);
        assert_eq!(verdict(&[(true, stale), (true, fresh)], now), Some(Party::Display));
        assert_eq!(verdict(&[(true, fresh), (true, stale)], now), Some(Party::Clip));
        assert_eq!(verdict(&[(true, stale), (true, stale)], now), Some(Party::Display));
    }

    #[test]
    fn trips_round_trip_through_the_word() {
        assert_eq!(Trips::default().to_word(), 0, "an empty record's zero word is no reset");
        assert_eq!(Trips::from_word(0), Trips::default());
        for last in [Cause::None, Cause::Hardware, Cause::Missed(Party::Display), Cause::Missed(Party::Clip)] {
            for clips_suspect in [false, true] {
                let t = Trips { count: 12_345, last, clips_suspect };
                assert_eq!(Trips::from_word(t.to_word()), t);
            }
        }
        let t = Trips::default().tripped(Cause::Missed(Party::Display)).tripped(Cause::Hardware);
        assert_eq!(t, Trips { count: 2, last: Cause::Hardware, clips_suspect: false });
    }

    #[test]
    fn the_count_saturates_and_an_unknown_cause_is_the_hardwares() {
        let full = Trips { count: COUNT_MASK, last: Cause::Hardware, clips_suspect: false };
        assert_eq!(full.tripped(Cause::Hardware).count, COUNT_MASK);
        assert_eq!(Trips::from_word(Trips { count: u32::MAX, last: Cause::None, clips_suspect: false }.to_word()), Trips { count: COUNT_MASK, ..Trips::default() });
        // A party a later firmware added.
        assert_eq!(Trips::from_word(3 | 0x40 << 24), Trips { count: 3, last: Cause::Hardware, clips_suspect: false });
    }

    #[test]
    fn only_a_hung_clip_task_puts_the_clips_under_suspicion() {
        for cause in [Cause::Hardware, Cause::Missed(Party::Display)] {
            assert!(!Trips::default().tripped(cause).clips_suspect, "{cause:?}");
        }
        let hung = Trips::default().tripped(Cause::Missed(Party::Clip));
        assert!(hung.clips_suspect);
        // Whatever resets the clock next, the clip is no better for it.
        let later = hung.tripped(Cause::Hardware).tripped(Cause::Missed(Party::Display));
        assert!(later.clips_suspect);
        assert_eq!(later.count, 3);
        // A new pack or firmware: the count and the cause stay.
        let trusted = later.clips_trusted();
        assert_eq!(trusted, Trips { count: 3, last: Cause::Missed(Party::Display), clips_suspect: false });
        assert!(!Trips::default().clips_trusted().clips_suspect);
    }

    #[test]
    fn every_party_has_its_index() {
        for (i, p) in Party::ALL.into_iter().enumerate() {
            assert_eq!(p as usize, i);
        }
    }
}

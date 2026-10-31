// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! Calendar arithmetic for the wall clock: a Unix time and a timezone
//! offset to the local time of day and the weekday. The clock itself,
//! uptime plus the NTP offset, is in the firmware.

const SECS_PER_DAY: i64 = 86_400;

/// Local time of day.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LocalTime {
    /// The Unix time this is the local time of.
    pub unix: i64,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// 0 is Monday, 6 is Sunday.
    pub weekday: u8,
}

/// Day of the week of a local day count since 1970-01-01, which was a
/// Thursday. 0 is Monday.
pub fn weekday_of_days(days: i64) -> u8 {
    (days + 3).rem_euclid(7) as u8
}

/// Local time of day for a Unix time in seconds, `tz_minutes` east of UTC.
pub fn local_time(unix_s: i64, tz_minutes: i16) -> LocalTime {
    let local = unix_s + tz_minutes as i64 * 60;
    let local_s = local.rem_euclid(SECS_PER_DAY);
    LocalTime {
        unix: unix_s,
        hour: (local_s / 3600) as u8,
        minute: (local_s % 3600 / 60) as u8,
        second: (local_s % 60) as u8,
        weekday: weekday_of_days(local.div_euclid(SECS_PER_DAY)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_was_a_thursday() {
        assert_eq!(weekday_of_days(0), 3);
        assert_eq!(weekday_of_days(4), 0, "1970-01-05 was a Monday");
        assert_eq!(weekday_of_days(-1), 2);
        assert_eq!(weekday_of_days(-4), 6);
    }

    #[test]
    fn local_time_of_day_and_weekday() {
        let t = local_time(0, 0);
        assert_eq!((t.hour, t.minute, t.second, t.weekday, t.unix), (0, 0, 0, 3, 0));
        let t = local_time(86_399, 0);
        assert_eq!((t.hour, t.minute, t.second, t.weekday), (23, 59, 59, 3));
        // Seven hours west: the epoch second is the evening before.
        let t = local_time(0, -420);
        assert_eq!((t.hour, t.minute, t.second, t.weekday), (17, 0, 0, 2));
        // Fourteen hours east across midnight.
        let t = local_time(10 * 3600, 840);
        assert_eq!((t.hour, t.weekday), (0, 4));
        // A real date: 2026-10-29 12:34:56 UTC is a Thursday.
        let t = local_time(1_793_277_296, 0);
        assert_eq!((t.hour, t.minute, t.second, t.weekday), (12, 34, 56, 3));
        assert_eq!(local_time(1_793_277_296, -420).hour, 5);
        // Negative Unix times do not wrap the fields.
        let t = local_time(-1, 0);
        assert_eq!((t.hour, t.minute, t.second, t.weekday), (23, 59, 59, 2));
    }
}

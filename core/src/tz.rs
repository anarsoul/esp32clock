// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! POSIX TZ rules: `PST8PDT,M3.2.0,M11.1.0`, `CET-1CEST,M3.5.0,M10.5.0/3`,
//! `<+0545>-5:45`. The rule is the timezone setting; the clock evaluates
//! it from the Unix time, so daylight saving needs nothing from the
//! network. `parse` is `const` so a rule in the build is checked by the
//! compiler, and `fixed` turns a plain offset into a rule for the old
//! setting and the old build variable.
//!
//! The grammar, from POSIX `TZ`: `std offset[dst[offset],start[/time],
//! end[/time]]`. A name is three or more letters, or `<…>` of three or
//! more letters, digits, `+` and `-`. An offset is `[+-]h[h][:mm[:ss]]`,
//! positive *west* of UTC as POSIX has it; this module stores every
//! offset east positive, as the rest of the clock counts. The daylight
//! offset defaults to one hour ahead of standard. A transition day is
//! `Mm.w.d` (month 1 to 12, week 1 to 5 where 5 is the last, day 0
//! Sunday to 6 Saturday), `Jn` (day 1 to 365 of the year, February 29
//! never counted) or `n` (day 0 to 365, February 29 counted); its time is
//! `[+-]h[h][h][:mm[:ss]]` from −167 to 167 hours, default 02:00, in
//! the local time in force before the transition. A daylight name
//! without rules is refused: the TZ database's default rules are not
//! knowable here.

use crate::settings::Text;

/// The longest rule stored, in bytes. `AEST-10AEDT,M10.1.0/2,M4.1.0/3`
/// is 30; a quoted pair with seconds in every field about 46.
pub const TZ_MAX: usize = 48;

/// The longest zone name taken, in bytes, without the brackets.
pub const NAME_MAX: usize = 12;

const SECS_PER_DAY: i64 = 86_400;

/// A parsed rule, borrowing the names from the string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Tz<'a> {
    pub std: Zone<'a>,
    pub dst: Option<Daylight<'a>>,
}

/// A name and its offset east of UTC in seconds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Zone<'a> {
    /// The name as written, without the brackets of a quoted one.
    pub name: &'a str,
    pub offset_secs: i32,
}

/// Daylight time: its zone and when it starts and ends each year.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Daylight<'a> {
    pub zone: Zone<'a>,
    /// Given in standard time.
    pub start: Transition,
    /// Given in daylight time.
    pub end: Transition,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Transition {
    pub day: Day,
    /// Seconds after local midnight of that day; may be negative or past
    /// the day.
    pub time_secs: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Day {
    /// `Mm.w.d`: `week` 5 is the last `weekday` (0 Sunday) of `month`.
    MonthWeekDay { month: u8, week: u8, weekday: u8 },
    /// `Jn`: 1 to 365, February 29 never counted.
    Julian(u16),
    /// `n`: 0 to 365, February 29 counted.
    ZeroBased(u16),
}

/// What the rule says about one instant.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Period<'a> {
    pub offset_secs: i32,
    pub dst: bool,
    pub name: &'a str,
}

/// UTC, for a record that cannot be parsed.
pub const UTC: Tz<'static> = Tz { std: Zone { name: "UTC", offset_secs: 0 }, dst: None };

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Some(v) => v,
            None => return None,
        }
    };
}

/// Parse a rule; `None` for anything the grammar above does not cover,
/// including a rule longer than `TZ_MAX`.
pub const fn parse(s: &str) -> Option<Tz<'_>> {
    let b = s.as_bytes();
    if b.len() > TZ_MAX {
        return None;
    }
    let (n0, n1, i) = tri!(name(b, 0));
    let (west, i) = tri!(offset(b, i, 24));
    let std = Zone { name: str_of(s, n0, n1), offset_secs: -west };
    if i == b.len() {
        return Some(Tz { std, dst: None });
    }
    let (d0, d1, mut i) = tri!(name(b, i));
    let mut dst_offset = std.offset_secs + 3600;
    if i < b.len() && b[i] != b',' {
        let (west, next) = tri!(offset(b, i, 24));
        dst_offset = -west;
        i = next;
    }
    if i >= b.len() || b[i] != b',' {
        return None;
    }
    let (start, i) = tri!(transition(b, i + 1));
    if i >= b.len() || b[i] != b',' {
        return None;
    }
    let (end, i) = tri!(transition(b, i + 1));
    if i != b.len() {
        return None;
    }
    Some(Tz { std, dst: Some(Daylight { zone: Zone { name: str_of(s, d0, d1), offset_secs: dst_offset }, start, end }) })
}

/// `s[a..b]`, in a `const fn`; the bounds come from the parser, so this
/// cannot fail.
const fn str_of(s: &str, a: usize, b: usize) -> &str {
    match core::str::from_utf8(s.as_bytes().split_at(b).0.split_at(a).1) {
        Ok(t) => t,
        Err(_) => "",
    }
}

/// A name at `i`: the start and end of its letters and the index after it.
const fn name(b: &[u8], i: usize) -> Option<(usize, usize, usize)> {
    if i >= b.len() {
        return None;
    }
    if b[i] == b'<' {
        let start = i + 1;
        let mut j = start;
        while j < b.len() && b[j] != b'>' {
            if !(b[j].is_ascii_alphanumeric() || b[j] == b'+' || b[j] == b'-') {
                return None;
            }
            j += 1;
        }
        if j >= b.len() || j - start < 3 || j - start > NAME_MAX {
            return None;
        }
        return Some((start, j, j + 1));
    }
    let mut j = i;
    while j < b.len() && b[j].is_ascii_alphabetic() {
        j += 1;
    }
    if j - i < 3 || j - i > NAME_MAX {
        return None;
    }
    Some((i, j, j))
}

/// Up to three digits at `i`.
const fn number(b: &[u8], i: usize) -> Option<(u32, usize)> {
    let mut j = i;
    let mut n = 0u32;
    while j < b.len() && b[j].is_ascii_digit() && j - i < 3 {
        n = n * 10 + (b[j] - b'0') as u32;
        j += 1;
    }
    if j == i {
        return None;
    }
    Some((n, j))
}

/// `[+-]h[h][h][:mm[:ss]]` at `i`, as signed seconds, with the hours no
/// more than `max_hours`.
const fn offset(b: &[u8], i: usize, max_hours: u32) -> Option<(i32, usize)> {
    let mut i = i;
    let mut negative = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        negative = b[i] == b'-';
        i += 1;
    }
    let (hours, mut i) = tri!(number(b, i));
    if hours > max_hours {
        return None;
    }
    let mut secs = hours * 3600;
    let mut parts = 0;
    while parts < 2 && i < b.len() && b[i] == b':' {
        let (n, next) = tri!(number(b, i + 1));
        if n > 59 {
            return None;
        }
        secs += n * if parts == 0 { 60 } else { 1 };
        i = next;
        parts += 1;
    }
    Some((if negative { -(secs as i32) } else { secs as i32 }, i))
}

/// A transition at `i`: the day and the optional `/time`.
const fn transition(b: &[u8], i: usize) -> Option<(Transition, usize)> {
    if i >= b.len() {
        return None;
    }
    let (day, mut i) = match b[i] {
        b'M' => {
            let (month, i) = tri!(number(b, i + 1));
            if i >= b.len() || b[i] != b'.' {
                return None;
            }
            let (week, i) = tri!(number(b, i + 1));
            if i >= b.len() || b[i] != b'.' {
                return None;
            }
            let (weekday, i) = tri!(number(b, i + 1));
            if month < 1 || month > 12 || week < 1 || week > 5 || weekday > 6 {
                return None;
            }
            (Day::MonthWeekDay { month: month as u8, week: week as u8, weekday: weekday as u8 }, i)
        }
        b'J' => {
            let (n, i) = tri!(number(b, i + 1));
            if n < 1 || n > 365 {
                return None;
            }
            (Day::Julian(n as u16), i)
        }
        _ => {
            let (n, i) = tri!(number(b, i));
            if n > 365 {
                return None;
            }
            (Day::ZeroBased(n as u16), i)
        }
    };
    let mut time_secs = 2 * 3600;
    if i < b.len() && b[i] == b'/' {
        let (t, next) = tri!(offset(b, i + 1, 167));
        time_secs = t;
        i = next;
    }
    Some((Transition { day, time_secs }, i))
}

impl Tz<'_> {
    /// The offset, the daylight flag and the name in force at `unix`.
    pub fn period_at(&self, unix: i64) -> Period<'_> {
        let Some(d) = self.dst else {
            return Period { offset_secs: self.std.offset_secs, dst: false, name: self.std.name };
        };
        // The latest transition at or before the instant decides. A
        // transition time may be negative or past 24:00 (to 167 hours),
        // which puts the event of one year's rule in the year next to
        // it, so the neighbouring years' events are candidates too; the
        // southern hemisphere, whose daylight time spans the new year,
        // needs nothing special this way.
        let year = civil_from_days((unix + self.std.offset_secs as i64).div_euclid(SECS_PER_DAY)).0;
        let mut latest: Option<(i64, bool)> = None;
        for y in year - 1..=year + 1 {
            for (at, dst) in [(transition_unix(y, d.start, self.std.offset_secs), true), (transition_unix(y, d.end, d.zone.offset_secs), false)] {
                if at <= unix && latest.is_none_or(|(l, _)| at > l) {
                    latest = Some((at, dst));
                }
            }
        }
        let in_dst = latest.is_some_and(|(_, dst)| dst);
        if in_dst {
            Period { offset_secs: d.zone.offset_secs, dst: true, name: d.zone.name }
        } else {
            Period { offset_secs: self.std.offset_secs, dst: false, name: self.std.name }
        }
    }

    /// The standard offset in whole minutes east of UTC, for the record's
    /// old field.
    pub fn std_offset_minutes(&self) -> i16 {
        (self.std.offset_secs / 60) as i16
    }
}

/// The Unix time of a transition in `year`, given in the local time whose
/// offset is `offset_before`.
fn transition_unix(year: i64, t: Transition, offset_before: i32) -> i64 {
    let days = match t.day {
        Day::MonthWeekDay { month, week, weekday } => {
            let first = days_from_civil(year, month as u32, 1);
            // 1970-01-01 was a Thursday, 4 in the Sunday-first count.
            let first_weekday = (first + 4).rem_euclid(7) as u32;
            let mut day = (weekday as i64 - first_weekday as i64).rem_euclid(7) + (week as i64 - 1) * 7;
            let len = month_len(year, month as u32);
            while day >= len {
                day -= 7;
            }
            first + day
        }
        Day::Julian(n) => {
            let mut doy = n as i64 - 1;
            if is_leap(year) && doy >= 59 {
                doy += 1;
            }
            days_from_civil(year, 1, 1) + doy
        }
        Day::ZeroBased(n) => days_from_civil(year, 1, 1) + n as i64,
    };
    days * SECS_PER_DAY + t.time_secs as i64 - offset_before as i64
}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn month_len(year: i64, month: u32) -> i64 {
    let next = if month == 12 { days_from_civil(year + 1, 1, 1) } else { days_from_civil(year, month + 1, 1) };
    next - days_from_civil(year, month, 1)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day count since 1970-01-01 (Howard Hinnant's
/// `civil_from_days`): year, month 1 to 12, day 1 to 31.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if month <= 2 { 1 } else { 0 }, month, day)
}

/// The rule for a plain offset in minutes east of UTC: `<-07>7`,
/// `<+0545>-5:45`, `<+00>0`. What the old setting and the old build
/// variable become.
pub const fn fixed(minutes: i16) -> Text<TZ_MAX> {
    let mut out = [0u8; TZ_MAX];
    let mut n = 0;
    let west = minutes < 0;
    let m = minutes.unsigned_abs();
    let (h, mm) = (m / 60, m % 60);
    out[n] = b'<';
    out[n + 1] = if west { b'-' } else { b'+' };
    out[n + 2] = b'0' + (h / 10) as u8;
    out[n + 3] = b'0' + (h % 10) as u8;
    n += 4;
    if mm != 0 {
        out[n] = b'0' + (mm / 10) as u8;
        out[n + 1] = b'0' + (mm % 10) as u8;
        n += 2;
    }
    out[n] = b'>';
    n += 1;
    // POSIX counts west positive: the sign flips.
    if !west && minutes != 0 {
        out[n] = b'-';
        n += 1;
    }
    if h >= 10 {
        out[n] = b'0' + (h / 10) as u8;
        n += 1;
    }
    out[n] = b'0' + (h % 10) as u8;
    n += 1;
    if mm != 0 {
        out[n] = b':';
        out[n + 1] = b'0' + (mm / 10) as u8;
        out[n + 2] = b'0' + (mm % 10) as u8;
        n += 3;
    }
    Text::from_ascii(out, n as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_displaced_across_new_year() {
        // Daylight time from 23:00 on December 31 (`J1/-1`) to 02:00 on
        // June 29: the start belongs to the next year's rule, and the
        // instant's own year has no transition at midnight.
        let tz = parse("STD0DST,J1/-1,J180/2").unwrap();
        let new_year_2027 = days_from_civil(2027, 1, 1) * SECS_PER_DAY;
        assert!(!tz.period_at(new_year_2027 - 3601).dst, "22:59:59 on December 31 is standard time");
        assert!(tz.period_at(new_year_2027 - 3600).dst, "23:00 on December 31 starts daylight time");
        assert!(tz.period_at(new_year_2027 - 1).dst, "23:59:59 stays in it");
        assert!(tz.period_at(new_year_2027).dst, "and so does midnight");
        assert!(tz.period_at(new_year_2027 + 1).dst);
        let june_29 = days_from_civil(2027, 6, 29) * SECS_PER_DAY;
        assert!(tz.period_at(june_29 + 2 * 3600 - 3600 - 1).dst, "01:59:59 daylight time");
        assert!(!tz.period_at(june_29 + 2 * 3600 - 3600).dst, "02:00 daylight time is the end (the end is given in daylight time)");
        // A positive time past the year's end: the end at 25:00 of
        // December 31 is 01:00 daylight time on January 1, 00:00 UTC.
        let tz = parse("STD0DST,M3.2.0,J365/25").unwrap();
        assert!(tz.period_at(new_year_2027 - 1).dst, "23:59:59 UTC on December 31 is still daylight time");
        assert!(!tz.period_at(new_year_2027).dst, "00:00 UTC on January 1 ends it");
        assert!(!tz.period_at(new_year_2027 + 3600).dst);
    }

    const US: &str = "PST8PDT,M3.2.0,M11.1.0";
    const EU: &str = "CET-1CEST,M3.5.0,M10.5.0/3";
    const SYDNEY: &str = "AEST-10AEDT,M10.1.0,M4.1.0/3";
    const LORD_HOWE: &str = "<+1030>-10:30<+11>-11,M10.1.0,M4.1.0";
    const NUUK: &str = "<-02>2<-01>,M3.5.0/-1,M10.5.0/0";

    fn std_dst(rule: &str) -> (i32, i32) {
        let tz = parse(rule).unwrap();
        (tz.std.offset_secs, tz.dst.unwrap().zone.offset_secs)
    }

    #[test]
    fn parses_the_common_shapes() {
        let tz = parse(US).unwrap();
        assert_eq!(tz.std, Zone { name: "PST", offset_secs: -8 * 3600 });
        let d = tz.dst.unwrap();
        assert_eq!(d.zone, Zone { name: "PDT", offset_secs: -7 * 3600 });
        assert_eq!(d.start, Transition { day: Day::MonthWeekDay { month: 3, week: 2, weekday: 0 }, time_secs: 7200 });
        assert_eq!(d.end, Transition { day: Day::MonthWeekDay { month: 11, week: 1, weekday: 0 }, time_secs: 7200 });

        let tz = parse(EU).unwrap();
        assert_eq!(tz.std, Zone { name: "CET", offset_secs: 3600 });
        assert_eq!(tz.dst.unwrap().end.time_secs, 3 * 3600);

        assert_eq!(std_dst(SYDNEY), (10 * 3600, 11 * 3600));
        assert_eq!(std_dst(LORD_HOWE), (10 * 3600 + 1800, 11 * 3600));
        let tz = parse(LORD_HOWE).unwrap();
        assert_eq!((tz.std.name, tz.dst.unwrap().zone.name), ("+1030", "+11"));

        let tz = parse(NUUK).unwrap();
        assert_eq!(tz.dst.unwrap().start.time_secs, -3600);
        assert_eq!(tz.dst.unwrap().end.time_secs, 0);

        assert_eq!(parse("<+0545>-5:45").unwrap(), Tz { std: Zone { name: "+0545", offset_secs: 5 * 3600 + 45 * 60 }, dst: None });
        assert_eq!(parse("IST-5:30").unwrap().std.offset_secs, 5 * 3600 + 30 * 60);
        assert_eq!(parse("UTC0").unwrap(), UTC);
        assert_eq!(parse("EST5EDT,J60,J300").unwrap().dst.unwrap().start.day, Day::Julian(60));
        assert_eq!(parse("EST5EDT,60,300/1:30:15").unwrap().dst.unwrap().end, Transition { day: Day::ZeroBased(300), time_secs: 5415 });
        assert_eq!(parse("UTC+0:0:0").unwrap().std.offset_secs, 0);
    }

    #[test]
    fn refuses_what_the_grammar_does_not_cover() {
        for bad in [
            "",
            "TZ",       // too short a name
            "PST",      // no offset
            "PST8PDT",  // daylight name without rules
            "PST8PDT,M3.2.0", // one rule
            "PST8PDT,M3.2.0,M11.1.0,", // trailing comma
            "PST8PDT,M3.2.0,M11.1.0x", // trailing junk
            "PST8PDT,M13.2.0,M11.1.0",
            "PST8PDT,M3.0.0,M11.1.0",
            "PST8PDT,M3.6.0,M11.1.0",
            "PST8PDT,M3.2.7,M11.1.0",
            "PST8PDT,J0,J300",
            "PST8PDT,J366,J300",
            "PST8PDT,366,300",
            "PST8PDT,M3.2.0/168,M11.1.0",
            "PST25",
            "PST8:60",
            "<PST>8",    // a quoted name is fine, but...
            "<PS T>8",   // ...not with a space
            "<+08",      // unterminated
            "PST 8",
            "PST8 PDT,M3.2.0,M11.1.0",
            "America/Vancouver",
            "<ABCDEFGHIJKLM>5", // a 13-letter name
        ] {
            let r = parse(bad);
            assert!(r.is_none() || bad == "<PST>8", "{bad:?} parsed as {r:?}");
        }
        assert!(parse("<PST>8").is_some());
        let long = "AEST-10AEDT,M10.1.0/2:00:00,M4.1.0/3:00:00XXXXXXXXXXX";
        assert!(long.len() > TZ_MAX && parse(long).is_none());
    }

    /// The second before and the second of each transition in 2026.
    fn check(rule: &str, start_utc: i64, end_utc: i64) {
        let tz = parse(rule).unwrap();
        let (std, dst) = std_dst(rule);
        let (sn, dn) = (tz.std.name, tz.dst.unwrap().zone.name);
        assert_eq!(tz.period_at(start_utc - 1), Period { offset_secs: std, dst: false, name: sn }, "{rule} before start");
        assert_eq!(tz.period_at(start_utc), Period { offset_secs: dst, dst: true, name: dn }, "{rule} at start");
        assert_eq!(tz.period_at(end_utc - 1), Period { offset_secs: dst, dst: true, name: dn }, "{rule} before end");
        assert_eq!(tz.period_at(end_utc), Period { offset_secs: std, dst: false, name: sn }, "{rule} at end");
    }

    #[test]
    fn transitions_of_2026() {
        // 2026-03-08 02:00 PST = 10:00 UTC; 2026-11-01 02:00 PDT = 09:00 UTC.
        check(US, 1_772_964_000, 1_793_523_600);
        // 2026-03-29 02:00 CET and 2026-10-25 03:00 CEST, both 01:00 UTC.
        check(EU, 1_774_746_000, 1_792_890_000);
        // Nuuk's rules are written to change with Europe: 01:00 UTC both.
        check(NUUK, 1_774_746_000, 1_792_890_000);
        // The end before the start in the year: 2026-10-04 02:00 AEST =
        // 10-03 16:00 UTC; 2026-04-05 03:00 AEDT = 04-04 16:00 UTC.
        check(SYDNEY, 1_791_043_200, 1_775_318_400);
        // Half an hour of daylight: 10-04 02:00 +10:30 = 10-03 15:30 UTC;
        // 04-05 02:00 +11 = 04-04 15:00 UTC.
        check(LORD_HOWE, 1_791_041_400, 1_775_314_800);
        // The whole southern summer, across the new year.
        let tz = parse(SYDNEY).unwrap();
        assert!(tz.period_at(1_767_225_600).dst, "2026-01-01 00:00 UTC");
        assert!(tz.period_at(1_767_225_599).dst, "2025-12-31 23:59:59 UTC");
        assert!(!tz.period_at(1_782_864_000).dst, "2026-07-01 00:00 UTC");
    }

    #[test]
    fn julian_and_zero_based_days() {
        // J60 is always 1 March; 60 is 2 March in 2026 and 1 March in 2028.
        let j = parse("EST5EDT,J60,J300").unwrap();
        let z = parse("EST5EDT,60,300").unwrap();
        let (mar1_2026, mar2_2026, mar1_2028) = (1_772_348_400, 1_772_434_800, 1_835_506_800); // 02:00 EST = 07:00 UTC
        assert!(!j.period_at(mar1_2026 - 1).dst && j.period_at(mar1_2026).dst);
        assert!(!z.period_at(mar2_2026 - 1).dst && z.period_at(mar2_2026).dst);
        assert!(!j.period_at(mar1_2028 - 1).dst && j.period_at(mar1_2028).dst);
        assert!(!z.period_at(mar1_2028 - 1).dst && z.period_at(mar1_2028).dst);
        // J300 is 27 October every year (the leap day is skipped); 300 is
        // 28 October 2026 and 27 October 2028. 02:00 EDT is 06:00 UTC.
        let (oct27_2026, oct27_2028) = (1_793_084_400 - 3600, 1_856_156_400 + 86_400 - 3600);
        assert!(j.period_at(oct27_2026 - 1).dst && !j.period_at(oct27_2026).dst);
        assert!(j.period_at(oct27_2028 - 1).dst && !j.period_at(oct27_2028).dst);
        assert!(z.period_at(oct27_2026 + 86_400 - 1).dst && !z.period_at(oct27_2026 + 86_400).dst);
        assert!(z.period_at(oct27_2028 - 1).dst && !z.period_at(oct27_2028).dst);
    }

    #[test]
    fn fixed_offsets_round_trip() {
        for (m, text) in [(-420, "<-07>7"), (0, "<+00>0"), (345, "<+0545>-5:45"), (-210, "<-0330>3:30"), (840, "<+14>-14"), (-840, "<-14>14")] {
            let t = fixed(m);
            assert_eq!(t.as_str(), text);
            let tz = parse(t.as_str()).unwrap();
            assert_eq!(tz.std.offset_secs, m as i32 * 60, "{text}");
            assert!(tz.dst.is_none());
            assert_eq!(tz.std_offset_minutes(), m);
        }
    }

    #[test]
    fn calendar_arithmetic() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 11, 7), 1_794_052_800 / 86_400);
        assert_eq!(civil_from_days(1_794_052_800 / 86_400), (2026, 11, 7));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(days_from_civil(2000, 2, 29)), (2000, 2, 29));
        assert_eq!(month_len(2026, 2), 28);
        assert_eq!(month_len(2028, 2), 29);
        assert_eq!(month_len(2026, 12), 31);
    }
}

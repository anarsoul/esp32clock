// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The user's settings: one shared value with a change signal, and the
//! record they are stored as in flash.
//!
//! Anything may change the settings through [`update`]; the display and
//! the persist task each hold a [`Receiver`] and wake when something did.
//! Pure logic apart from the `embassy-sync` watch, so host-testable.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Receiver, Watch};

use crate::config::{
    API_TOKEN, DEFAULT_ALARM, DEFAULT_BRIGHTNESS, DEFAULT_COLOR, DEFAULT_HOSTNAME, DEFAULT_TWELVE_HOUR,
    DEFAULT_TZ_OFFSET_MINUTES, MAX_CURRENT_STEP, WIFI_PASSWORD, WIFI_SSID,
};
use crate::melody;
use crate::panel::{Levels, MAX_LEVEL};

/// A channel fully lit.
const F: u8 = MAX_LEVEL;

/// Colours the top button cycles through, as duty levels in the panel's
/// blue-green-red order. Orange has a touch of green.
pub const PALETTE: [Levels; 8] = [
    Levels::new(0, 0, F), // red
    Levels::new(0, 1, F), // orange
    Levels::new(0, F, F), // yellow
    Levels::new(0, F, 0), // green
    Levels::new(F, F, 0), // cyan
    Levels::new(F, 0, 0), // blue
    Levels::new(F, 0, F), // magenta
    Levels::new(F, F, F), // white
];

/// Names of the palette entries, for the page and the API.
pub const PALETTE_NAMES: [&str; 8] = ["red", "orange", "yellow", "green", "cyan", "blue", "magenta", "white"];

/// Every brightness the firmware knows, as a percent for the page and
/// the current step (0 is 2.5 mA, 15 is 40.4 mA per lit sink) it means,
/// ascending. A stored brightness is an index into this list, whatever
/// the build's ceiling. The steps are the stock firmware's curve, gamma
/// 2.8 between the floor and the top step, read at these five points.
const BRIGHTNESS_CHOICES: [(u8, u8); 5] = [(10, 1), (25, 2), (50, 5), (75, 9), (100, 15)];

/// How many of the choices stay within `config::MAX_CURRENT_STEP`. The
/// list is ascending, so they are a prefix. 10 % is step 1, so any
/// ceiling from 1 up keeps at least one.
const fn brightness_count() -> usize {
    let mut n = 0;
    while n < BRIGHTNESS_CHOICES.len() && BRIGHTNESS_CHOICES[n].1 <= MAX_CURRENT_STEP {
        n += 1;
    }
    n
}

/// Brightness steps the bottom button cycles through and the page and
/// the API offer, in percent: the choices whose current step is within
/// the build's ceiling. Anything above it would draw the same as the
/// last one, so it is not offered.
pub const BRIGHTNESS_STEPS: [u8; brightness_count()] = {
    let mut out = [0u8; brightness_count()];
    let mut i = 0;
    while i < out.len() {
        out[i] = BRIGHTNESS_CHOICES[i].0;
        i += 1;
    }
    out
};

const _: () = assert!(!BRIGHTNESS_STEPS.is_empty(), "no brightness step within MAX_CURRENT_STEP");

/// Timezone offsets this far either side of UTC are accepted, in minutes.
pub const TZ_LIMIT_MINUTES: i16 = 14 * 60;

/// Longest hostname, in bytes. Fits embassy-net's 32-byte DHCP option.
pub const HOSTNAME_MAX: usize = 24;

/// A DHCP hostname: 1 to `HOSTNAME_MAX` of `a-z`, `0-9` and `-`, not
/// starting or ending with `-`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Hostname {
    len: u8,
    bytes: [u8; HOSTNAME_MAX],
}

impl Hostname {
    /// Validate and store a name, lower-casing ASCII letters. `const` so
    /// the build-time default is checked by the same rule.
    pub const fn new(name: &str) -> Option<Hostname> {
        let src = name.as_bytes();
        if src.is_empty() || src.len() > HOSTNAME_MAX || src[0] == b'-' || src[src.len() - 1] == b'-' {
            return None;
        }
        let mut bytes = [0u8; HOSTNAME_MAX];
        let mut i = 0;
        while i < src.len() {
            bytes[i] = match src[i] {
                c @ (b'a'..=b'z' | b'0'..=b'9' | b'-') => c,
                c @ b'A'..=b'Z' => c.to_ascii_lowercase(),
                _ => return None,
            };
            i += 1;
        }
        Some(Hostname { len: src.len() as u8, bytes })
    }

    /// The compile-time default; a constant that breaks the rule is a
    /// build error.
    pub const DEFAULT: Hostname = Hostname::new(DEFAULT_HOSTNAME).expect("DEFAULT_HOSTNAME: 1 to 24 of a-z, 0-9 and inner dashes");

    pub fn as_str(&self) -> &str {
        // Only ASCII gets in, so this cannot fail.
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }
}

impl core::fmt::Debug for Hostname {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// A short string stored in place: a length byte and `N` bytes of
/// UTF-8. The settings are `Copy`, so everything in them is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Text<const N: usize> {
    len: u8,
    bytes: [u8; N],
}

impl<const N: usize> Text<N> {
    pub const EMPTY: Self = Text { len: 0, bytes: [0; N] };

    /// Store a string of up to `N` bytes.
    pub fn new(s: &str) -> Option<Self> {
        let src = s.as_bytes();
        if src.len() > N {
            return None;
        }
        let mut bytes = [0u8; N];
        bytes[..src.len()].copy_from_slice(src);
        Some(Text { len: src.len() as u8, bytes })
    }

    /// `new` for a build-time constant; the length is checked at compile time.
    pub const fn literal(s: &str) -> Self {
        let src = s.as_bytes();
        assert!(src.len() <= N, "build-time string too long");
        let mut bytes = [0u8; N];
        let mut i = 0;
        while i < src.len() {
            bytes[i] = src[i];
            i += 1;
        }
        Text { len: src.len() as u8, bytes }
    }

    /// From a stored length and field; `None` if the length is past the
    /// field or the bytes are not UTF-8.
    fn from_record(len: u8, field: &[u8]) -> Option<Self> {
        Self::new(core::str::from_utf8(field.get(..len as usize)?).ok()?)
    }

    pub fn as_str(&self) -> &str {
        // Only `&str` gets in, so this cannot fail.
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const N: usize> core::fmt::Debug for Text<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// A `Text` whose value never reaches a log line: the Wi-Fi password
/// and the API token print as set or not.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Secret<const N: usize>(pub Text<N>);

impl<const N: usize> core::fmt::Debug for Secret<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(if self.0.is_empty() { "<none>" } else { "<set>" })
    }
}

/// Longest SSID and password the driver takes.
pub const SSID_MAX: usize = 32;
pub const PASSWORD_MAX: usize = 64;

/// A WPA2 password is at least this long; shorter is refused by the
/// driver. Empty means an open network.
pub const PASSWORD_MIN: usize = 8;

/// The network the station joins. `NONE` (an empty SSID) means the
/// clock has no network and raises its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Credentials {
    pub ssid: Text<SSID_MAX>,
    pub password: Secret<PASSWORD_MAX>,
}

impl Credentials {
    pub const NONE: Credentials = Credentials { ssid: Text::EMPTY, password: Secret(Text::EMPTY) };

    /// The build-time seed, checked at compile time.
    const BUILT_IN: Credentials =
        Credentials { ssid: Text::literal(WIFI_SSID), password: Secret(Text::literal(WIFI_PASSWORD)) };

    pub fn is_none(&self) -> bool {
        self.ssid.is_empty()
    }

    /// The password is empty (open network) or WPA2 length; the SSID is
    /// any length the field holds, empty for none.
    pub fn is_valid(&self) -> bool {
        valid_password(self.password.0.as_str())
    }
}

/// Empty (open network) or `PASSWORD_MIN` to `PASSWORD_MAX` bytes.
pub fn valid_password(p: &str) -> bool {
    p.is_empty() || (PASSWORD_MIN..=PASSWORD_MAX).contains(&p.len())
}

/// Longest and shortest API token. Shorter than the minimum is refused;
/// empty means none, which closes the script paths.
pub const TOKEN_MAX: usize = 32;
pub const TOKEN_MIN: usize = 8;

/// Empty, or `TOKEN_MIN` to `TOKEN_MAX` bytes of printable ASCII without
/// spaces, so it survives a shell and a header.
pub const fn valid_token(t: &str) -> bool {
    let b = t.as_bytes();
    if b.is_empty() {
        return true;
    }
    if b.len() < TOKEN_MIN || b.len() > TOKEN_MAX {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        if b[i] <= b' ' || b[i] > b'~' {
            return false;
        }
        i += 1;
    }
    true
}

const _: () = assert!(WIFI_SSID.len() <= SSID_MAX, "WIFI_SSID is longer than 32 bytes");
const _: () = assert!(
    WIFI_PASSWORD.is_empty() || (WIFI_PASSWORD.len() >= PASSWORD_MIN && WIFI_PASSWORD.len() <= PASSWORD_MAX),
    "WIFI_PASSWORD must be empty or 8 to 64 bytes"
);
const _: () = assert!(valid_token(API_TOKEN), "API_TOKEN must be empty or 8 to 32 printable characters without spaces");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    pub color: usize,
    pub brightness: usize,
    pub alarm_hour: u8,
    pub alarm_minute: u8,
    pub alarm_armed: bool,
    /// 12 hour time with the AM mark instead of 24 hour time.
    pub twelve_hour: bool,
    /// Minutes east of UTC.
    pub tz_offset_minutes: i16,
    /// Name sent to the DHCP server.
    pub hostname: Hostname,
    /// Days the alarm rings on: bit 0 Monday to bit 6 Sunday.
    pub alarm_days: u8,
    /// Index into `melody::MELODIES`, or `melody::RANDOM`.
    pub melody: usize,
    /// Skip the next alarm that would ring, then clear. Not stored: a
    /// reboot clears it, and `to_record` leaves it out so toggling it
    /// does not write flash.
    pub skip_next: bool,
    /// The network to join; `Credentials::NONE` for none.
    pub wifi: Credentials,
    /// The API token; empty for none. Never shown.
    pub token: Secret<TOKEN_MAX>,
    /// The token was cleared on purpose (`POST /token` with an empty
    /// value), so the build's `API_TOKEN` must not fill it back in at
    /// boot; a token that was never set is filled. Not a form or API
    /// key, not in the JSON; stored as a flag bit.
    pub token_cleared: bool,
}

/// Bytes in a stored record.
pub const RECORD_LEN: usize = 168;

/// Stored record layout version. Bump when the layout changes, append
/// the new fields and teach `from_record` the old one, or the user's
/// settings are lost; never reorder, since an older image reads a newer
/// record by its prefix.
const RECORD_VERSION: u8 = 7;

/// Where the hostname sits in a version 3 record.
const RECORD_HOSTNAME_LEN: usize = 9;
const RECORD_HOSTNAME: usize = 10;

/// Where the alarm days sit in a version 4 record.
const RECORD_ALARM_DAYS: usize = 34;

/// Where the melody index sits in a version 5 record.
const RECORD_MELODY: usize = 35;

/// The melody byte for `melody::RANDOM`: out of any table's reach, so a
/// tune added later does not change what a stored record means.
const RECORD_MELODY_RANDOM: u8 = 0xff;

/// Where the credentials and the token sit in a version 6 record: each
/// a length byte and its field.
const RECORD_SSID_LEN: usize = 36;
const RECORD_SSID: usize = 37;
const RECORD_PASSWORD_LEN: usize = RECORD_SSID + SSID_MAX;
const RECORD_PASSWORD: usize = RECORD_PASSWORD_LEN + 1;
const RECORD_TOKEN_LEN: usize = RECORD_PASSWORD + PASSWORD_MAX;
const RECORD_TOKEN: usize = RECORD_TOKEN_LEN + 1;

/// Where the flags byte sits in a version 7 record. Bits this image
/// does not know are ignored, so a later image may add some.
const RECORD_FLAGS: usize = RECORD_TOKEN + TOKEN_MAX;
const FLAG_TOKEN_CLEARED: u8 = 1 << 0;
const _: () = assert!(RECORD_FLAGS + 1 == RECORD_LEN, "RECORD_LEN does not match the layout");

/// Day names, Monday first, for the page and the API.
pub const DAY_NAMES: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// Every day of the week.
pub const DAYS_ALL: u8 = 0x7f;

/// Monday to Friday.
pub const DAYS_WEEKDAYS: u8 = 0x1f;

/// Saturday and Sunday.
pub const DAYS_WEEKEND: u8 = 0x60;

impl Settings {
    /// The compile-time defaults.
    pub const DEFAULT: Settings = Settings {
        color: DEFAULT_COLOR,
        // The brightest offered step when the ceiling is below the default.
        brightness: if DEFAULT_BRIGHTNESS < BRIGHTNESS_STEPS.len() {
            DEFAULT_BRIGHTNESS
        } else {
            BRIGHTNESS_STEPS.len() - 1
        },
        alarm_hour: DEFAULT_ALARM.0,
        alarm_minute: DEFAULT_ALARM.1,
        alarm_armed: false,
        twelve_hour: DEFAULT_TWELVE_HOUR,
        tz_offset_minutes: DEFAULT_TZ_OFFSET_MINUTES,
        hostname: Hostname::DEFAULT,
        alarm_days: DAYS_ALL,
        melody: 0,
        skip_next: false,
        wifi: Credentials::NONE,
        token: Secret(Text::EMPTY),
        token_cleared: false,
    };

    /// Fill in what the build has and the record lacks: the network and
    /// the token. Run once on the loaded value before `init`; the next
    /// save then carries them in the record, after which the record wins
    /// over a later build's constants. A token cleared on purpose
    /// (`token_cleared`) stays empty.
    pub fn seed(&mut self) {
        if self.wifi.is_none() && !Credentials::BUILT_IN.is_none() {
            self.wifi = Credentials::BUILT_IN;
        }
        if self.token.0.is_empty() && !self.token_cleared && !API_TOKEN.is_empty() {
            self.token = Secret(Text::literal(API_TOKEN));
        }
    }

    pub fn color(&self) -> Levels {
        PALETTE[self.color % PALETTE.len()]
    }

    pub fn brightness_pct(&self) -> u8 {
        BRIGHTNESS_STEPS[self.brightness % BRIGHTNESS_STEPS.len()]
    }

    /// The current step the brightness means. Within the build's ceiling
    /// by construction of `BRIGHTNESS_STEPS`.
    pub fn current_step(&self) -> u8 {
        BRIGHTNESS_CHOICES[self.brightness % BRIGHTNESS_STEPS.len()].1
    }

    /// Every field within its range.
    pub fn is_valid(&self) -> bool {
        self.color < PALETTE.len()
            && self.brightness < BRIGHTNESS_STEPS.len()
            && self.alarm_hour < 24
            && self.alarm_minute < 60
            && (-TZ_LIMIT_MINUTES..=TZ_LIMIT_MINUTES).contains(&self.tz_offset_minutes)
            && self.alarm_days != 0
            && self.alarm_days & !DAYS_ALL == 0
            && self.melody <= melody::RANDOM
            && self.wifi.is_valid()
            && valid_token(self.token.0.as_str())
    }

    /// Whether the alarm rings on `weekday` (0 Monday to 6 Sunday).
    pub fn alarm_on(&self, weekday: u8) -> bool {
        self.alarm_days & (1 << (weekday % 7)) != 0
    }

    /// The flash record: version, colour, brightness, alarm hour, alarm
    /// minute, armed, twelve-hour, timezone offset as little-endian i16,
    /// hostname length, hostname padded to 24 bytes, alarm days, melody
    /// (`0xff` for random), then the SSID, the password and the token,
    /// each a length byte and a padded field, then the flags byte.
    pub fn to_record(&self) -> [u8; RECORD_LEN] {
        let tz = self.tz_offset_minutes.to_le_bytes();
        let mut r = [0u8; RECORD_LEN];
        r[..9].copy_from_slice(&[
            RECORD_VERSION,
            self.color as u8,
            self.brightness as u8,
            self.alarm_hour,
            self.alarm_minute,
            self.alarm_armed as u8,
            self.twelve_hour as u8,
            tz[0],
            tz[1],
        ]);
        r[RECORD_HOSTNAME_LEN] = self.hostname.len;
        r[RECORD_HOSTNAME..RECORD_HOSTNAME + HOSTNAME_MAX].copy_from_slice(&self.hostname.bytes);
        r[RECORD_ALARM_DAYS] = self.alarm_days;
        r[RECORD_MELODY] = if self.melody == melody::RANDOM { RECORD_MELODY_RANDOM } else { self.melody as u8 };
        r[RECORD_SSID_LEN] = self.wifi.ssid.len;
        r[RECORD_SSID..RECORD_SSID + SSID_MAX].copy_from_slice(&self.wifi.ssid.bytes);
        r[RECORD_PASSWORD_LEN] = self.wifi.password.0.len;
        r[RECORD_PASSWORD..RECORD_PASSWORD + PASSWORD_MAX].copy_from_slice(&self.wifi.password.0.bytes);
        r[RECORD_TOKEN_LEN] = self.token.0.len;
        r[RECORD_TOKEN..RECORD_TOKEN + TOKEN_MAX].copy_from_slice(&self.token.0.bytes);
        r[RECORD_FLAGS] = if self.token_cleared { FLAG_TOKEN_CLEARED } else { 0 };
        r
    }

    /// Parse a flash record. Older versions lack the later fields, which
    /// take the defaults: version 1 (M5) stops after `armed`, version 2
    /// (M6) after the timezone, version 3 (M6) after the hostname, version
    /// 4 (M7) after the alarm days, version 5 (M8) after the melody, so
    /// those read as no network and no token, and version 6 (M10) after
    /// the token, so no flags. Versions are append-only, so a record
    /// from a newer image is read by the prefix this one knows, and a
    /// colour, brightness or melody index past this image's table (an
    /// entry a newer image added, or a higher current ceiling) lands on
    /// the last one offered, so that a rollback keeps the user's settings
    /// instead of starting over. `None` for a field that is out of range
    /// whatever the version, so a bad record falls back to the defaults.
    pub fn from_record(r: &[u8]) -> Option<Settings> {
        let mut s = Settings {
            color: *r.get(1)? as usize,
            brightness: *r.get(2)? as usize,
            alarm_hour: *r.get(3)?,
            alarm_minute: *r.get(4)?,
            alarm_armed: *r.get(5)? != 0,
            ..Settings::DEFAULT
        };
        let version = r[0];
        if version >= 2 {
            s.twelve_hour = *r.get(6)? != 0;
            s.tz_offset_minutes = i16::from_le_bytes([*r.get(7)?, *r.get(8)?]);
        }
        if version >= 3 {
            let len = *r.get(RECORD_HOSTNAME_LEN)? as usize;
            let bytes = r.get(RECORD_HOSTNAME..RECORD_HOSTNAME + HOSTNAME_MAX)?;
            let name = core::str::from_utf8(bytes.get(..len)?).ok()?;
            s.hostname = Hostname::new(name)?;
        }
        if version >= 4 {
            s.alarm_days = *r.get(RECORD_ALARM_DAYS)?;
        }
        if version >= 5 {
            s.melody = match *r.get(RECORD_MELODY)? {
                RECORD_MELODY_RANDOM => melody::RANDOM,
                m => (m as usize).min(melody::MELODIES.len() - 1),
            };
        }
        if version >= 6 {
            s.wifi = Credentials {
                ssid: Text::from_record(*r.get(RECORD_SSID_LEN)?, r.get(RECORD_SSID..RECORD_SSID + SSID_MAX)?)?,
                password: Secret(Text::from_record(
                    *r.get(RECORD_PASSWORD_LEN)?,
                    r.get(RECORD_PASSWORD..RECORD_PASSWORD + PASSWORD_MAX)?,
                )?),
            };
            s.token = Secret(Text::from_record(*r.get(RECORD_TOKEN_LEN)?, r.get(RECORD_TOKEN..RECORD_TOKEN + TOKEN_MAX)?)?);
        }
        if version >= 7 {
            s.token_cleared = *r.get(RECORD_FLAGS)? & FLAG_TOKEN_CLEARED != 0;
        }
        s.color = s.color.min(PALETTE.len() - 1);
        // A record saved by a build with a higher ceiling may name a step
        // this build does not offer: land on the brightest one instead of
        // throwing the whole record away. Anything past the full list is
        // still a bad record.
        if s.brightness < BRIGHTNESS_CHOICES.len() && s.brightness >= BRIGHTNESS_STEPS.len() {
            s.brightness = BRIGHTNESS_STEPS.len() - 1;
        }
        s.is_valid().then_some(s)
    }
}

/// Receivers: the display task, the persist task and the hostname task.
const RECEIVERS: usize = 3;

static WATCH: Watch<CriticalSectionRawMutex, Settings, RECEIVERS> = Watch::new();

pub type SettingsReceiver = Receiver<'static, CriticalSectionRawMutex, Settings, RECEIVERS>;

/// Publish the boot-time value. Call once before anything reads.
pub fn init(settings: Settings) {
    WATCH.sender().send(settings);
}

/// The current settings.
pub fn get() -> Settings {
    WATCH.try_get().unwrap_or(Settings::DEFAULT)
}

/// Apply `f` to the settings and notify receivers if anything changed.
/// Returns whether it did.
pub fn update(f: impl FnOnce(&mut Settings)) {
    // The watch wants a `Fn`; the closure runs exactly once under its lock.
    let f = RefCell::new(Some(f));
    WATCH.sender().send_if_modified(|slot| {
        let before = slot.unwrap_or(Settings::DEFAULT);
        let mut after = before;
        if let Some(f) = f.borrow_mut().take() {
            f(&mut after);
        }
        let modified = after != before || slot.is_none();
        if modified {
            *slot = Some(after);
        }
        modified
    });
}

/// A receiver that wakes on every change. Panics past `RECEIVERS`.
pub fn receiver() -> SettingsReceiver {
    WATCH.receiver().expect("too many settings receivers")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A settings value with every field away from its default and every
    /// string at its longest.
    fn full() -> Settings {
        Settings {
            color: PALETTE.len() - 1,
            brightness: BRIGHTNESS_STEPS.len() - 1,
            alarm_hour: 23,
            alarm_minute: 59,
            alarm_armed: true,
            twelve_hour: true,
            tz_offset_minutes: -420,
            hostname: Hostname::new(&"h".repeat(HOSTNAME_MAX)).unwrap(),
            alarm_days: 0b0101010,
            melody: melody::RANDOM,
            skip_next: false,
            wifi: Credentials {
                ssid: Text::new(&"s".repeat(SSID_MAX)).unwrap(),
                password: Secret(Text::new(&"p".repeat(PASSWORD_MAX)).unwrap()),
            },
            token: Secret(Text::new(&"t".repeat(TOKEN_MAX)).unwrap()),
            token_cleared: false,
        }
    }

    #[test]
    fn hostname_rule() {
        assert_eq!(Hostname::new("Clock-01").map(|h| h.as_str().to_string()), Some("clock-01".to_string()));
        assert!(Hostname::new("").is_none());
        assert!(Hostname::new("-a").is_none());
        assert!(Hostname::new("a-").is_none());
        assert!(Hostname::new("a_b").is_none());
        assert!(Hostname::new("a.b").is_none());
        assert!(Hostname::new("a b").is_none());
        assert!(Hostname::new(&"a".repeat(HOSTNAME_MAX)).is_some());
        assert!(Hostname::new(&"a".repeat(HOSTNAME_MAX + 1)).is_none());
        assert_eq!(Hostname::DEFAULT.as_str(), DEFAULT_HOSTNAME);
    }

    #[test]
    fn palette_is_the_old_gamma_of_the_old_rgb() {
        // The palette was 8-bit RGB through a gamma table until 2026-10-29;
        // the table mapped 255 to 7, 96 to 1 and 0 to 0.
        let old_rgb = [(255, 0, 0), (255, 96, 0), (255, 255, 0), (0, 255, 0), (0, 255, 255), (0, 0, 255), (255, 0, 255), (255, 255, 255)];
        let g = |v: u8| match v {
            255 => MAX_LEVEL,
            96 => 1,
            _ => 0,
        };
        for (i, (r, gr, b)) in old_rgb.iter().enumerate() {
            assert_eq!(PALETTE[i], Levels::new(g(*b), g(*gr), g(*r)), "{}", PALETTE_NAMES[i]);
        }
        assert_eq!(PALETTE.len(), PALETTE_NAMES.len());
    }

    #[test]
    fn brightness_choices_stay_within_the_ceiling() {
        assert!(!BRIGHTNESS_STEPS.is_empty());
        for (i, choice) in BRIGHTNESS_CHOICES.iter().enumerate().take(BRIGHTNESS_STEPS.len()) {
            let s = Settings { brightness: i, ..Settings::DEFAULT };
            assert_eq!(s.brightness_pct(), choice.0);
            assert!(s.current_step() <= MAX_CURRENT_STEP, "{} % is step {}", s.brightness_pct(), s.current_step());
        }
        // The list is ascending in both percent and step, and the choice
        // past the ceiling, if there is one, is really past it.
        assert!(BRIGHTNESS_CHOICES.windows(2).all(|w| w[0].0 < w[1].0 && w[0].1 < w[1].1));
        if let Some(next) = BRIGHTNESS_CHOICES.get(BRIGHTNESS_STEPS.len()) {
            assert!(next.1 > MAX_CURRENT_STEP);
        }
        assert!(Settings::DEFAULT.brightness < BRIGHTNESS_STEPS.len());
        assert!(Settings::DEFAULT.is_valid());
    }

    #[test]
    fn record_round_trip() {
        let s = full();
        let r = s.to_record();
        assert_eq!(r[0], RECORD_VERSION);
        assert_eq!(Settings::from_record(&r), Some(s));
        // `skip_next` is not stored.
        let mut skipping = s;
        skipping.skip_next = true;
        assert_eq!(skipping.to_record(), r);
    }

    /// The length of a record of each older version: where its last
    /// field ends.
    fn prefix_len(version: u8) -> usize {
        match version {
            1 => 6,
            2 => 9,
            3 => RECORD_HOSTNAME + HOSTNAME_MAX,
            4 => RECORD_ALARM_DAYS + 1,
            5 => RECORD_MELODY + 1,
            6 => RECORD_FLAGS,
            _ => RECORD_LEN,
        }
    }

    #[test]
    fn older_records_read_with_defaults_for_what_they_lack() {
        let s = full();
        let r = s.to_record();
        for version in 1..RECORD_VERSION {
            let mut old = r[..prefix_len(version)].to_vec();
            old[0] = version;
            let got = Settings::from_record(&old).unwrap_or_else(|| panic!("version {version} rejected"));
            let d = Settings::DEFAULT;
            assert_eq!((got.color, got.brightness, got.alarm_hour, got.alarm_minute, got.alarm_armed), (s.color, s.brightness, 23, 59, true));
            assert_eq!(got.twelve_hour, if version >= 2 { s.twelve_hour } else { d.twelve_hour }, "v{version}");
            assert_eq!(got.tz_offset_minutes, if version >= 2 { s.tz_offset_minutes } else { d.tz_offset_minutes });
            assert_eq!(got.hostname, if version >= 3 { s.hostname } else { d.hostname });
            assert_eq!(got.alarm_days, if version >= 4 { s.alarm_days } else { d.alarm_days });
            assert_eq!(got.melody, if version >= 5 { s.melody } else { d.melody });
            assert_eq!(got.wifi, if version >= 6 { s.wifi } else { Credentials::NONE }, "v{version} network");
            assert_eq!(got.token, if version >= 6 { s.token } else { Secret(Text::EMPTY) }, "v{version} token");
            assert!(!got.token_cleared, "v{version} has no flags");
        }
    }

    #[test]
    fn newer_record_is_read_by_its_known_prefix() {
        let s = full();
        let mut r = s.to_record().to_vec();
        r[0] = RECORD_VERSION + 1;
        r.extend_from_slice(&[0xAA; 40]);
        assert_eq!(Settings::from_record(&r), Some(s));
        // Flag bits a later image may add are ignored.
        let mut flagged = s.to_record();
        flagged[RECORD_FLAGS] |= 0xFE;
        assert_eq!(Settings::from_record(&flagged), Some(s));
    }

    #[test]
    fn bad_records_are_rejected_and_table_indexes_clamped() {
        let s = full();
        let r = s.to_record();
        let with = |f: &dyn Fn(&mut [u8; RECORD_LEN])| {
            let mut r = r;
            f(&mut r);
            Settings::from_record(&r)
        };
        assert!(with(&|r| r[RECORD_SSID_LEN] = SSID_MAX as u8 + 1).is_none(), "SSID length past the field");
        assert!(with(&|r| r[RECORD_PASSWORD_LEN] = PASSWORD_MIN as u8 - 1).is_none(), "password of 7");
        assert!(with(&|r| r[RECORD_PASSWORD_LEN] = PASSWORD_MAX as u8 + 1).is_none(), "password of 65");
        assert!(with(&|r| r[RECORD_TOKEN_LEN] = TOKEN_MIN as u8 - 1).is_none(), "token of 7");
        assert!(with(&|r| r[RECORD_TOKEN] = b' ').is_none(), "token with a space");
        assert!(with(&|r| r[RECORD_SSID] = 0xFF).is_none(), "SSID not UTF-8");
        assert!(with(&|r| r[3] = 24).is_none(), "alarm hour 24");
        assert!(with(&|r| r[4] = 60).is_none(), "alarm minute 60");
        assert!(with(&|r| r[2] = BRIGHTNESS_CHOICES.len() as u8).is_none(), "brightness past the whole list");
        assert!(with(&|r| r[RECORD_ALARM_DAYS] = 0).is_none(), "no alarm day");
        assert!(with(&|r| r[RECORD_ALARM_DAYS] = 0x80).is_none(), "an eighth day");
        assert!(with(&|r| r[7..9].copy_from_slice(&(TZ_LIMIT_MINUTES + 1).to_le_bytes())).is_none(), "timezone past the limit");
        // Indexes a newer image may have added land on the last entry.
        assert_eq!(with(&|r| r[1] = 200).map(|s| s.color), Some(PALETTE.len() - 1));
        assert_eq!(with(&|r| r[RECORD_MELODY] = 200).map(|s| s.melody), Some(melody::MELODIES.len() - 1));
        // A record from a build with a higher current ceiling.
        if BRIGHTNESS_STEPS.len() < BRIGHTNESS_CHOICES.len() {
            assert_eq!(with(&|r| r[2] = BRIGHTNESS_CHOICES.len() as u8 - 1).map(|s| s.brightness), Some(BRIGHTNESS_STEPS.len() - 1));
        }
        // An empty network and token are fine: no network, no token.
        let open = with(&|r| {
            r[RECORD_SSID_LEN] = 0;
            r[RECORD_PASSWORD_LEN] = 0;
            r[RECORD_TOKEN_LEN] = 0;
        })
        .unwrap();
        assert!(open.wifi.is_none() && open.token.0.is_empty());
    }

    #[test]
    fn seed_fills_only_what_is_empty() {
        let mut s = full();
        s.seed();
        assert_eq!(s, full());
        let mut d = Settings::DEFAULT;
        d.seed();
        assert_eq!(d.wifi.is_none(), WIFI_SSID.is_empty());
        assert_eq!(d.token.0.is_empty(), API_TOKEN.is_empty());
        // A token cleared on purpose is not filled back in, and the
        // flag survives the record.
        let mut cleared = Settings { token: Secret(Text::EMPTY), token_cleared: true, ..full() };
        cleared.seed();
        assert!(cleared.token.0.is_empty());
        assert!(cleared.token_cleared);
        assert_eq!(Settings::from_record(&cleared.to_record()), Some(cleared));
        assert_eq!(cleared.to_record()[RECORD_FLAGS], FLAG_TOKEN_CLEARED);
        assert_eq!(full().to_record()[RECORD_FLAGS], 0);
    }

    #[test]
    fn debug_never_shows_a_secret() {
        let s = full();
        let text = format!("{s:?}");
        assert!(text.contains(&"s".repeat(SSID_MAX)), "the SSID is shown");
        assert!(!text.contains("pppp"), "the password is hidden");
        assert!(!text.contains("tttt"), "the token is hidden");
        assert_eq!(format!("{:?}", s.token), "<set>");
        assert_eq!(format!("{:?}", Secret(Text::<8>::EMPTY)), "<none>");
        assert_eq!(format!("{:?}", s.hostname), format!("{:?}", s.hostname.as_str()));
    }

    #[test]
    fn token_and_password_rules() {
        assert!(valid_token(""));
        assert!(!valid_token(&"a".repeat(TOKEN_MIN - 1)));
        assert!(valid_token(&"a".repeat(TOKEN_MIN)));
        assert!(valid_token(&"~".repeat(TOKEN_MAX)));
        assert!(!valid_token(&"a".repeat(TOKEN_MAX + 1)));
        assert!(!valid_token("abcd efgh"));
        assert!(!valid_token("abcdefg\x7f"));
        assert!(!valid_token("abcdefgé"));
        assert!(valid_password(""));
        assert!(!valid_password(&"p".repeat(PASSWORD_MIN - 1)));
        assert!(valid_password(&"p".repeat(PASSWORD_MIN)));
        assert!(valid_password(&"p".repeat(PASSWORD_MAX)));
        assert!(!valid_password(&"p".repeat(PASSWORD_MAX + 1)));
        assert!(Text::<4>::new("abcde").is_none());
        assert_eq!(Text::<4>::new("abcd").map(|t| t.len()), Some(4));
    }

    #[test]
    fn is_valid_at_the_edges() {
        let ok = full();
        assert!(ok.is_valid());
        let tz = |m| Settings { tz_offset_minutes: m, ..ok };
        assert!(tz(TZ_LIMIT_MINUTES).is_valid() && tz(-TZ_LIMIT_MINUTES).is_valid());
        assert!(!tz(TZ_LIMIT_MINUTES + 1).is_valid() && !tz(-TZ_LIMIT_MINUTES - 1).is_valid());
        assert!(!Settings { color: PALETTE.len(), ..ok }.is_valid());
        assert!(!Settings { brightness: BRIGHTNESS_STEPS.len(), ..ok }.is_valid());
        assert!(Settings { melody: melody::RANDOM, ..ok }.is_valid());
        assert!(!Settings { melody: melody::RANDOM + 1, ..ok }.is_valid());
        assert!(Settings { alarm_days: DAYS_WEEKEND, ..ok }.alarm_on(5));
        assert!(!Settings { alarm_days: DAYS_WEEKEND, ..ok }.alarm_on(4));
        assert_eq!(RECORD_LEN, ok.to_record().len());
    }

    #[test]
    fn update_publishes_and_get_reads() {
        update(|s| s.color = 3);
        assert_eq!(get().color, 3);
        update(|s| s.color = 0);
        assert_eq!(get().color, 0);
    }
}

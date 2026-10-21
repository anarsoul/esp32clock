//! The user's settings: one shared value with a change signal, and the
//! record they are stored as in flash.
//!
//! Anything may change the settings through [`update`]; the display and
//! the persist task each hold a [`Receiver`] and wake when something did.
//! Pure logic apart from the `embassy-sync` watch, so host-testable.

use core::cell::{Cell, RefCell};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Receiver, Watch};

use crate::config::{
    DEFAULT_ALARM, DEFAULT_BRIGHTNESS, DEFAULT_COLOR, DEFAULT_HOSTNAME, DEFAULT_TWELVE_HOUR, DEFAULT_TZ_OFFSET_MINUTES,
};
use crate::melody;
use crate::panel::Rgb;

/// Colours the top button cycles through.
pub const PALETTE: [Rgb; 8] = [
    Rgb::RED,
    Rgb::ORANGE,
    Rgb::YELLOW,
    Rgb::GREEN,
    Rgb::CYAN,
    Rgb::BLUE,
    Rgb::MAGENTA,
    Rgb::WHITE,
];

/// Names of the palette entries, for the page and the API.
pub const PALETTE_NAMES: [&str; 8] = ["red", "orange", "yellow", "green", "cyan", "blue", "magenta", "white"];

/// Brightness steps the bottom button cycles through, in percent.
pub const BRIGHTNESS_STEPS: [u8; 5] = [10, 25, 50, 75, 100];

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
    /// Validate and store a name, lower-casing ASCII letters.
    pub fn new(name: &str) -> Option<Hostname> {
        let src = name.as_bytes();
        if src.is_empty() || src.len() > HOSTNAME_MAX || src[0] == b'-' || src[src.len() - 1] == b'-' {
            return None;
        }
        let mut bytes = [0u8; HOSTNAME_MAX];
        for (dst, &c) in bytes.iter_mut().zip(src) {
            *dst = match c {
                b'a'..=b'z' | b'0'..=b'9' | b'-' => c,
                b'A'..=b'Z' => c.to_ascii_lowercase(),
                _ => return None,
            };
        }
        Some(Hostname { len: src.len() as u8, bytes })
    }

    /// The compile-time default; the constant is checked at build time.
    pub const DEFAULT: Hostname = {
        let src = DEFAULT_HOSTNAME.as_bytes();
        assert!(!src.is_empty() && src.len() <= HOSTNAME_MAX, "DEFAULT_HOSTNAME length");
        let mut bytes = [0u8; HOSTNAME_MAX];
        let mut i = 0;
        while i < src.len() {
            let c = src[i];
            assert!(
                (c >= b'a' && c <= b'z') || (c >= b'0' && c <= b'9') || (c == b'-' && i != 0 && i != src.len() - 1),
                "DEFAULT_HOSTNAME must be a-z, 0-9 and inner dashes"
            );
            bytes[i] = c;
            i += 1;
        }
        Hostname { len: src.len() as u8, bytes }
    };

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
    /// Index into `melody::MELODIES`.
    pub melody: usize,
}

/// Bytes in a stored record.
pub const RECORD_LEN: usize = 36;

/// Stored record layout version. Bump when the layout changes and teach
/// `from_record` the old one, or the user's settings are lost.
const RECORD_VERSION: u8 = 5;

/// Where the hostname sits in a version 3 record.
const RECORD_HOSTNAME_LEN: usize = 9;
const RECORD_HOSTNAME: usize = 10;

/// Where the alarm days sit in a version 4 record.
const RECORD_ALARM_DAYS: usize = 34;

/// Where the melody index sits in a version 5 record.
const RECORD_MELODY: usize = 35;

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
        brightness: DEFAULT_BRIGHTNESS,
        alarm_hour: DEFAULT_ALARM.0,
        alarm_minute: DEFAULT_ALARM.1,
        alarm_armed: false,
        twelve_hour: DEFAULT_TWELVE_HOUR,
        tz_offset_minutes: DEFAULT_TZ_OFFSET_MINUTES,
        hostname: Hostname::DEFAULT,
        alarm_days: DAYS_ALL,
        melody: 0,
    };

    pub fn color(&self) -> Rgb {
        PALETTE[self.color % PALETTE.len()]
    }

    pub fn brightness_pct(&self) -> u8 {
        BRIGHTNESS_STEPS[self.brightness % BRIGHTNESS_STEPS.len()]
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
            && self.melody < melody::MELODIES.len()
    }

    /// Whether the alarm rings on `weekday` (0 Monday to 6 Sunday).
    pub fn alarm_on(&self, weekday: u8) -> bool {
        self.alarm_days & (1 << (weekday % 7)) != 0
    }

    /// The flash record: version, colour, brightness, alarm hour, alarm
    /// minute, armed, twelve-hour, timezone offset as little-endian i16,
    /// hostname length, hostname padded to 24 bytes, alarm days, melody.
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
        r[RECORD_MELODY] = self.melody as u8;
        r
    }

    /// Parse a flash record. Older versions lack the later fields, which
    /// take the defaults: version 1 (M5) stops after `armed`, version 2
    /// (M6) after the timezone, version 3 (M6) after the hostname, version
    /// 4 (M7) after the alarm days. `None`
    /// for an unknown version or a field out of range, so a bad record
    /// falls back to the defaults.
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
            s.melody = *r.get(RECORD_MELODY)? as usize;
        }
        if version > RECORD_VERSION {
            return None;
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
pub fn update(f: impl FnOnce(&mut Settings)) -> bool {
    // The watch wants a `Fn`; the closure runs exactly once under its lock.
    let f = RefCell::new(Some(f));
    let changed = Cell::new(false);
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
        changed.set(modified);
        modified
    });
    changed.get()
}

/// A receiver that wakes on every change. Panics past `RECEIVERS`.
pub fn receiver() -> SettingsReceiver {
    WATCH.receiver().expect("too many settings receivers")
}

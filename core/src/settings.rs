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
    API_TOKEN, DEFAULT_ALARM, DEFAULT_BRIGHTNESS, DEFAULT_COLOR, DEFAULT_HOSTNAME, DEFAULT_TEMP_OFFSET_DC, DEFAULT_TWELVE_HOUR,
    DEFAULT_TX_POWER_QDBM, DEFAULT_TZ_OFFSET_MINUTES, DEFAULT_TZ_RULE, MAX_CURRENT_STEP, MQTT_HOST, MQTT_PASSWORD, MQTT_PORT, MQTT_USER, WIFI_MAX_TX_POWER_QDBM, WIFI_PASSWORD, WIFI_SSID,
};
use crate::melody;
use crate::panel::{Levels, MAX_LEVEL};
use crate::tz::{self, TZ_MAX};

/// A channel fully lit.
const F: u8 = MAX_LEVEL;

/// Colours the top button cycles through, as duty levels in the panel's
/// blue-green-red order. Orange has a touch of green. Yellow runs the
/// green at two sevenths: the green die puts out far more light than
/// the red one, and at equal duty the panel showed a pale green, not
/// yellow, and at half green still a tint (2026-11-01). Lime is the
/// equal-duty mix, kept next to yellow so one press compares the two.
pub const PALETTE: [Levels; 9] = [
    Levels::new(0, 0, F), // red
    Levels::new(0, 1, F), // orange
    Levels::new(0, 2, F), // yellow
    Levels::new(0, F, F), // lime
    Levels::new(0, F, 0), // green
    Levels::new(F, F, 0), // cyan
    Levels::new(F, 0, 0), // blue
    Levels::new(F, 0, F), // magenta
    Levels::new(F, F, F), // white
];

/// Names of the palette entries, for the page and the API.
pub const PALETTE_NAMES: [&str; 9] = ["red", "orange", "yellow", "lime", "green", "cyan", "blue", "magenta", "white"];

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

    /// ASCII bytes a `const fn` built, `len` of them; `tz::fixed` writes
    /// a rule this way. The caller keeps the bytes ASCII.
    pub(crate) const fn from_ascii(bytes: [u8; N], len: u8) -> Self {
        Text { len, bytes }
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

    /// A password with no network to use it on, which `/api` refuses
    /// to make.
    pub fn password_alone(&self) -> bool {
        self.ssid.is_empty() && !self.password.0.is_empty()
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

/// Longest broker host (an IPv4 literal or a DNS name; a name is 63
/// bytes a label but this is a LAN), user and password.
pub const BROKER_HOST_MAX: usize = 64;
pub const BROKER_USER_MAX: usize = 32;
pub const BROKER_PASSWORD_MAX: usize = 64;

/// The MQTT broker the clock announces itself to for Home Assistant.
/// `NONE` (an empty host) means MQTT is off.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Broker {
    pub host: Text<BROKER_HOST_MAX>,
    pub port: u16,
    pub user: Text<BROKER_USER_MAX>,
    pub password: Secret<BROKER_PASSWORD_MAX>,
}

impl Broker {
    pub const NONE: Broker = Broker { host: Text::EMPTY, port: MQTT_PORT, user: Text::EMPTY, password: Secret(Text::EMPTY) };

    /// The build-time seed, checked at compile time.
    const BUILT_IN: Broker = Broker {
        host: Text::literal(MQTT_HOST),
        port: MQTT_PORT,
        user: Text::literal(MQTT_USER),
        password: Secret(Text::literal(MQTT_PASSWORD)),
    };

    pub fn is_none(&self) -> bool {
        self.host.is_empty()
    }

    /// The host is empty or a host name, the port is not 0, the user has
    /// no spaces, and both the user and the password are printable.
    pub fn is_valid(&self) -> bool {
        valid_broker_host(self.host.as_str())
            && self.port != 0
            && valid_broker_user(self.user.as_str())
            && valid_broker_password(self.password.0.as_str())
    }
}

/// Empty, or up to `BROKER_HOST_MAX` of letters, digits, dots and
/// dashes: an IPv4 literal or a DNS name, what `dns_query` and the
/// socket take, and nothing a log line or a page need escape.
pub const fn valid_broker_host(h: &str) -> bool {
    let b = h.as_bytes();
    if b.len() > BROKER_HOST_MAX {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        if !(b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'-') {
            return false;
        }
        i += 1;
    }
    true
}

/// Printable ASCII, no spaces, up to the length: a user name that
/// survives a shell and a configuration file.
const fn printable_no_spaces(s: &str, max: usize) -> bool {
    let b = s.as_bytes();
    if b.len() > max {
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

pub const fn valid_broker_user(u: &str) -> bool {
    printable_no_spaces(u, BROKER_USER_MAX)
}

/// Printable ASCII with spaces allowed, up to `BROKER_PASSWORD_MAX`.
pub const fn valid_broker_password(p: &str) -> bool {
    let b = p.as_bytes();
    if b.len() > BROKER_PASSWORD_MAX {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        if b[i] < b' ' || b[i] > b'~' {
            return false;
        }
        i += 1;
    }
    true
}

const _: () = assert!(valid_broker_host(MQTT_HOST), "MQTT_HOST must be up to 64 of letters, digits, dots and dashes");
const _: () = assert!(valid_broker_user(MQTT_USER), "MQTT_USER must be up to 32 printable characters without spaces");
const _: () = assert!(valid_broker_password(MQTT_PASSWORD), "MQTT_PASSWORD must be up to 64 printable characters");

/// Whether the panel shows the temperature for a few seconds each
/// minute, and in which unit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum TempShow {
    Off = 0,
    Celsius = 1,
    Fahrenheit = 2,
}

impl TempShow {
    /// The form and API names, in record-byte order.
    pub const NAMES: [&str; 3] = ["off", "c", "f"];
    /// The page's labels, in the same order.
    pub const LABELS: [&str; 3] = ["Off", "Celsius", "Fahrenheit"];
    const ALL: [TempShow; 3] = [TempShow::Off, TempShow::Celsius, TempShow::Fahrenheit];

    pub fn from_byte(b: u8) -> Option<Self> {
        Self::ALL.get(b as usize).copied()
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::NAMES.iter().position(|n| *n == name).and_then(|i| Self::from_byte(i as u8))
    }

    pub fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }
}

/// A pin a sound can go out of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SoundPin {
    /// GPIO5, open-drain, into the buzzer's transistor.
    Buzzer,
    /// GPIO20, push-pull, into an RC filter and an amplifier.
    Line,
}

/// Where the sound goes: the buzzer's pin, the line output a clock
/// with an amplifier has its filter on, or each kind of sound to its
/// own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum AudioOut {
    /// Everything on the buzzer's pin.
    Buzzer = 0,
    /// Everything on the line output.
    Line = 1,
    /// The note melodies on the buzzer, the clips on the line output.
    Split = 2,
}

impl AudioOut {
    /// The form and API names, in record-byte order.
    pub const NAMES: [&str; 3] = ["buzzer", "line", "split"];
    /// The page's labels, in the same order.
    pub const LABELS: [&str; 3] = ["Buzzer (GPIO5)", "Amplifier (GPIO20)", "Notes: buzzer, clips: amp"];
    pub const ALL: [AudioOut; 3] = [AudioOut::Buzzer, AudioOut::Line, AudioOut::Split];

    /// The pin a note melody's tones go out of.
    pub const fn tones(self) -> SoundPin {
        match self {
            AudioOut::Buzzer | AudioOut::Split => SoundPin::Buzzer,
            AudioOut::Line => SoundPin::Line,
        }
    }

    /// The pin a clip goes out of.
    pub const fn clips(self) -> SoundPin {
        match self {
            AudioOut::Buzzer => SoundPin::Buzzer,
            AudioOut::Line | AudioOut::Split => SoundPin::Line,
        }
    }

    pub fn from_byte(b: u8) -> Option<Self> {
        Self::ALL.get(b as usize).copied()
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::NAMES.iter().position(|n| *n == name).and_then(|i| Self::from_byte(i as u8))
    }

    pub fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }
}

/// The Wi-Fi transmit powers the driver has, as the form and API name
/// them in dBm and as it takes them in quarter dBm (the table in
/// esp-wifi-sys's `esp_wifi_set_max_tx_power` doc; 16 dBm is 66 to 71,
/// and 68 is what the clocks have run at). The 18 and 20 dBm levels are
/// left out: `config::WIFI_MAX_TX_POWER_QDBM` says why.
const TX_POWER_ALL: [(&str, i8); 9] =
    [("2", 8), ("5", 20), ("7", 28), ("8.5", 34), ("11", 44), ("13", 52), ("14", 56), ("15", 60), ("16", 68)];

/// The transmit powers offered: those within the ceiling.
pub const TX_POWER_CHOICES: &[(&str, i8)] = {
    let mut n = 0;
    while n < TX_POWER_ALL.len() && TX_POWER_ALL[n].1 <= WIFI_MAX_TX_POWER_QDBM {
        n += 1;
    }
    TX_POWER_ALL.split_at(n).0
};

/// The quarter dBm of the power named `name` (`8.5`, `16`).
pub fn tx_power_from_name(name: &str) -> Option<i8> {
    TX_POWER_CHOICES.iter().find(|(n, _)| *n == name).map(|(_, q)| *q)
}

/// The name of a power in quarter dBm, if it is one of the choices.
pub const fn tx_power_name(qdbm: i8) -> Option<&'static str> {
    let mut i = 0;
    while i < TX_POWER_CHOICES.len() {
        if TX_POWER_CHOICES[i].1 == qdbm {
            return Some(TX_POWER_CHOICES[i].0);
        }
        i += 1;
    }
    None
}

const _: () = assert!(tx_power_name(DEFAULT_TX_POWER_QDBM).is_some(), "WIFI_TX_POWER_QDBM must be 8, 20, 28, 34, 44, 52, 56, 60 or 68");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    pub color: usize,
    pub brightness: usize,
    pub alarm_hour: u8,
    pub alarm_minute: u8,
    pub alarm_armed: bool,
    /// 12 hour time with the AM mark instead of 24 hour time.
    pub twelve_hour: bool,
    /// The timezone as a POSIX TZ rule (`tz::parse`), evaluated by the
    /// clock for daylight saving. Always parses: `is_valid` checks it.
    pub tz: Text<TZ_MAX>,
    /// Tenths of a degree Celsius added to the thermometer's reading:
    /// the sensor sits in the case's warm air, a little above the room.
    pub temp_offset_dc: i16,
    /// The temperature on the panel each minute, and its unit.
    pub temp_show: TempShow,
    /// Name sent to the DHCP server.
    pub hostname: Hostname,
    /// Days the alarm rings on: bit 0 Monday to bit 6 Sunday.
    pub alarm_days: u8,
    /// Index into the melody table (`melody::get`), or `melody::RANDOM`.
    /// The record keeps it by the melody's name as well, which is what
    /// counts when it is read back: the table is another after a new
    /// pack.
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
    /// The MQTT broker for Home Assistant; `Broker::NONE` for no MQTT.
    pub mqtt: Broker,
    /// The broker was cleared on the page, so the build's `MQTT_HOST`
    /// must not fill it back in at boot. A flag bit like `token_cleared`.
    pub mqtt_cleared: bool,
    /// The Wi-Fi transmit power in quarter dBm, one of
    /// `TX_POWER_CHOICES`, for a clock whose antenna wants less than
    /// the ceiling.
    pub tx_power_qdbm: i8,
    /// The pin the tones and the clips go out of. Read at each tone and
    /// each clip, so a change needs no reset.
    pub output: AudioOut,
}

/// Bytes in a stored record.
pub const RECORD_LEN: usize = 408;

/// The thermometer offset's bound, tenths of a degree either way.
pub const TEMP_OFFSET_LIMIT_DC: i16 = 100;

/// Stored record layout version. Bump when the layout changes, append
/// the new fields and teach `from_record` the old one, or the user's
/// settings are lost; never reorder, since an older image reads a newer
/// record by its prefix. Version 12 is the first public release's, and
/// what it promises an image after it: the fields up to here stay where
/// they are, a value it does not know in one of them costs it that field
/// only (`from_record`), and the record with its key stays within
/// `storage::ITEM_MAX` bytes (1000), which that image's map can read; a
/// record that outgrows it goes on in a second item. Version 13 appends
/// the sound output, version 14 the alarm sound's name.
const RECORD_VERSION: u8 = 14;

/// Whether a stored record is of a layout before this image's: one an
/// older firmware wrote, which the store writes again in this layout at
/// the boot that finds it, so that what a later version added (the alarm
/// sound's name, above all: the next pack may move its clip) is there
/// before anything else changes. Never a newer one's: that is the
/// record of an image this one was rolled back from, and is that
/// image's to keep.
pub fn record_is_older(record: &[u8]) -> bool {
    record.first().is_some_and(|&version| version < RECORD_VERSION)
}

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
const FLAG_MQTT_CLEARED: u8 = 1 << 1;

/// Where the thermometer offset sits in a version 8 record, two bytes
/// little-endian.
const RECORD_TEMP_OFFSET: usize = RECORD_FLAGS + 1;

/// Where the temperature-on-the-panel byte sits in a version 9 record.
const RECORD_TEMP_SHOW: usize = RECORD_TEMP_OFFSET + 2;

/// Where the MQTT broker sits in a version 10 record: the host, a
/// length byte and its field; the port, two bytes little-endian; the
/// user and the password, each a length byte and its field.
const RECORD_BROKER_HOST_LEN: usize = RECORD_TEMP_SHOW + 1;
const RECORD_BROKER_HOST: usize = RECORD_BROKER_HOST_LEN + 1;
const RECORD_BROKER_PORT: usize = RECORD_BROKER_HOST + BROKER_HOST_MAX;
const RECORD_BROKER_USER_LEN: usize = RECORD_BROKER_PORT + 2;
const RECORD_BROKER_USER: usize = RECORD_BROKER_USER_LEN + 1;
const RECORD_BROKER_PASSWORD_LEN: usize = RECORD_BROKER_USER + BROKER_USER_MAX;
const RECORD_BROKER_PASSWORD: usize = RECORD_BROKER_PASSWORD_LEN + 1;

/// Where the timezone rule sits in a version 11 record: a length byte and
/// its field. Bytes 7 and 8 still carry its standard offset in minutes,
/// so an older image shows standard time after a rollback.
const RECORD_TZ_LEN: usize = RECORD_BROKER_PASSWORD + BROKER_PASSWORD_MAX;
const RECORD_TZ: usize = RECORD_TZ_LEN + 1;

/// Where the transmit power sits in a version 12 record: quarter dBm.
const RECORD_TX_POWER: usize = RECORD_TZ + TZ_MAX;

/// Where the sound output sits in a version 13 record.
const RECORD_OUTPUT: usize = RECORD_TX_POWER + 1;

/// Where the alarm sound's API name sits in a version 14 record: a
/// length byte and its field. It is what the sound is kept by; the
/// index at `RECORD_MELODY` stays for an older image, and for a record
/// with no name.
const RECORD_MELODY_NAME_LEN: usize = RECORD_OUTPUT + 1;
const RECORD_MELODY_NAME: usize = RECORD_MELODY_NAME_LEN + 1;
const _: () = assert!(RECORD_MELODY_NAME + crate::pack::NAME_MAX == RECORD_LEN, "RECORD_LEN does not match the layout");

/// The timezone at boot until the user sets it: `TZ_RULE` from the build,
/// checked here so a bad rule fails the build, or else `TZ_OFFSET_MINUTES`
/// as a fixed rule (`<-07>7` for the CI's Vancouver), or UTC.
pub const DEFAULT_TZ: Text<TZ_MAX> = match DEFAULT_TZ_RULE {
    Some(rule) => {
        assert!(tz::parse(rule).is_some(), "TZ_RULE is not a POSIX TZ rule, like PST8PDT,M3.2.0,M11.1.0");
        Text::literal(rule)
    }
    None => tz::fixed(DEFAULT_TZ_OFFSET_MINUTES),
};

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
        tz: DEFAULT_TZ,
        temp_offset_dc: DEFAULT_TEMP_OFFSET_DC,
        temp_show: TempShow::Celsius,
        hostname: Hostname::DEFAULT,
        alarm_days: DAYS_ALL,
        melody: 0,
        skip_next: false,
        wifi: Credentials::NONE,
        token: Secret(Text::EMPTY),
        token_cleared: false,
        mqtt: Broker::NONE,
        mqtt_cleared: false,
        tx_power_qdbm: DEFAULT_TX_POWER_QDBM,
        output: AudioOut::Buzzer,
    };

    /// Fill in what the build has and the record lacks: the network, the
    /// token and the broker. Run once on the loaded value before `init`;
    /// the next save then carries them in the record, after which the
    /// record wins over a later build's constants. A token or a broker
    /// cleared on purpose (`token_cleared`, `mqtt_cleared`) stays empty.
    pub fn seed(&mut self) {
        if self.wifi.is_none() && !Credentials::BUILT_IN.is_none() {
            self.wifi = Credentials::BUILT_IN;
        }
        if self.token.0.is_empty() && !self.token_cleared && !API_TOKEN.is_empty() {
            self.token = Secret(Text::literal(API_TOKEN));
        }
        if self.mqtt.is_none() && !self.mqtt_cleared && !Broker::BUILT_IN.is_none() {
            self.mqtt = Broker::BUILT_IN;
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
            && tz::parse(self.tz.as_str()).is_some()
            && (-TEMP_OFFSET_LIMIT_DC..=TEMP_OFFSET_LIMIT_DC).contains(&self.temp_offset_dc)
            && self.alarm_days != 0
            && self.alarm_days & !DAYS_ALL == 0
            && (self.melody < melody::LIMIT || self.melody == melody::RANDOM)
            && self.wifi.is_valid()
            && valid_token(self.token.0.as_str())
            && self.mqtt.is_valid()
            && tx_power_name(self.tx_power_qdbm).is_some()
    }

    /// Whether the alarm rings on `weekday` (0 Monday to 6 Sunday).
    pub fn alarm_on(&self, weekday: u8) -> bool {
        self.alarm_days & (1 << (weekday % 7)) != 0
    }

    /// The flash record: version, colour, brightness, alarm hour, alarm
    /// minute, armed, twelve-hour, the rule's standard offset in minutes as
    /// little-endian i16, hostname length, hostname padded to 24 bytes, alarm days, melody
    /// (`0xff` for random), then the SSID, the password and the token,
    /// each a length byte and a padded field, then the flags byte, the
    /// thermometer offset, the temperature-on-the-panel byte, and the
    /// broker: host, port, user and password; then the timezone rule,
    /// the transmit power, the sound output and the alarm sound's name.
    pub fn to_record(&self) -> [u8; RECORD_LEN] {
        // The old minute field, for an image before version 11, which
        // checks it against `TZ_LIMIT_MINUTES`; a rule's standard offset
        // may go to 24 hours, so it is clamped there.
        let tz = tz::parse(self.tz.as_str()).unwrap_or(tz::UTC).std_offset_minutes().clamp(-TZ_LIMIT_MINUTES, TZ_LIMIT_MINUTES).to_le_bytes();
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
        // The name of the melody the index is in the table in force.
        // An index the table has nothing at gets none, and is read back
        // as the index: the pack is out of the table while an upload
        // writes over it, and a save then must not name the beeps.
        if self.melody == melody::RANDOM || self.melody < melody::count() {
            let name = melody::name(self.melody).as_bytes();
            if name.len() <= crate::pack::NAME_MAX {
                r[RECORD_MELODY_NAME_LEN] = name.len() as u8;
                r[RECORD_MELODY_NAME..RECORD_MELODY_NAME + name.len()].copy_from_slice(name);
            }
        }
        r[RECORD_SSID_LEN] = self.wifi.ssid.len;
        r[RECORD_SSID..RECORD_SSID + SSID_MAX].copy_from_slice(&self.wifi.ssid.bytes);
        r[RECORD_PASSWORD_LEN] = self.wifi.password.0.len;
        r[RECORD_PASSWORD..RECORD_PASSWORD + PASSWORD_MAX].copy_from_slice(&self.wifi.password.0.bytes);
        r[RECORD_TOKEN_LEN] = self.token.0.len;
        r[RECORD_TOKEN..RECORD_TOKEN + TOKEN_MAX].copy_from_slice(&self.token.0.bytes);
        r[RECORD_FLAGS] = if self.token_cleared { FLAG_TOKEN_CLEARED } else { 0 } | if self.mqtt_cleared { FLAG_MQTT_CLEARED } else { 0 };
        r[RECORD_TEMP_OFFSET..RECORD_TEMP_OFFSET + 2].copy_from_slice(&self.temp_offset_dc.to_le_bytes());
        r[RECORD_TEMP_SHOW] = self.temp_show as u8;
        r[RECORD_BROKER_HOST_LEN] = self.mqtt.host.len;
        r[RECORD_BROKER_HOST..RECORD_BROKER_HOST + BROKER_HOST_MAX].copy_from_slice(&self.mqtt.host.bytes);
        r[RECORD_BROKER_PORT..RECORD_BROKER_PORT + 2].copy_from_slice(&self.mqtt.port.to_le_bytes());
        r[RECORD_BROKER_USER_LEN] = self.mqtt.user.len;
        r[RECORD_BROKER_USER..RECORD_BROKER_USER + BROKER_USER_MAX].copy_from_slice(&self.mqtt.user.bytes);
        r[RECORD_BROKER_PASSWORD_LEN] = self.mqtt.password.0.len;
        r[RECORD_BROKER_PASSWORD..RECORD_BROKER_PASSWORD + BROKER_PASSWORD_MAX].copy_from_slice(&self.mqtt.password.0.bytes);
        r[RECORD_TZ_LEN] = self.tz.len;
        r[RECORD_TZ..RECORD_TZ + TZ_MAX].copy_from_slice(&self.tz.bytes);
        r[RECORD_TX_POWER] = self.tx_power_qdbm as u8;
        r[RECORD_OUTPUT] = self.output as u8;
        r
    }

    /// Parse a flash record. Older versions lack the later fields, which
    /// take the defaults: version 1 (M5) stops after `armed`, version 2
    /// (M6) after the timezone, version 3 (M6) after the hostname, version
    /// 4 (M7) after the alarm days, version 5 (M8) after the melody, so
    /// those read as no network and no token, version 6 (M10) after
    /// the token, so no flags, version 7 after the flags, so no
    /// thermometer offset, version 8 after the offset, so the
    /// temperature on the panel at its default, and version 9 after
    /// that byte, so no broker, and version 10 after the broker, so the
    /// timezone is the fixed offset of bytes 7 and 8 (every version up to
    /// 10 stored minutes there; version 11 appends the rule and keeps the
    /// minutes as its standard offset), version 11 after the rule, so the
    /// transmit power is the build's, and version 12 after the power, so
    /// the sound goes to the buzzer, and version 13 after the output, so
    /// the alarm sound is the one at the index and not the one of a
    /// name. Versions are append-only, so a record
    /// from a newer image is read by the prefix this one knows, and a
    /// colour, brightness or melody index past this image's table (an
    /// entry a newer image added, or a higher current ceiling) lands on
    /// the last one offered, so that a rollback keeps the user's settings
    /// instead of starting over. The same goes for every other field
    /// (2026-11-17, before the first public release): a value this image
    /// does not know, which a later one may have written into a field
    /// there already is, is that field at its default (a network or a
    /// broker that does not read is none, an alarm at no time of day is
    /// disarmed), and the rest of the record stands, the network above
    /// all. `None` only for a record shorter than its version says.
    pub fn from_record(r: &[u8]) -> Option<Settings> {
        let d = Settings::DEFAULT;
        let mut s = Settings {
            color: *r.get(1)? as usize,
            brightness: *r.get(2)? as usize,
            alarm_hour: *r.get(3)?,
            alarm_minute: *r.get(4)?,
            alarm_armed: *r.get(5)? != 0,
            ..d
        };
        // An alarm at no time of day is no alarm.
        if s.alarm_hour >= 24 || s.alarm_minute >= 60 {
            (s.alarm_hour, s.alarm_minute, s.alarm_armed) = (d.alarm_hour, d.alarm_minute, false);
        }
        let version = r[0];
        if version >= 2 {
            s.twelve_hour = *r.get(6)? != 0;
            let minutes = i16::from_le_bytes([*r.get(7)?, *r.get(8)?]);
            // The timezone up to version 10; informational after, where
            // the rule below is the one that counts.
            if version < 11 && (-TZ_LIMIT_MINUTES..=TZ_LIMIT_MINUTES).contains(&minutes) {
                s.tz = tz::fixed(minutes);
            }
        }
        if version >= 3 {
            let len = *r.get(RECORD_HOSTNAME_LEN)? as usize;
            let bytes = r.get(RECORD_HOSTNAME..RECORD_HOSTNAME + HOSTNAME_MAX)?;
            s.hostname = bytes.get(..len).and_then(|b| core::str::from_utf8(b).ok()).and_then(Hostname::new).unwrap_or(d.hostname);
        }
        if version >= 4 {
            // Bits past the week are a later image's.
            s.alarm_days = match *r.get(RECORD_ALARM_DAYS)? & DAYS_ALL {
                0 => d.alarm_days,
                days => days,
            };
        }
        if version >= 5 {
            s.melody = match *r.get(RECORD_MELODY)? {
                RECORD_MELODY_RANDOM => melody::RANDOM,
                // A clip that is not there (another pack, none, a build
                // without it): the beeps.
                m if (m as usize) < melody::count() => m as usize,
                _ => 0,
            };
        }
        if version >= 6 {
            let ssid = Text::from_record(*r.get(RECORD_SSID_LEN)?, r.get(RECORD_SSID..RECORD_SSID + SSID_MAX)?);
            let password = Text::from_record(*r.get(RECORD_PASSWORD_LEN)?, r.get(RECORD_PASSWORD..RECORD_PASSWORD + PASSWORD_MAX)?);
            s.wifi = match (ssid, password) {
                (Some(ssid), Some(password)) => Credentials { ssid, password: Secret(password) },
                _ => Credentials::NONE,
            };
            if !s.wifi.is_valid() {
                s.wifi = Credentials::NONE;
            }
            s.token = match Text::from_record(*r.get(RECORD_TOKEN_LEN)?, r.get(RECORD_TOKEN..RECORD_TOKEN + TOKEN_MAX)?) {
                Some(t) if valid_token(t.as_str()) => Secret(t),
                _ => Secret(Text::EMPTY),
            };
        }
        if version >= 7 {
            let flags = *r.get(RECORD_FLAGS)?;
            s.token_cleared = flags & FLAG_TOKEN_CLEARED != 0;
            s.mqtt_cleared = flags & FLAG_MQTT_CLEARED != 0;
        }
        if version >= 8 {
            s.temp_offset_dc = i16::from_le_bytes([*r.get(RECORD_TEMP_OFFSET)?, *r.get(RECORD_TEMP_OFFSET + 1)?])
                .clamp(-TEMP_OFFSET_LIMIT_DC, TEMP_OFFSET_LIMIT_DC);
        }
        if version >= 9 {
            s.temp_show = TempShow::from_byte(*r.get(RECORD_TEMP_SHOW)?).unwrap_or(d.temp_show);
        }
        if version >= 10 {
            let host = Text::from_record(*r.get(RECORD_BROKER_HOST_LEN)?, r.get(RECORD_BROKER_HOST..RECORD_BROKER_HOST + BROKER_HOST_MAX)?);
            let port = u16::from_le_bytes([*r.get(RECORD_BROKER_PORT)?, *r.get(RECORD_BROKER_PORT + 1)?]);
            let user = Text::from_record(*r.get(RECORD_BROKER_USER_LEN)?, r.get(RECORD_BROKER_USER..RECORD_BROKER_USER + BROKER_USER_MAX)?);
            let password = Text::from_record(
                *r.get(RECORD_BROKER_PASSWORD_LEN)?,
                r.get(RECORD_BROKER_PASSWORD..RECORD_BROKER_PASSWORD + BROKER_PASSWORD_MAX)?,
            );
            s.mqtt = match (host, user, password) {
                (Some(host), Some(user), Some(password)) => Broker { host, port, user, password: Secret(password) },
                _ => Broker::NONE,
            };
            if !s.mqtt.is_valid() {
                s.mqtt = Broker::NONE;
            }
        }
        if version >= 11
            && let Some(rule) = Text::from_record(*r.get(RECORD_TZ_LEN)?, r.get(RECORD_TZ..RECORD_TZ + TZ_MAX)?)
            && tz::parse(rule.as_str()).is_some()
        {
            s.tz = rule;
        }
        if version >= 12 {
            let qdbm = *r.get(RECORD_TX_POWER)? as i8;
            if tx_power_name(qdbm).is_some() {
                s.tx_power_qdbm = qdbm;
            }
        }
        // An output a later image added is the buzzer, which every
        // clock has.
        if version >= 13 {
            s.output = AudioOut::from_byte(*r.get(RECORD_OUTPUT)?).unwrap_or(d.output);
        }
        // The alarm sound by its name, wherever the table in force has
        // it: another pack may have the clip at another place, and a
        // later image more note melodies before the clips. A name the
        // table has not is a clip that is gone: the beeps, as an index
        // past the table is. No name, or none that reads: the index.
        if version >= 14
            && let Some(name) = Text::<{ crate::pack::NAME_MAX }>::from_record(
                *r.get(RECORD_MELODY_NAME_LEN)?,
                r.get(RECORD_MELODY_NAME..RECORD_MELODY_NAME + crate::pack::NAME_MAX)?,
            )
            && !name.as_str().is_empty()
        {
            s.melody = melody::index_of(name.as_str()).unwrap_or(0);
        }
        s.color = s.color.min(PALETTE.len() - 1);
        // A record saved by a build with a higher ceiling, or by a later
        // image with more steps, may name a step this build does not
        // offer: the brightest one it does.
        s.brightness = s.brightness.min(BRIGHTNESS_STEPS.len() - 1);
        s.is_valid().then_some(s)
    }
}

/// Receivers: the display task, the persist task, the hostname task and
/// the MQTT task.
const RECEIVERS: usize = 4;

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
            tz: Text::new("AEST-10AEDT,M10.1.0/2:00:00,M4.1.0/3:00:00").unwrap(),
            temp_offset_dc: -TEMP_OFFSET_LIMIT_DC,
            temp_show: TempShow::Fahrenheit,
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
            mqtt: Broker {
                host: Text::new(&"h".repeat(BROKER_HOST_MAX)).unwrap(),
                port: 65535,
                user: Text::new(&"u".repeat(BROKER_USER_MAX)).unwrap(),
                password: Secret(Text::new(&"w".repeat(BROKER_PASSWORD_MAX)).unwrap()),
            },
            mqtt_cleared: false,
            tx_power_qdbm: 34,
            output: AudioOut::Line,
        }
    }

    #[test]
    fn a_password_alone_is_one_with_no_network() {
        let with = |ssid: &str, password: &str| Credentials {
            ssid: Text::new(ssid).unwrap(),
            password: Secret(Text::new(password).unwrap()),
        };
        assert!(with("", "abcdefgh").password_alone());
        assert!(!with("home", "abcdefgh").password_alone());
        assert!(!with("home", "").password_alone());
        assert!(!Credentials::NONE.password_alone());
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
    fn palette_is_the_old_gamma_of_the_old_rgb_but_for_yellow() {
        // The palette was 8-bit RGB through a gamma table until 2026-10-29;
        // the table mapped 255 to 7, 96 to 1 and 0 to 0. Yellow left that
        // on 2026-11-01: full green with full red looked pale green on the
        // panel and half green still had a tint, so its green is held at
        // level 2, and the old mix sits next to it as "lime", kept as a
        // colour of its own.
        let old_rgb = [(255, 0, 0), (255, 96, 0), (255, 255, 0), (255, 255, 0), (0, 255, 0), (0, 255, 255), (0, 0, 255), (255, 0, 255), (255, 255, 255)];
        let g = |v: u8| match v {
            255 => MAX_LEVEL,
            96 => 1,
            _ => 0,
        };
        for (i, (r, gr, b)) in old_rgb.iter().enumerate() {
            let want = if PALETTE_NAMES[i] == "yellow" { Levels::new(0, 2, MAX_LEVEL) } else { Levels::new(g(*b), g(*gr), g(*r)) };
            assert_eq!(PALETTE[i], want, "{}", PALETTE_NAMES[i]);
        }
        assert_eq!(PALETTE.len(), PALETTE_NAMES.len());
        // Yellow keeps the green well below the red, and above orange's touch;
        // lime is the next entry, so one press of the top button shows both.
        let yellow = PALETTE[2];
        assert!(yellow.green > PALETTE[1].green && yellow.green < yellow.red && yellow.blue == 0);
        assert_eq!(PALETTE_NAMES[3], "lime");
        assert_eq!(PALETTE[3], Levels::new(0, MAX_LEVEL, MAX_LEVEL));
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
            7 => RECORD_TEMP_OFFSET,
            8 => RECORD_TEMP_SHOW,
            9 => RECORD_BROKER_HOST_LEN,
            10 => RECORD_TZ_LEN,
            11 => RECORD_TX_POWER,
            12 => RECORD_OUTPUT,
            13 => RECORD_MELODY_NAME_LEN,
            _ => RECORD_LEN,
        }
    }

    #[test]
    fn only_an_older_record_is_to_be_written_again() {
        let mut r = full().to_record();
        assert!(!record_is_older(&r) && !record_is_older(&[]));
        r[0] = RECORD_VERSION - 1;
        assert!(record_is_older(&r));
        r[0] = RECORD_VERSION + 1;
        assert!(!record_is_older(&r), "a newer image's record is left to it");
    }

    /// A pack of these clips, in the table.
    fn install(names: &[&str]) {
        let clips: Vec<(&str, &str, bool, &[u8])> = names.iter().map(|&n| (n, "Label", true, &crate::pack::tests::SECOND[..])).collect();
        melody::install(Some(crate::pack::Pack::parse(Vec::leak(crate::pack::build(&clips))).unwrap()));
    }

    #[test]
    fn the_alarm_sound_is_kept_by_its_name() {
        let notes = melody::count() - melody::clip_count();
        install(&["alpha", "beta", "gamma"]);
        let saved = Settings { melody: notes + 1, ..full() }.to_record();
        assert_eq!(saved[RECORD_MELODY], notes as u8 + 1, "the index, for an image before the name");
        assert_eq!(&saved[RECORD_MELODY_NAME_LEN..RECORD_MELODY_NAME + 5], b"\x04beta\0");
        let read = |r: &[u8]| Settings::from_record(r).unwrap().melody;
        assert_eq!(read(&saved), notes + 1);
        // Another pack with the clip at another place: the clip still,
        // where the index alone would name its neighbour.
        install(&["gamma", "delta", "alpha", "beta"]);
        assert_eq!(read(&saved), notes + 3);
        let mut old = saved[..prefix_len(13)].to_vec();
        old[0] = 13;
        assert_eq!(read(&old), notes + 1, "a record before the name goes by the index");
        // A pack without it, or no pack: the beeps, though the index
        // has a clip at it.
        install(&["gamma", "delta", "alpha"]);
        assert_eq!(read(&saved), 0);
        melody::install(None);
        assert_eq!(read(&saved), 0);
        // A note melody and the random draw have names as well.
        for melody in [0, 3, melody::RANDOM] {
            let r = Settings { melody, ..full() }.to_record();
            assert_eq!(read(&r), melody);
            assert_eq!(&r[RECORD_MELODY_NAME..RECORD_MELODY_NAME + melody::name(melody).len()], melody::name(melody).as_bytes());
        }
        // While an upload has the pack out of the table the index has
        // no melody at it: saved with no name, and read back with the
        // pack there again as the index it is, not as the beeps.
        // (The place just past the table with no pack in it, which a
        // build with clips of its own has further on.)
        let place = melody::count();
        let adrift = Settings { melody: place, ..full() }.to_record();
        assert_eq!(adrift[RECORD_MELODY_NAME_LEN], 0);
        assert_eq!(read(&adrift), 0, "and with no pack it is past the table");
        melody::install(Some(crate::pack::fullest()));
        assert_eq!(read(&adrift), place);
        melody::install(None);
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
            // Up to version 10 the timezone was minutes; the record keeps
            // the rule's standard offset there, so an old image reads 600.
            let fixed = tz::fixed(600);
            assert_eq!(
                got.tz,
                match version {
                    11.. => s.tz,
                    2.. => fixed,
                    _ => d.tz,
                },
                "v{version} timezone"
            );
            assert_eq!(got.hostname, if version >= 3 { s.hostname } else { d.hostname });
            assert_eq!(got.alarm_days, if version >= 4 { s.alarm_days } else { d.alarm_days });
            assert_eq!(got.melody, if version >= 5 { s.melody } else { d.melody });
            assert_eq!(got.wifi, if version >= 6 { s.wifi } else { Credentials::NONE }, "v{version} network");
            assert_eq!(got.token, if version >= 6 { s.token } else { Secret(Text::EMPTY) }, "v{version} token");
            assert!(!got.token_cleared, "v{version} has no flags");
            assert_eq!(got.temp_offset_dc, if version >= 8 { s.temp_offset_dc } else { d.temp_offset_dc }, "v{version} thermometer offset");
            assert_eq!(got.temp_show, if version >= 9 { s.temp_show } else { d.temp_show }, "v{version} temperature on the panel");
            assert_eq!(got.mqtt, if version >= 10 { s.mqtt } else { Broker::NONE }, "v{version} broker");
            assert!(!got.mqtt_cleared);
            assert_eq!(got.tx_power_qdbm, if version >= 12 { s.tx_power_qdbm } else { d.tx_power_qdbm }, "v{version} transmit power");
            assert_eq!(got.output, if version >= 13 { s.output } else { d.output }, "v{version} sound output");
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
        flagged[RECORD_FLAGS] |= 0xFC;
        assert_eq!(Settings::from_record(&flagged), Some(s));
    }

    #[test]
    fn a_bad_field_is_its_default_and_table_indexes_are_clamped() {
        let s = full();
        let r = s.to_record();
        let with = |f: &dyn Fn(&mut [u8; RECORD_LEN])| {
            let mut r = r;
            f(&mut r);
            Settings::from_record(&r)
        };
        // A value this image does not know costs its own field and
        // nothing else: what a later image may have written is read by
        // an earlier one after a rollback, and the network must stand.
        let d = Settings::DEFAULT;
        let but = |name: &str, f: &dyn Fn(&mut [u8; RECORD_LEN]), expect: Settings| {
            assert_eq!(with(f), Some(expect), "{name}");
        };
        for (name, f) in [
            ("SSID length past the field", (&|r: &mut [u8; RECORD_LEN]| r[RECORD_SSID_LEN] = SSID_MAX as u8 + 1) as &dyn Fn(&mut [u8; RECORD_LEN])),
            ("password of 7", &|r| r[RECORD_PASSWORD_LEN] = PASSWORD_MIN as u8 - 1),
            ("password of 65", &|r| r[RECORD_PASSWORD_LEN] = PASSWORD_MAX as u8 + 1),
            ("SSID not UTF-8", &|r| r[RECORD_SSID] = 0xFF),
        ] {
            but(name, f, Settings { wifi: Credentials::NONE, ..s });
        }
        for (name, f) in [
            ("token of 7", (&|r: &mut [u8; RECORD_LEN]| r[RECORD_TOKEN_LEN] = TOKEN_MIN as u8 - 1) as &dyn Fn(&mut [u8; RECORD_LEN])),
            ("token of 33", &|r| r[RECORD_TOKEN_LEN] = TOKEN_MAX as u8 + 1),
            ("token with a space", &|r| r[RECORD_TOKEN] = b' '),
        ] {
            but(name, f, Settings { token: Secret(Text::EMPTY), ..s });
        }
        for (name, f) in [
            ("host length past the field", (&|r: &mut [u8; RECORD_LEN]| r[RECORD_BROKER_HOST_LEN] = BROKER_HOST_MAX as u8 + 1) as &dyn Fn(&mut [u8; RECORD_LEN])),
            ("host with a space", &|r| r[RECORD_BROKER_HOST] = b' '),
            ("port 0", &|r| r[RECORD_BROKER_PORT..RECORD_BROKER_PORT + 2].copy_from_slice(&[0, 0])),
            ("user with a space", &|r| r[RECORD_BROKER_USER] = b' '),
            ("password with a control character", &|r| r[RECORD_BROKER_PASSWORD] = 0x7f),
        ] {
            but(name, f, Settings { mqtt: Broker::NONE, ..s });
        }
        let unarmed = Settings { alarm_hour: d.alarm_hour, alarm_minute: d.alarm_minute, alarm_armed: false, ..s };
        but("alarm hour 24", &|r| r[3] = 24, unarmed);
        but("alarm minute 60", &|r| r[4] = 60, unarmed);
        but("brightness past the whole list", &|r| r[2] = BRIGHTNESS_CHOICES.len() as u8, Settings { brightness: BRIGHTNESS_STEPS.len() - 1, ..s });
        but("no alarm day", &|r| r[RECORD_ALARM_DAYS] = 0, Settings { alarm_days: d.alarm_days, ..s });
        but("an eighth day", &|r| r[RECORD_ALARM_DAYS] |= 0x80, s);
        but("a rule that does not parse", &|r| r[RECORD_TZ] = b',', Settings { tz: d.tz, ..s });
        but("a rule past its field", &|r| r[RECORD_TZ_LEN] = TZ_MAX as u8 + 1, Settings { tz: d.tz, ..s });
        but("a hostname with a space", &|r| r[RECORD_HOSTNAME] = b' ', Settings { hostname: d.hostname, ..s });
        but("a hostname past its field", &|r| r[RECORD_HOSTNAME_LEN] = HOSTNAME_MAX as u8 + 1, Settings { hostname: d.hostname, ..s });
        but("another face for the temperature", &|r| r[RECORD_TEMP_SHOW] = 9, Settings { temp_show: d.temp_show, ..s });
        but("a transmit power that is no level", &|r| r[RECORD_TX_POWER] = 69, Settings { tx_power_qdbm: d.tx_power_qdbm, ..s });
        but("an output of a later image", &|r| r[RECORD_OUTPUT] = 3, Settings { output: d.output, ..s });
        but(
            "an offset past the limit",
            &|r| r[RECORD_TEMP_OFFSET..RECORD_TEMP_OFFSET + 2].copy_from_slice(&(TEMP_OFFSET_LIMIT_DC + 1).to_le_bytes()),
            Settings { temp_offset_dc: TEMP_OFFSET_LIMIT_DC, ..s },
        );
        // Everything wrong at once is still a record, and a short one is not.
        let wreck = with(&|r| r[1..].fill(0xFF)).expect("a record of 0xFF");
        assert!(wreck.is_valid() && wreck.wifi.is_none());
        assert_eq!(Settings::from_record(&r[..RECORD_LEN - 1]), None, "a record cut short");
        assert_eq!(Settings::from_record(&[]), None);
        // Bytes 7 and 8 are informational in a version 11 record, but a
        // version 10 record is read from them and checked.
        let old = |m: i16| {
            let mut r = full().to_record();
            r[0] = 10;
            r[7..9].copy_from_slice(&m.to_le_bytes());
            Settings::from_record(&r)
        };
        assert_eq!(old(-420).map(|s| s.tz), Some(tz::fixed(-420)), "a version 10 record's minutes");
        assert_eq!(old(TZ_LIMIT_MINUTES + 1).map(|s| s.tz), Some(d.tz), "timezone past the limit");
        // A rule's standard offset may pass the old limit (POSIX allows
        // 24 hours): the record round-trips on this image, and the old
        // field is clamped so that an older image reads the record too.
        for rule in ["XXX-15", "<-14>14", "<+24>-24", "AAA-15BBB,M3.2.0,M11.1.0"] {
            let s = Settings { tz: Text::new(rule).unwrap(), ..full() };
            assert!(s.is_valid(), "{rule}");
            let r = s.to_record();
            assert_eq!(Settings::from_record(&r).map(|s| s.tz), Some(s.tz), "{rule} round trip");
            let legacy = i16::from_le_bytes([r[7], r[8]]);
            assert!((-TZ_LIMIT_MINUTES..=TZ_LIMIT_MINUTES).contains(&legacy), "{rule}: {legacy}");
            let mut v10 = r;
            v10[0] = 10;
            assert_eq!(Settings::from_record(&v10).map(|s| s.tz), Some(tz::fixed(legacy)), "{rule} read by version 10");
        }
        let wild = with(&|r| r[7..9].copy_from_slice(&(TZ_LIMIT_MINUTES + 1).to_le_bytes()));
        assert_eq!(wild.map(|s| s.tz), Some(full().tz), "the old field is not checked in a version 11 record");
        // Indexes a newer image may have added land on the last entry.
        assert_eq!(with(&|r| r[1] = 200).map(|s| s.color), Some(PALETTE.len() - 1));
        // A melody that is not in the table is the beeps: a clip of a
        // pack that is gone, or of another build. By the index, in a
        // record with no name.
        let index = |m: u8| {
            with(&|r| {
                r[RECORD_MELODY] = m;
                r[RECORD_MELODY_NAME_LEN] = 0;
            })
            .map(|s| s.melody)
        };
        assert_eq!(index(200), Some(0));
        assert_eq!(index(melody::count() as u8), Some(0));
        assert_eq!(index(melody::count() as u8 - 1), Some(melody::count() - 1));
        // With a pack in the table its clips are there to be named.
        let last = melody::count() - melody::clip_count() + crate::pack::MAX_CLIPS - 1;
        melody::install(Some(crate::pack::fullest()));
        assert_eq!(index(last as u8), Some(last));
        assert_eq!(index(last as u8 + 1), Some(0));
        melody::install(None);
        // A name that does not read is no name: the index.
        let odd = with(&|r| {
            r[RECORD_MELODY] = 2;
            r[RECORD_MELODY_NAME_LEN] = crate::pack::NAME_MAX as u8 + 1;
        });
        assert_eq!(odd.map(|s| s.melody), Some(2));
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
        let no_mqtt = with(&|r| r[RECORD_BROKER_HOST_LEN] = 0).unwrap();
        assert!(no_mqtt.mqtt.is_none(), "an empty host is MQTT off");
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
        // The broker the same way: filled when never set, not when cleared.
        assert_eq!(d.mqtt.is_none(), MQTT_HOST.is_empty());
        let mut off = Settings { mqtt: Broker::NONE, mqtt_cleared: true, ..full() };
        off.seed();
        assert!(off.mqtt.is_none() && off.mqtt_cleared);
        assert_eq!(Settings::from_record(&off.to_record()), Some(off));
        assert_eq!(off.to_record()[RECORD_FLAGS], FLAG_MQTT_CLEARED);
        let both = Settings { token: Secret(Text::EMPTY), token_cleared: true, ..off };
        assert_eq!(both.to_record()[RECORD_FLAGS], FLAG_TOKEN_CLEARED | FLAG_MQTT_CLEARED);
        assert_eq!(Settings::from_record(&both.to_record()), Some(both));
    }

    #[test]
    fn debug_never_shows_a_secret() {
        let s = full();
        let text = format!("{s:?}");
        assert!(text.contains(&"s".repeat(SSID_MAX)), "the SSID is shown");
        assert!(!text.contains("pppp"), "the password is hidden");
        assert!(!text.contains("tttt"), "the token is hidden");
        assert!(text.contains(&"h".repeat(BROKER_HOST_MAX)) && text.contains(&"u".repeat(BROKER_USER_MAX)), "the broker and its user are shown");
        assert!(!text.contains("wwww"), "the broker password is hidden");
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
    fn broker_rules() {
        assert!(valid_broker_host(""));
        assert!(valid_broker_host("10.1.1.5"));
        assert!(valid_broker_host("homeassistant.local"));
        assert!(valid_broker_host("mqtt-1.example.com"));
        assert!(valid_broker_host(&"h".repeat(BROKER_HOST_MAX)));
        assert!(!valid_broker_host(&"h".repeat(BROKER_HOST_MAX + 1)));
        assert!(!valid_broker_host("a b"));
        assert!(!valid_broker_host("a_b"));
        assert!(!valid_broker_host("http://a"));
        assert!(!valid_broker_host("a:1883"));
        assert!(valid_broker_user(""));
        assert!(valid_broker_user("clock"));
        assert!(!valid_broker_user("cl ock"));
        assert!(!valid_broker_user(&"u".repeat(BROKER_USER_MAX + 1)));
        assert!(valid_broker_password(""));
        assert!(valid_broker_password("p&ss word"));
        assert!(!valid_broker_password("tab\there"));
        assert!(!valid_broker_password(&"p".repeat(BROKER_PASSWORD_MAX + 1)));
        assert!(Broker::NONE.is_none() && Broker::NONE.is_valid());
        assert_eq!(Broker::NONE.port, MQTT_PORT);
        assert!(!Broker { port: 0, ..Broker::NONE }.is_valid());
        assert!(!Settings { mqtt: Broker { port: 0, ..Broker::NONE }, ..Settings::DEFAULT }.is_valid());
    }

    #[test]
    fn is_valid_at_the_edges() {
        let ok = full();
        assert!(ok.is_valid());
        assert!(Settings { tz: tz::fixed(TZ_LIMIT_MINUTES), ..ok }.is_valid());
        assert!(!Settings { tz: Text::new("PST").unwrap(), ..ok }.is_valid(), "a rule that does not parse");
        assert!(Settings::DEFAULT.is_valid(), "the build's default rule parses");
        assert!(!Settings { color: PALETTE.len(), ..ok }.is_valid());
        assert!(!Settings { brightness: BRIGHTNESS_STEPS.len(), ..ok }.is_valid());
        assert!(Settings { melody: melody::RANDOM, ..ok }.is_valid());
        assert!(!Settings { melody: melody::LIMIT, ..ok }.is_valid());
        assert!(Settings { melody: melody::LIMIT - 1, ..ok }.is_valid(), "a pack's last clip, whichever table is in force");
        assert!(Settings { temp_offset_dc: TEMP_OFFSET_LIMIT_DC, ..ok }.is_valid());
        assert!(Settings { temp_offset_dc: -TEMP_OFFSET_LIMIT_DC, ..ok }.is_valid());
        assert!(!Settings { temp_offset_dc: TEMP_OFFSET_LIMIT_DC + 1, ..ok }.is_valid());
        assert!(!Settings { temp_offset_dc: -TEMP_OFFSET_LIMIT_DC - 1, ..ok }.is_valid());
        assert_eq!(TempShow::from_name("f"), Some(TempShow::Fahrenheit));
        assert_eq!(TempShow::from_name("F"), None);
        assert_eq!(TempShow::from_byte(3), None);
        for t in TempShow::ALL {
            assert_eq!(TempShow::from_name(t.name()), Some(t));
            assert_eq!(TempShow::from_byte(t as u8), Some(t));
        }
        let mut bad = ok.to_record();
        bad[RECORD_TEMP_SHOW] = 3;
        assert_eq!(Settings::from_record(&bad), Some(Settings { temp_show: Settings::DEFAULT.temp_show, ..ok }), "a temperature byte of a later image is the default");
        // The transmit power: the driver's levels up to the ceiling.
        assert_eq!(TX_POWER_CHOICES.last().map(|c| c.1), Some(WIFI_MAX_TX_POWER_QDBM));
        for (name, qdbm) in TX_POWER_CHOICES {
            assert!(Settings { tx_power_qdbm: *qdbm, ..ok }.is_valid());
            assert_eq!(tx_power_from_name(name), Some(*qdbm));
            assert_eq!(tx_power_name(*qdbm), Some(*name));
            // 68 is inside the 16 dBm level, which starts at 66.
            let quarters = (name.parse::<f32>().unwrap() * 4.0) as i8;
            assert!((quarters..=quarters + 4).contains(qdbm), "{name} dBm in quarters");
        }
        for q in [0, 7, 9, 44 + 1, 72, 80, -8] {
            assert!(!Settings { tx_power_qdbm: q, ..ok }.is_valid(), "{q}");
        }
        assert_eq!(tx_power_from_name("18"), None);
        let mut bad = ok.to_record();
        bad[RECORD_TX_POWER] = 72;
        assert_eq!(Settings::from_record(&bad).map(|s| s.tx_power_qdbm), Some(Settings::DEFAULT.tx_power_qdbm), "a power above the ceiling is the default");
        assert_eq!(AudioOut::from_name("line"), Some(AudioOut::Line));
        assert_eq!(AudioOut::from_name("Line"), None);
        assert_eq!(AudioOut::from_byte(3), None);
        for o in AudioOut::ALL {
            assert_eq!(AudioOut::from_name(o.name()), Some(o));
            assert_eq!(AudioOut::from_byte(o as u8), Some(o));
        }
        assert_eq!(Settings::DEFAULT.output, AudioOut::Buzzer, "what a stock clock has");
        for (out, tones, clips) in [
            (AudioOut::Buzzer, SoundPin::Buzzer, SoundPin::Buzzer),
            (AudioOut::Line, SoundPin::Line, SoundPin::Line),
            (AudioOut::Split, SoundPin::Buzzer, SoundPin::Line),
        ] {
            assert_eq!((out.tones(), out.clips()), (tones, clips), "{out:?}");
            assert_eq!(AudioOut::from_byte(out as u8), Some(out));
            assert_eq!(AudioOut::from_name(out.name()), Some(out));
        }
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

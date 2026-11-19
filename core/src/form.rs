// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The settings as key=value pairs: parsing a form body or query string
//! into changes, and the names the page and the API use. Pure code.

use core::fmt::Write as _;

use crate::melody;
use crate::mqtt::Link;
use crate::sensors::{Tenths, fahrenheit_tenths};
use crate::settings::{
    AudioOut, BRIGHTNESS_STEPS, BROKER_HOST_MAX, BROKER_PASSWORD_MAX, BROKER_USER_MAX, Credentials, DAY_NAMES, DAYS_ALL, DAYS_WEEKDAYS,
    DAYS_WEEKEND, HOSTNAME_MAX, Hostname, PALETTE_NAMES, PASSWORD_MAX, PASSWORD_MIN, SSID_MAX, Secret, Settings,
    TEMP_OFFSET_LIMIT_DC, TOKEN_MAX, TX_POWER_CHOICES, TZ_LIMIT_MINUTES, TempShow, Text, tx_power_from_name, tx_power_name,
    valid_broker_host, valid_broker_password, valid_broker_user, valid_password, valid_token,
};
use crate::config::DEFAULT_TX_POWER_QDBM;
use crate::tz::{self, TZ_MAX};

/// Longest form value, decoded: the password.
pub const VALUE_MAX: usize = PASSWORD_MAX;

/// A network the access point's scan saw, for the setup page's list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Network {
    pub ssid: Text<SSID_MAX>,
    pub rssi: i8,
    pub open: bool,
}

/// Read-only status shown under the form and in the JSON. Not settings:
/// a posted status key is refused as unknown like any other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Status {
    /// What the firmware calls itself, and the app slot it runs from.
    pub version: &'static str,
    pub slot: &'static str,
    /// `sta` or `ap`: joined to a network, or raising its own.
    pub mode: &'static str,
    pub uptime_secs: u64,
    /// Self-heal resets since power-up.
    pub heals: u32,
    /// Watchdog resets since power-up.
    pub watchdog: u32,
    /// The clip task hung in a clip and the watchdog reset the clock:
    /// notes play in place of every clip until a new pack or firmware.
    pub clips_paused: bool,
    /// When the last self-heal happened, if one did and the time was known.
    pub last_heal: Option<HealStamp>,
    /// The room in tenths of a degree Celsius: `None` before the first
    /// reading or with no thermometer fitted.
    pub temp_dc: Option<i16>,
    /// The light index, 0 (dark) to 100: `None` before the first reading.
    pub light: Option<u8>,
    /// The station's signal in dBm: `None` while not connected, and on
    /// the access point.
    pub rssi: Option<i8>,
    /// What the MQTT task is doing.
    pub mqtt: Link,
    /// Whether the alarm is ringing right now.
    pub ringing: bool,
    /// The size of the `clips` partition in bytes: `None` on a clock
    /// whose partition table has none, which plays the built-in clips
    /// and takes no pack.
    pub clips_room: Option<u32>,
    /// The local time right now under the settings' rule: `None` before
    /// the first time sync.
    pub local: Option<LocalNow>,
}

/// The local time of the moment the page was built, for checking a
/// timezone rule; converted by the caller so this module stays free of
/// the clock. The zone's name is read from the rule with `dst`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LocalNow {
    /// 0 is Monday, 6 is Sunday.
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    /// Seconds east of UTC in force.
    pub offset_secs: i32,
    /// Whether daylight time is in force.
    pub dst: bool,
}

/// The time of a self-heal, converted by the caller so this module stays
/// free of the clock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HealStamp {
    pub unix_secs: i64,
    /// 0 is Monday, 6 is Sunday.
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    /// Seconds since it happened, if the time is known now.
    pub ago_secs: Option<i64>,
}

const WEEKDAY_LABELS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// A span of seconds as `3 d 4 h 12 min`, dropping leading zero parts;
/// seconds only under a minute.
struct Span(i64);

impl core::fmt::Display for Span {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = self.0.max(0);
        let (d, h, m) = (s / 86_400, s % 86_400 / 3600, s % 3600 / 60);
        if d > 0 {
            write!(f, "{d} d {h} h {m} min")
        } else if h > 0 {
            write!(f, "{h} h {m} min")
        } else if m > 0 {
            write!(f, "{m} min")
        } else {
            write!(f, "{s} s")
        }
    }
}

/// What a bad key or value looks like to the caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FormError {
    UnknownKey,
    BadValue,
    /// A network key (`ssid`, `password`) or a broker key (`mqtt_*`)
    /// from a request that may not set it (`Allow`).
    Forbidden,
    /// The settings page moved the broker to another host without
    /// typing the password again: the stored one is not handed on.
    BrokerPassword,
}

/// The keys of the pages' forms that take a typed or chosen value, with
/// the label each has on its page: what a refused form names its bad
/// field by. The page writes the label, never the query's own text.
pub const FIELDS: [(&str, &str); 17] = [
    ("alarm", "Alarm time"),
    ("days", "Alarm days"),
    ("melody", "Alarm sound"),
    ("color", "Colour"),
    ("brightness", "Brightness"),
    ("tz", "Timezone"),
    ("temp_show", "Temperature on the panel"),
    ("temp_offset", "Thermometer offset"),
    ("hostname", "Hostname"),
    ("tx_power", "Wi-Fi transmit power"),
    ("output", "Sound output"),
    ("mqtt_host", "Host"),
    ("mqtt_port", "Port"),
    ("mqtt_user", "User"),
    ("mqtt_password", "Password"),
    ("ssid", "Network name"),
    ("password", "Password"),
];

/// The key of `FIELDS` equal to `key`, as a `'static` string a redirect
/// can carry.
pub fn field(key: &str) -> Option<&'static str> {
    FIELDS.iter().find(|(k, _)| *k == key).map(|(k, _)| *k)
}

/// What a page says above its form about the post that led to it:
/// nothing, that it was saved, or why it was refused, with the key of
/// the field at fault (one of `FIELDS`) when there was one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Note {
    None,
    Saved,
    Refused(FormError, Option<&'static str>),
}

/// The notice of a station page, under the line of links.
fn write_note(out: &mut impl core::fmt::Write, note: Note) -> core::fmt::Result {
    match note {
        Note::None => Ok(()),
        Note::Saved => out.write_str("<p class=c><b>Saved.</b></p>"),
        Note::Refused(FormError::BadValue, Some(key)) => match FIELDS.iter().find(|(k, _)| *k == key) {
            Some((_, label)) => write!(out, "<p class=err>Nothing was saved: bad value for <b>{label}</b>.</p>"),
            None => out.write_str("<p class=err>Nothing was saved: a field had a bad value.</p>"),
        },
        Note::Refused(FormError::BadValue, None) => out.write_str("<p class=err>Nothing was saved: a field had a bad value.</p>"),
        Note::Refused(FormError::UnknownKey, _) => out.write_str("<p class=err>Nothing was saved: an unknown field was sent.</p>"),
        Note::Refused(FormError::Forbidden, _) => out.write_str("<p class=err>Nothing was saved: the network is set with the token.</p>"),
        Note::Refused(FormError::BrokerPassword, _) => out.write_str("<p class=err>Nothing was saved: a new broker needs its password typed again.</p>"),
    }
}

/// Which guarded keys a request may set. The network keys need the
/// token on `/api`, or the access point's own page; the broker keys
/// need the token on `/api` too, since pointing the clock at another
/// broker would hand that broker the stored password, but the settings
/// page takes them, as it takes the hostname and the alarm, under the
/// rule that a new host needs the password typed again (`apply`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Allow {
    pub network: bool,
    pub broker: bool,
}

impl Allow {
    /// `/api` without the token, and a command over MQTT.
    pub const NONE: Allow = Allow { network: false, broker: false };
    /// The settings page: the broker, but a new host needs its password.
    pub const PAGE: Allow = Allow { network: false, broker: true };
    /// `/api` with the token, and the setup form on the access point.
    pub const ALL: Allow = Allow { network: true, broker: true };
}

/// A string inside JSON quotes: `"` and `\` escaped, control characters
/// as `\uXXXX`. The SSID can hold anything; the other strings are
/// validated to plain characters.
pub struct JsonStr<'a>(pub &'a str);

impl core::fmt::Display for JsonStr<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for c in self.0.chars() {
            match c {
                '"' => f.write_str("\\\"")?,
                '\\' => f.write_str("\\\\")?,
                c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
                c => f.write_char(c)?,
            }
        }
        Ok(())
    }
}

/// A string inside a double-quoted HTML attribute or in text: `&`, `<`,
/// `>` and `"` as entities.
pub struct HtmlStr<'a>(pub &'a str);

impl core::fmt::Display for HtmlStr<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for c in self.0.chars() {
            match c {
                '&' => f.write_str("&amp;")?,
                '<' => f.write_str("&lt;")?,
                '>' => f.write_str("&gt;")?,
                '"' => f.write_str("&quot;")?,
                c => f.write_char(c)?,
            }
        }
        Ok(())
    }
}

/// Decode `%XX` and `+` of one form token into `out`. Returns the decoded
/// length, or `None` if it does not fit or the escape is malformed.
pub fn percent_decode(src: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut n = 0;
    let mut i = 0;
    while i < src.len() {
        let c = match src[i] {
            b'+' => b' ',
            b'%' => {
                let hi = hex(*src.get(i + 1)?)?;
                let lo = hex(*src.get(i + 2)?)?;
                i += 2;
                hi << 4 | lo
            }
            c => c,
        };
        *out.get_mut(n)? = c;
        n += 1;
        i += 1;
    }
    Some(n)
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `on`, `1`, `true` and `yes` are on, anything else is off.
fn truthy(v: &str) -> bool {
    matches!(v, "on" | "1" | "true" | "yes")
}

fn parse_u8(v: &str) -> Option<u8> {
    v.parse().ok()
}

/// `all`, `weekdays`, `weekend`, or a comma-separated list of day names.
/// `None` for anything else, including an empty list.
pub fn parse_days(v: &str) -> Option<u8> {
    match v {
        "all" => return Some(DAYS_ALL),
        "weekdays" => return Some(DAYS_WEEKDAYS),
        "weekend" => return Some(DAYS_WEEKEND),
        _ => {}
    }
    let mut mask = 0;
    for name in v.split(',') {
        mask |= 1 << DAY_NAMES.iter().position(|d| *d == name.trim())?;
    }
    (mask != 0).then_some(mask)
}

/// The preset name for a day mask, or the day list.
pub fn write_days(out: &mut impl core::fmt::Write, mask: u8) -> core::fmt::Result {
    match mask {
        DAYS_ALL => out.write_str("all"),
        DAYS_WEEKDAYS => out.write_str("weekdays"),
        DAYS_WEEKEND => out.write_str("weekend"),
        _ => {
            let mut first = true;
            for (i, name) in DAY_NAMES.iter().enumerate() {
                if mask & (1 << i) != 0 {
                    if !first {
                        out.write_str(",")?;
                    }
                    out.write_str(name)?;
                    first = false;
                }
            }
            Ok(())
        }
    }
}

/// Apply one key and value to `s`. `allow` says whether the request may
/// set the network (`ssid`, `password`) and the broker (`mqtt_*`). The
/// check is on the key, never on whether the value would change
/// anything, so a request without the token learns nothing about the
/// stored passwords.
pub fn apply_one(s: &mut Settings, key: &str, value: &str, allow: Allow) -> Result<(), FormError> {
    match key {
        "ssid" | "ssid_pick" | "password" if !allow.network => return Err(FormError::Forbidden),
        "mqtt_host" | "mqtt_port" | "mqtt_user" | "mqtt_password" if !allow.broker => return Err(FormError::Forbidden),
        // An empty host is MQTT off and takes the user and password with
        // it, so the record keeps no stale secret; the flag keeps a
        // build's `MQTT_HOST` from filling it back in at boot.
        "mqtt_host" => {
            if !valid_broker_host(value) {
                return Err(FormError::BadValue);
            }
            s.mqtt.host = Text::new(value).ok_or(FormError::BadValue)?;
            s.mqtt_cleared = value.is_empty();
            if value.is_empty() {
                s.mqtt.user = Text::EMPTY;
                s.mqtt.password = Secret(Text::EMPTY);
            }
        }
        "mqtt_port" => {
            let port: u16 = value.parse().map_err(|_| FormError::BadValue)?;
            if port == 0 {
                return Err(FormError::BadValue);
            }
            s.mqtt.port = port;
        }
        "mqtt_user" => {
            if !valid_broker_user(value) {
                return Err(FormError::BadValue);
            }
            s.mqtt.user = Text::new(value).ok_or(FormError::BadValue)?;
        }
        // The page's field is write-only and always posted: empty keeps
        // the stored password. Clearing the host is the way to drop it.
        "mqtt_password" => {
            if !valid_broker_password(value) {
                return Err(FormError::BadValue);
            }
            if !value.is_empty() {
                s.mqtt.password = Secret(Text::new(value).ok_or(FormError::BadValue)?);
            }
        }
        // The setup form sends both the list's choice and the typed name,
        // one of them empty; empty means unchanged.
        "ssid" | "ssid_pick" => {
            if !value.is_empty() {
                s.wifi.ssid = Text::new(value).ok_or(FormError::BadValue)?;
            }
        }
        "password" => {
            if !valid_password(value) {
                return Err(FormError::BadValue);
            }
            s.wifi.password = Secret(Text::new(value).ok_or(FormError::BadValue)?);
        }
        "color" => {
            s.color = PALETTE_NAMES.iter().position(|n| *n == value).ok_or(FormError::BadValue)?;
        }
        "brightness" => {
            let pct = parse_u8(value).ok_or(FormError::BadValue)?;
            s.brightness = BRIGHTNESS_STEPS.iter().position(|p| *p == pct).ok_or(FormError::BadValue)?;
        }
        // `HH:MM`, or `HH:MM:SS` as Home Assistant's time entity sends
        // it; the seconds are checked and dropped.
        "alarm" => {
            let (h, rest) = value.split_once(':').ok_or(FormError::BadValue)?;
            let (m, sec) = match rest.split_once(':') {
                Some((m, sec)) => (m, parse_u8(sec).ok_or(FormError::BadValue)?),
                None => (rest, 0),
            };
            let (h, m) = (parse_u8(h).ok_or(FormError::BadValue)?, parse_u8(m).ok_or(FormError::BadValue)?);
            if h >= 24 || m >= 60 || sec >= 60 {
                return Err(FormError::BadValue);
            }
            s.alarm_hour = h;
            s.alarm_minute = m;
        }
        "armed" => s.alarm_armed = truthy(value),
        "twelve_hour" => s.twelve_hour = truthy(value),
        "skip" => s.skip_next = truthy(value),
        // A POSIX TZ rule, or minutes east of UTC as the key took until
        // 2026-11-07, which become a fixed rule.
        "tz" => match value.parse::<i16>() {
            Ok(minutes) if (-TZ_LIMIT_MINUTES..=TZ_LIMIT_MINUTES).contains(&minutes) => s.tz = tz::fixed(minutes),
            Ok(_) => return Err(FormError::BadValue),
            Err(_) => {
                tz::parse(value).ok_or(FormError::BadValue)?;
                s.tz = Text::new(value).ok_or(FormError::BadValue)?;
            }
        },
        "temp_offset" => {
            let dc = parse_tenths(value).ok_or(FormError::BadValue)?;
            if !(-TEMP_OFFSET_LIMIT_DC..=TEMP_OFFSET_LIMIT_DC).contains(&dc) {
                return Err(FormError::BadValue);
            }
            s.temp_offset_dc = dc;
        }
        "temp_show" => s.temp_show = TempShow::from_name(value).ok_or(FormError::BadValue)?,
        "hostname" => s.hostname = Hostname::new(value).ok_or(FormError::BadValue)?,
        "tx_power" => s.tx_power_qdbm = tx_power_from_name(value).ok_or(FormError::BadValue)?,
        "output" => s.output = AudioOut::from_name(value).ok_or(FormError::BadValue)?,
        "days" => s.alarm_days = parse_days(value).ok_or(FormError::BadValue)?,
        "melody" => s.melody = melody::index_of(value).ok_or(FormError::BadValue)?,
        _ => return Err(FormError::UnknownKey),
    }
    Ok(())
}

/// A decimal with at most one place, as tenths: `-0.7` is −7, `1` is
/// 10, `+2.5` is 25. Nothing else: no second decimal, no bare point.
pub fn parse_tenths(value: &str) -> Option<i16> {
    let (negative, rest) = match value.as_bytes().first()? {
        b'-' => (true, &value[1..]),
        b'+' => (false, &value[1..]),
        _ => (false, value),
    };
    let (whole, tenth) = match rest.split_once('.') {
        Some((w, t)) => (w, t),
        None => (rest, "0"),
    };
    if whole.is_empty() || whole.len() > 4 || tenth.len() != 1 || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let whole: i16 = whole.parse().ok()?;
    let tenth = (*tenth.as_bytes().first()? as char).to_digit(10)? as i16;
    let dc = whole.checked_mul(10)?.checked_add(tenth)?;
    Some(if negative { -dc } else { dc })
}

/// Apply every `key=value` pair of a form body or query string to `s`:
/// a partial update, since a key that is not sent is left alone. A page
/// sends a hidden `0` before each checkbox (`Check`), so an unticked
/// box, for which a browser sends nothing, is the `0` and a ticked one
/// the `on` after it. `allow` is passed to `apply_one`. The first bad
/// pair stops and is reported;
/// earlier pairs stay applied, so the caller works on a copy. A
/// non-empty `ssid_pick` (the setup form's list) wins over `ssid` (its
/// typed field) whatever their order, since a browser may send a stale
/// typed name along with the pick. A request the token did not vouch
/// for (`Allow::PAGE`) that moves the broker to another host without a
/// non-empty `mqtt_password` keeps nothing: it is refused as
/// `BrokerPassword`, so the stored password never goes to a host the
/// page named; the same host keeps it on an empty field, and an empty
/// host clears it.
pub fn apply(s: &mut Settings, body: &[u8], allow: Allow) -> Result<(), FormError> {
    apply_naming(s, body, allow).map_err(|(e, _)| e)
}

/// `apply`, answering with an error the key it was about, if that is
/// one of `FIELDS`: what a page's form is refused with, so the page can
/// say which field.
pub fn apply_naming(s: &mut Settings, body: &[u8], allow: Allow) -> Result<(), (FormError, Option<&'static str>)> {
    let before = s.mqtt;
    let mut password_typed = false;
    let mut pick = None;
    let mut days_seen = false;
    for (key, value) in Pairs(body) {
        let mut kbuf = [0u8; 16];
        let mut vbuf = [0u8; VALUE_MAX];
        let kn = percent_decode(key, &mut kbuf).ok_or((FormError::UnknownKey, None))?;
        let key = core::str::from_utf8(&kbuf[..kn]).map_err(|_| (FormError::UnknownKey, None))?;
        let named = |e| (e, field(key));
        let vn = percent_decode(value, &mut vbuf).ok_or(FormError::BadValue).map_err(named)?;
        let value = core::str::from_utf8(&vbuf[..vn]).map_err(|_| FormError::BadValue).map_err(named)?;
        let days = s.alarm_days;
        apply_one(s, key, value, allow).map_err(named)?;
        // The page's seven checkboxes are one key sent once a ticked
        // day: the first replaces the days, the rest add to them.
        if key == "days" {
            if days_seen {
                s.alarm_days |= days;
            }
            days_seen = true;
        }
        if key == "ssid_pick" && !value.is_empty() {
            pick = Some(s.wifi.ssid);
        }
        if key == "mqtt_password" && !value.is_empty() {
            password_typed = true;
        }
    }
    if let Some(ssid) = pick {
        s.wifi.ssid = ssid;
    }
    let moved = s.mqtt.host != before.host && !s.mqtt.host.is_empty();
    if allow == Allow::PAGE && moved && !password_typed && !before.password.0.is_empty() {
        return Err((FormError::BrokerPassword, None));
    }
    Ok(())
}

/// The setup form's password rule, run on the applied form against the
/// stored network and the scan list. The form's password field is
/// write-only and so always posted, empty unless typed:
///
/// - the stored network again with no password keeps the stored
///   password, unless the scan lists that network as open;
/// - another network the scan lists as secured, with no password, is a
///   bad value: the join would fail for certain;
/// - anything else is as posted, so a network the scan did not see is
///   taken on trust, open or not.
pub fn settle_password(trial: &mut Settings, stored: &Credentials, networks: &[Network]) -> Result<(), FormError> {
    if !trial.wifi.password.0.is_empty() {
        return Ok(());
    }
    let listed_open = networks.iter().find(|n| n.ssid == trial.wifi.ssid).map(|n| n.open);
    if trial.wifi.ssid == stored.ssid {
        if listed_open != Some(true) {
            trial.wifi.password = stored.password;
        }
    } else if listed_open == Some(false) {
        return Err(FormError::BadValue);
    }
    Ok(())
}

/// The `key=value` pairs of a form body, undecoded, empty pairs skipped.
struct Pairs<'a>(&'a [u8]);

impl<'a> Iterator for Pairs<'a> {
    type Item = (&'a [u8], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.0.is_empty() {
                return None;
            }
            let (pair, rest) = match self.0.iter().position(|&c| c == b'&') {
                Some(i) => (&self.0[..i], &self.0[i + 1..]),
                None => (self.0, &self.0[self.0.len()..]),
            };
            self.0 = rest;
            if pair.is_empty() {
                continue;
            }
            return Some(match pair.iter().position(|&c| c == b'=') {
                Some(i) => (&pair[..i], &pair[i + 1..]),
                None => (pair, &pair[pair.len()..]),
            });
        }
    }
}

/// The new token from a `POST /token` body: the `token` key, decoded and
/// checked against `settings::valid_token`; empty clears it. Any other
/// key is unknown, so a settings form posted here by mistake changes
/// nothing.
pub fn parse_token(body: &[u8]) -> Result<Secret<TOKEN_MAX>, FormError> {
    let mut token = None;
    for (key, value) in Pairs(body) {
        if key != b"token" {
            return Err(FormError::UnknownKey);
        }
        let mut vbuf = [0u8; VALUE_MAX];
        let vn = percent_decode(value, &mut vbuf).ok_or(FormError::BadValue)?;
        let value = core::str::from_utf8(&vbuf[..vn]).map_err(|_| FormError::BadValue)?;
        if !valid_token(value) {
            return Err(FormError::BadValue);
        }
        token = Some(Secret(Text::new(value).ok_or(FormError::BadValue)?));
    }
    token.ok_or(FormError::BadValue)
}

/// An optional value as JSON: the value, or `null`.
struct OrNull<T>(Option<T>);

impl<T: core::fmt::Display> core::fmt::Display for OrNull<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match &self.0 {
            Some(v) => write!(f, "{v}"),
            None => f.write_str("null"),
        }
    }
}

/// The timezone field of the settings page and the setup form: the rule, and a button that fills
/// it from the browser's own zone through `/tz.js` (`src/tz.js`), which
/// then says which zone that was in the `<small>` after it. `TZ_SCRIPT`
/// loads the script after the form, so the three elements exist.
fn write_tz_field(out: &mut impl core::fmt::Write, s: &Settings) -> core::fmt::Result {
    write!(
        out,
        "<label>Timezone, POSIX rule <input type=text name=tz id=z maxlength={} value=\"{}\"></label>\
<button type=button id=b>Use this browser's zone</button> <small id=zn></small>",
        TZ_MAX,
        HtmlStr(s.tz.as_str())
    )
}

/// Loads the browser-zone script; after the form, so `z`, `b` and `zn`
/// exist when it runs. The firmware version in the query makes a browser
/// that cached the script for a day (`/tz.js` is cacheable, like the
/// favicon) fetch the new one after an update.
fn write_tz_script(out: &mut impl core::fmt::Write, st: &Status) -> core::fmt::Result {
    write!(out, "<script src=\"/tz.js?v={}\"></script>", HtmlStr(st.version))
}

/// The longest `write_json` writes, which the MQTT task's state buffer
/// is sized from; the test builds the worst case against it (1467
/// bytes: every string full of control characters, six bytes each in
/// JSON, which today's rules let into the SSID alone). `/api` answers
/// from the response buffer, a page's, which is far larger.
pub const JSON_MAX: usize = 1536;

const _: () = assert!(JSON_MAX <= crate::http::PAGE_MAX);

/// The settings as JSON (the SSID, then whether a password and a token
/// are set, never their values), followed by the read-only status keys
/// `version`, `slot`, `clips` (`pack` when the clips are the `clips`
/// partition's, `builtin` when they are the firmware's), `mode`, `uptime` (seconds), `heals`, `watchdog` (resets by it since power-up), `clips_paused` (notes play in place of the clips, one having hung the player), `temp_c` and
/// `temp_f` (tenths, or null with no thermometer), `light` (0 to 100,
/// or null before the first reading), `rssi` (dBm, or null while not
/// connected), `utc_offset` and `dst` (the minutes east of UTC and
/// whether daylight time is in force right now under the `tz` rule,
/// null before the first time sync) and `last_heal` (Unix seconds or
/// null).
pub fn write_json(out: &mut impl core::fmt::Write, s: &Settings, st: &Status) -> core::fmt::Result {
    write!(
        out,
        "{{\"color\":\"{}\",\"brightness\":{},\"alarm\":\"{:02}:{:02}\",\"armed\":{},\"days\":\"",
        PALETTE_NAMES[s.color % PALETTE_NAMES.len()],
        s.brightness_pct(),
        s.alarm_hour,
        s.alarm_minute,
        s.alarm_armed,
    )?;
    write_days(out, s.alarm_days)?;
    write!(
        out,
        "\",\"skip\":{},\"melody\":\"{}\",\"twelve_hour\":{},\"tz\":\"{}\",\"temp_offset\":{},\"temp_show\":\"{}\",\"hostname\":\"{}\",\"tx_power\":{},\"output\":\"{}\",\"ssid\":\"{}\",\"password_set\":{},\"token_set\":{},\
\"mqtt_host\":\"{}\",\"mqtt_port\":{},\"mqtt_user\":\"{}\",\"mqtt_password_set\":{},\"mqtt\":\"{}\",\"ringing\":{},\
\"version\":\"{}\",\"slot\":\"{}\",\"clips\":\"{}\",\"mode\":\"{}\",\"uptime\":{},\"heals\":{},\"watchdog\":{},\"clips_paused\":{},\"temp_c\":{},\"temp_f\":{},\"light\":{},\"rssi\":{},\
\"utc_offset\":{},\"dst\":{},\"last_heal\":",
        s.skip_next,
        melody::name(s.melody),
        s.twelve_hour,
        JsonStr(s.tz.as_str()),
        Tenths(s.temp_offset_dc),
        s.temp_show.name(),
        s.hostname.as_str(),
        tx_power_name(s.tx_power_qdbm).unwrap_or("null"),
        s.output.name(),
        JsonStr(s.wifi.ssid.as_str()),
        !s.wifi.password.0.is_empty(),
        !s.token.0.is_empty(),
        s.mqtt.host.as_str(),
        s.mqtt.port,
        JsonStr(s.mqtt.user.as_str()),
        !s.mqtt.password.0.is_empty(),
        st.mqtt.name(),
        st.ringing,
        st.version,
        st.slot,
        if melody::pack().is_some() { "pack" } else { "builtin" },
        st.mode,
        st.uptime_secs,
        st.heals,
        st.watchdog,
        st.clips_paused,
        OrNull(st.temp_dc.map(Tenths)),
        OrNull(st.temp_dc.map(|dc| Tenths(fahrenheit_tenths(dc)))),
        OrNull(st.light),
        OrNull(st.rssi),
        OrNull(st.local.map(|l| l.offset_secs / 60)),
        OrNull(st.local.map(|l| l.dst)),
    )?;
    match st.last_heal {
        Some(h) => writeln!(out, "{}}}", h.unix_secs),
        None => writeln!(out, "null}}"),
    }
}

/// The status lines under the clock page's forms: the firmware, the
/// network and its signal, the broker, the room, the local time, then
/// uptime and self-heals. `ssid` is the stored network, the only one the station
/// joins.
fn write_status(out: &mut impl core::fmt::Write, s: &Settings, st: &Status) -> core::fmt::Result {
    write_firmware_line(out, st, false)?;
    write_wifi_line(out, s.wifi.ssid.as_str(), st)?;
    write_mqtt_line(out, s, st)?;
    write_room_line(out, st)?;
    write_local_line(out, s, st)?;
    write!(out, "<p><small>Up {}. Self-heal resets since power-up: {}", Span(st.uptime_secs as i64), st.heals)?;
    if let Some(h) = st.last_heal {
        write!(out, ", last {} {:02}:{:02}", WEEKDAY_LABELS[h.weekday as usize % 7], h.hour, h.minute)?;
        if let Some(ago) = h.ago_secs {
            write!(out, " ({} ago)", Span(ago))?;
        }
    }
    out.write_str(".</small></p>")
}

/// The network and the signal from its last beacon, or why not.
fn write_wifi_line(out: &mut impl core::fmt::Write, ssid: &str, st: &Status) -> core::fmt::Result {
    if ssid.is_empty() {
        return out.write_str("<p><small>Wi-Fi: no network stored.</small></p>");
    }
    match st.rssi {
        Some(rssi) => write!(out, "<p><small>Wi-Fi <b>{}</b>, {rssi} dBm.</small></p>", HtmlStr(ssid)),
        None => write!(out, "<p><small>Wi-Fi <b>{}</b>: not connected.</small></p>", HtmlStr(ssid)),
    }
}

/// The broker and whether the clock is on it, or why not.
fn write_mqtt_line(out: &mut impl core::fmt::Write, s: &Settings, st: &Status) -> core::fmt::Result {
    if s.mqtt.is_none() {
        return out.write_str("<p><small>MQTT off.</small></p>");
    }
    write!(out, "<p><small>MQTT {} <b>{}:{}</b>", st.mqtt.name(), s.mqtt.host.as_str(), s.mqtt.port)?;
    if let Link::Connecting { last_error: Some(e) } = st.mqtt {
        write!(out, ", last: {e}")?;
    }
    out.write_str(".</small></p>")
}

/// The sensors: the temperature in both units and the light index, or
/// why not. Nothing is shown before the first reading.
fn write_room_line(out: &mut impl core::fmt::Write, st: &Status) -> core::fmt::Result {
    let Some(light) = st.light else { return Ok(()) };
    match st.temp_dc {
        Some(dc) => write!(out, "<p><small>Room {} °C ({} °F), light {light} %.</small></p>", Tenths(dc), Tenths(fahrenheit_tenths(dc))),
        None => write!(out, "<p><small>Room: no thermometer, light {light} %.</small></p>"),
    }
}

/// The local time under the rule, with the zone's name and the offset in
/// force, for checking a rule: `Local time Fri 14:32 PST, UTC-08:00.`
fn write_local_line(out: &mut impl core::fmt::Write, s: &Settings, st: &Status) -> core::fmt::Result {
    let Some(l) = st.local else {
        return out.write_str("<p><small>Local time: not synced yet.</small></p>");
    };
    let rule = tz::parse(s.tz.as_str()).unwrap_or(tz::UTC);
    let name = match rule.dst {
        Some(d) if l.dst => d.zone.name,
        _ => rule.std.name,
    };
    let (sign, abs) = if l.offset_secs < 0 { ('-', -l.offset_secs) } else { ('+', l.offset_secs) };
    write!(
        out,
        "<p><small>Local time {} {:02}:{:02} {}, UTC{sign}{:02}:{:02}{}.</small></p>",
        WEEKDAY_LABELS[l.weekday as usize % 7],
        l.hour,
        l.minute,
        HtmlStr(name),
        abs / 3600,
        abs % 3600 / 60,
        if l.dst { ", daylight time" } else { "" }
    )
}

/// The firmware and its slot; `link` adds the way to the system page,
/// for the setup form, which has no line of links.
fn write_firmware_line(out: &mut impl core::fmt::Write, st: &Status, link: bool) -> core::fmt::Result {
    write!(out, "<p><small>Firmware {} in {}.", st.version, st.slot)?;
    if link {
        out.write_str(" <a href=/system>Update</a>.")?;
    }
    out.write_str("</small></p>")
}

/// Shared `<head>`: the viewport and the styles of every page. Light
/// or dark as the browser is; the heading, the same on every page, is
/// a fifth smaller than a browser's and centred; a link is one colour, visited or not
/// (`LinkText`, the browser's own for either scheme); the links of the first line (`nav`) are
/// given height to tap and centred, as are Save and the notices above a form (`.c`, and a `p.err` that
/// is not the PIN pages' status line) and the status
/// lines under a page's form (`.st`), which are also set closer than
/// paragraphs are; `.d` is the row of day checkboxes.
const HEAD: &str = "<!doctype html><html><head><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\">\
<style>:root{color-scheme:light dark}body{font-family:sans-serif;max-width:22em;margin:2em auto;padding:0 1em}\
label{display:block;margin:.8em 0}select,input{font-size:1em;width:100%;box-sizing:border-box}input[type=checkbox]{width:auto}input[type=file]{margin:.5em 0}\
button{font-size:1em;padding:.5em 1.5em}p.err{color:#c33;color:light-dark(#b00,#f66)}h1{font-size:1.6em;text-align:center}a{color:LinkText}nav{margin:1em 0;text-align:center}.c,p.err:not(#s){text-align:center}nav a,nav b{display:inline-block;padding:.5em 0}\
.d{margin:.8em 0}.d label{display:inline-block;margin:.3em .7em 0 0}.st{text-align:center}.st p{margin:.2em 0}progress{width:100%}[hidden]{display:none}\
fieldset{border:1px solid #ccc;border-radius:.3em;margin:1em 0;padding:0 1em}</style>";

/// The pages a visitor clicks between; each starts with a line of links
/// to the others.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    /// `/`: the alarm, the display and the status.
    Clock,
    /// `/settings`: the timezone, the panel's options and the hostname.
    Settings,
    /// `/ha`: the MQTT broker.
    HomeAssistant,
    /// `/clips`: the clip pack.
    Clips,
    /// `/system`: the firmware, the token, reboot and the access point.
    System,
}

impl Page {
    pub const ALL: [Page; 5] = [Page::Clock, Page::Settings, Page::HomeAssistant, Page::Clips, Page::System];

    /// Where the page is served, and where its form is posted.
    pub const fn path(self) -> &'static str {
        match self {
            Page::Clock => "/",
            Page::Settings => "/settings",
            Page::HomeAssistant => "/ha",
            Page::Clips => "/clips",
            Page::System => "/system",
        }
    }

    /// The page's name in full, for the title: tabs and the history
    /// tell the pages apart by it.
    const fn title(self) -> &'static str {
        match self {
            Page::Clock => "Clock",
            Page::Settings => "Settings",
            Page::HomeAssistant => "Home Assistant",
            Page::Clips => "Clips",
            Page::System => "System",
        }
    }

    /// The page's name in the line of links.
    const fn label(self) -> &'static str {
        match self {
            Page::Clock => "Clock",
            Page::Settings => "General",
            // Short: five names must fit one line of a phone.
            Page::HomeAssistant => "HA",
            Page::Clips => "Clips",
            Page::System => "System",
        }
    }
}

/// What every page starts with: the head, the hostname and the page's
/// name as title, the hostname as heading, the line of links with this page's own name in bold, and
/// what became of the last post of its form (`Note`). On the access
/// point (`ap`) `/` is the setup form and the two settings pages are
/// not served, so the links are that form, the clips and the system page.
fn write_top(out: &mut impl core::fmt::Write, hostname: &str, here: Page, ap: bool, note: Note) -> core::fmt::Result {
    out.write_str(HEAD)?;
    // The hostname is letters, digits and dashes, so no escaping here.
    let title = if ap && here == Page::Clock { "Setup" } else { here.title() };
    write!(out, "<title>{hostname}: {title} - onebitdawn</title></head><body><h1>onebitdawn: {hostname}</h1><nav>")?;
    let mut first = true;
    for page in Page::ALL {
        if ap && matches!(page, Page::Settings | Page::HomeAssistant) {
            continue;
        }
        if !first {
            out.write_str(" · ")?;
        }
        first = false;
        let label = if ap && page == Page::Clock { "Setup" } else { page.label() };
        if page == here {
            write!(out, "<b>{label}</b>")?;
        } else {
            write!(out, "<a href={}>{label}</a>", page.path())?;
        }
    }
    out.write_str("</nav>")?;
    write_note(out, note)
}

/// A checkbox that is posted either way: a hidden `0` under the same
/// name before it, so the form says off for an unticked box and the
/// later `on` wins for a ticked one (`apply`).
struct Check(&'static str, bool);

impl core::fmt::Display for Check {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "<input type=hidden name={0} value=0><input type=checkbox name={0}{1}>", self.0, if self.1 { " checked" } else { "" })
    }
}

/// How a settings form ends: the line a script shows when a post of
/// its own got no answer (`E`), then Save, centred, and under it the
/// mark `UNSAVED_JS` shows, so the button does not move when it
/// appears.
const SAVE: &str = "<p class=err id=e role=alert hidden>The clock did not answer; try again.</p><p class=c><button>Save</button><br><b id=u hidden>Changes not saved yet.</b></p>";

/// The script of a page with a settings form, without its tags: the
/// unsaved-changes mark, shown while any named control of the first
/// form differs from what the page was served with. It also takes the
/// notice's query (`?saved=1`, `?error=…`) off the address, so a reload
/// does not say it again. And it posts the form itself, only the
/// names with a control that differs (`G`), each with every pair the
/// form has under it (a checkbox with its hidden `0`, the days all
/// that are ticked), so a tab opened a while ago puts back nothing it
/// did not change (`X` is the
/// notice of a refusal, in this page or in an answer): a refused
/// form's notice is taken out of the answer and put above the form,
/// which stays as it was typed, and a saved one goes on to the page
/// the redirect named. A request that fails shows the line above Save
/// (`E`) and leaves the form as it is, so Save again posts the same
/// fields; the browser's own submit, which posts the whole form, is
/// only for a page without script.
const UNSAVED_JS: &str = "var D=document,f=D.forms[0],u=D.getElementById('u'),g=D.getElementById('e'),X='p.err:not([id])';function E(h){g.hidden=h}history.replaceState(0,'',location.pathname);\
function G(){var s=new Set;for(var e of f.elements)if(e.name&&(e.type=='checkbox'?e.checked!=e.defaultChecked:\
e.options?!e.options[e.selectedIndex].defaultSelected:e.value!=e.defaultValue))s.add(e.name);return s}\
f.onsubmit=function(e){e.preventDefault();E(1);var s=G(),b=new URLSearchParams;new FormData(f).forEach(function(v,k){if(s.has(k))b.append(k,v)});\
fetch(f.action,{method:'POST',body:b}).then(function(r){return r.text().then(function(h){\
var n=new DOMParser().parseFromString(h,'text/html').querySelector(X),o=D.querySelector(X);if(!n){location.replace(r.url);return}\
if(o)o.remove();f.before(n);n.scrollIntoView({block:'center'})})}).catch(function(){E(0)})};\
f.oninput=function(){u.hidden=!G().size};";

/// The clock page's own script, after `UNSAVED_JS`. `P` posts a body
/// and fails on anything but a 2xx or the redirect Play and Stop answer
/// with; `E`, the form script's, shows or hides the line that says a
/// post failed. Play and
/// Stop become plain buttons, so that Enter in the alarm form is Save
/// (a form's default button is its first submit button, which Play
/// is), and post the chosen tune to the button's own address without
/// leaving the page. The days' presets (`y`) tick the
/// seven boxes (`C`) when one is chosen, before the form's own input
/// handler looks for unsaved changes, and say `Custom` when the boxes
/// are ticked into none of them.
const CLOCK_JS: &str = "var m=D.getElementById('m');\
function P(a,b){return fetch(a,{method:'POST',redirect:'manual',headers:{'Content-Type':'application/x-www-form-urlencoded'},body:b})\
.then(function(r){if(!r.ok&&r.type!='opaqueredirect')throw 0;E(1)})}\
for(let i of'pq'){let b=D.getElementById(i);b.type='button';b.onclick=function(){P(b.formAction,'melody='+m.value).catch(function(){E(0)})}}\
var y=D.getElementById('y'),C=D.querySelectorAll('[name=days][type=checkbox]');y.hidden=false;\
y.oninput=function(){if(y.value)C.forEach(function(c,i){c.checked=y.value>>i&1})};\
f.addEventListener('input',function(e){if(e.target!=y){var k=0;C.forEach(function(c,i){if(c.checked)k|=1<<i});y.value=k;if(y.selectedIndex<0)y.value=''}});";

/// The clock page, `GET /`: one form of the alarm and the display with
/// Save under both, as on the other pages, and the status lines. Save
/// posts the display's two selects with the alarm. The alarm's
/// days are seven checkboxes under one name, and under them is when it
/// rings next (`NextAlarm`). Above Save is
/// the line the script shows when Play, Stop or Save got no answer. `note` is what became of the last post of
/// a form; the page says so above them.
pub fn write_page(
    out: &mut impl core::fmt::Write,
    s: &Settings,
    st: &Status,
    note: Note,
) -> core::fmt::Result {
    write_top(out, s.hostname.as_str(), Page::Clock, false, note)?;
    write!(
        out,
        "<form method=post action=/><fieldset><legend>Alarm</legend>\
<label>{} Alarm armed</label>\
<label>Alarm time <input type=time name=alarm value={:02}:{:02}></label>\
<div class=d>Alarm days{}</div>\
<label>{} Skip the next alarm</label>{}{}\
<label>Alarm sound <select name=melody id=m>",
        Check("armed", s.alarm_armed),
        s.alarm_hour,
        s.alarm_minute,
        DayChecks(s.alarm_days),
        Check("skip", s.skip_next),
        NextAlarm(s, st.local),
        ClipsPaused(st.clips_paused),
    )?;
    for (i, m) in melody::all().enumerate() {
        write!(out, "<option value={}{}>{}</option>", m.name, if i == s.melody { " selected" } else { "" }, HtmlStr(m.label))?;
    }
    write!(
        out,
        "<option value={}{}>Random tune</option></select></label>\
<p style=display:flex;gap:1em><button formaction=/play id=p style=flex:1>Play</button><button formaction=/stop id=q style=flex:1>Stop</button></p></fieldset>\
<fieldset><legend>Display</legend><label>Colour <select name=color>",
        melody::RANDOM_NAME,
        if s.melody == melody::RANDOM { " selected" } else { "" }
    )?;
    for (i, name) in PALETTE_NAMES.iter().enumerate() {
        // The names are lowercase ASCII; the label has a capital.
        let (initial, rest) = (name.as_bytes()[0].to_ascii_uppercase() as char, &name[1..]);
        write!(out, "<option value={name}{}>{initial}{rest}</option>", if i == s.color { " selected" } else { "" })?;
    }
    out.write_str("</select></label><label>Brightness <select name=brightness>")?;
    for (i, pct) in BRIGHTNESS_STEPS.iter().enumerate() {
        write!(out, "<option value={pct}{}>{pct} %</option>", if i == s.brightness { " selected" } else { "" })?;
    }
    write!(out, "</select></label></fieldset>{SAVE}</form><script>")?;
    out.write_str(UNSAVED_JS)?;
    out.write_str(CLOCK_JS)?;
    out.write_str("</script><div class=st>")?;
    write_status(out, s, st)?;
    out.write_str("</div></body></html>")
}

/// The settings page, `GET /settings`: what is set once. The timezone
/// with the browser-zone button, the 12 hour clock, the temperature on
/// the panel and the thermometer's offset, the hostname; under the form
/// the local time the rule gives, for checking it.
pub fn write_settings_page(
    out: &mut impl core::fmt::Write,
    s: &Settings,
    st: &Status,
    note: Note,
) -> core::fmt::Result {
    write_top(out, s.hostname.as_str(), Page::Settings, false, note)?;
    out.write_str("<form method=post action=/settings><fieldset><legend>Time</legend>")?;
    write_tz_field(out, s)?;
    write!(
        out,
        "<label>{} 12 hour clock</label></fieldset>\
<fieldset><legend>Temperature</legend>\
<label>Temperature on the panel <select name=temp_show>{}</select></label>\
<label>Thermometer offset, °C <input type=number name=temp_offset min=-{tlim} max={tlim} step=0.1 value={}></label></fieldset>\
<fieldset><legend>Network</legend>\
<label>Hostname, the clock's name on the network <input type=text name=hostname maxlength={hmax} pattern=\"[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?\" value=\"{}\"></label>\
{}\
<p><small>Leave the power at {dflt} dBm unless this clock's antenna needs less: with too little the clock may not reach the network at all. \
If that happens, hold both buttons for 10 s: the clock's own access point starts, and its page has this setting too.</small></p></fieldset>\
<fieldset><legend>Sound</legend>\
<label>Sound output <select name=output>{}</select></label>\
<p><small>Choose whichever is fitted in your clock.</small></p></fieldset>\
{SAVE}</form><script>{UNSAVED_JS}</script>",
        Check("twelve_hour", s.twelve_hour),
        TempShowOptions(s.temp_show),
        Tenths(s.temp_offset_dc),
        s.hostname.as_str(),
        TxPowerField(s.tx_power_qdbm),
        AudioOutOptions(s.output),
        dflt = TX_POWER_DEFAULT,
        tlim = Tenths(TEMP_OFFSET_LIMIT_DC),
        hmax = HOSTNAME_MAX,
    )?;
    write_tz_script(out, st)?;
    out.write_str("<div class=st>")?;
    write_local_line(out, s, st)?;
    out.write_str("</div></body></html>")
}

/// The Home Assistant page, `GET /ha`: the MQTT broker, and under the
/// form what the clock's connection to it is doing. The password field
/// is write-only and says whether one is stored; `apply` refuses a new
/// host without it. The script asks `/api` every 5 s for two minutes
/// and loads the page again when the connection is no longer in the
/// state the page was served with (a save is followed by `connecting`,
/// then `connected` or the error), unless the form has unsaved changes.
pub fn write_ha_page(
    out: &mut impl core::fmt::Write,
    s: &Settings,
    st: &Status,
    note: Note,
) -> core::fmt::Result {
    write_top(out, s.hostname.as_str(), Page::HomeAssistant, false, note)?;
    write!(
        out,
        "<p>Enter your MQTT broker (MQTT 5) and the clock shows up in Home Assistant by itself.</p>\
<form method=post action=/ha><fieldset><legend>MQTT broker</legend>\
<label>Host, empty for none <input type=text name=mqtt_host maxlength={bmax} pattern=\"[A-Za-z0-9.-]*\" value=\"{}\"></label>\
<label>Port <input type=number name=mqtt_port min=1 max=65535 value={}></label>\
<label>User <input type=text name=mqtt_user maxlength={umax} autocomplete=off value=\"{}\"></label>\
<label>Password ({}) <input type=password name=mqtt_password maxlength={pmax} autocomplete=new-password></label></fieldset>\
{SAVE}</form><script>{UNSAVED_JS}\
var M='{}',c=24,i=setInterval(function(){{if(!c--)return clearInterval(i);fetch('/api',{{cache:'no-store'}}).then(function(r){{return r.json()}})\
.then(function(j){{if(j.mqtt!=M&&u.hidden)location.replace('/ha')}})}},5000)</script>",
        s.mqtt.host.as_str(),
        s.mqtt.port,
        HtmlStr(s.mqtt.user.as_str()),
        if s.mqtt.password.0.is_empty() { "none is stored" } else { "one is stored; empty keeps it for the same host" },
        st.mqtt.name(),
        bmax = BROKER_HOST_MAX,
        umax = BROKER_USER_MAX,
        pmax = BROKER_PASSWORD_MAX,
    )?;
    out.write_str("<div class=st>")?;
    write_mqtt_line(out, s, st)?;
    out.write_str("</div></body></html>")
}

/// The start of the script of a page whose actions need the PIN, the
/// system page and the clips page. A PIN is asked for when an action's
/// button is clicked, not when the page loads: `ask` disables the
/// action buttons (`B`, those inside a fieldset), has the clock issue a
/// PIN (`POST /pin`; the page's `GET` issues none, since any site can
/// make a browser send a `GET`, and a post from this page is one the
/// server can tell from theirs, `http::admit`; a clock locked out by a
/// wrong PIN says so there), moves the PIN's form (`PIN_FORM`) under
/// that button and shows it. Its OK hands the digits to the action,
/// its Cancel enables the buttons again. `say` writes the status line
/// `s` (an error in red) and brings it into view; `go` opens a request
/// with the PIN in the header, which is what a script sends with its
/// token, so the server needs no multipart for a file, and after an
/// answer that is not the action's success enables the buttons, so the
/// next click asks for a new PIN with the file and the token still
/// there; `back` polls `/api` until the clock answers again; `up`
/// makes the Upload button send the chosen file as the raw body of a
/// route, with `XMLHttpRequest` for the progress bar.
const PIN_JS: &str = "<script>\
var $=function(i){return document.getElementById(i)},t=$('t'),s=$('s'),q=$('q'),w=$('w'),f=$('f'),p=$('p'),k=$('k'),a=$('a'),\
B=document.querySelectorAll('fieldset button'),G;\
function dis(d){for(var b of B)b.disabled=d}\
function say(m,e){s.textContent=m;s.className=e?'err':'';s.scrollIntoView({block:'center'})}\
function at(b){b.parentNode.after(w)}\
function ask(b,run){at(b);dis(1);say('');fetch('/pin',{method:'POST'}).then(function(r){return r.text().then(function(m){\
if(!r.ok){say(m.trim()+'.',1);dis(0);return}G=run;t.value='';q.hidden=false;t.focus()})})\
.catch(function(){say('The clock did not answer.',1);dis(0)})}\
q.onsubmit=function(e){e.preventDefault();q.hidden=true;G(t.value)};\
$('x').onclick=function(){q.hidden=true;dis(0)};\
function go(u,pin,ok){var x=new XMLHttpRequest();x.open('POST',u);x.setRequestHeader('Authorization','PIN '+pin);\
x.onload=function(){var m=x.responseText.trim();if(x.status==200)ok(m);else{say(x.status+': '+m+'.',1);dis(0)}};\
x.onerror=function(){say('Request failed: connection lost.',1);dis(0)};return x}\
function poll(n){fetch('/api',{cache:'no-store'}).then(function(x){return x.json()})\
.then(function(j){say('Back up: firmware '+j.version+' in '+j.slot+', clips: '+j.clips+'.');dis(0)})\
.catch(function(){if(n>0)setTimeout(function(){poll(n-1)},2000);else{say('The clock has not come back; check it.',1);dis(0)}})}\
function back(m){say(m+'. Waiting for the clock to come back.');setTimeout(function(){poll(20)},5000)}\
function up(u){$('b').onclick=function(){if(!f.files[0]){at(this);say('Choose a file.',1);return}\
ask(this,function(pin){var x=go(u,pin,back);\
x.upload.onprogress=function(e){if(e.lengthComputable)p.value=e.loaded/e.total};\
p.hidden=false;p.value=0;say('Uploading.');x.send(f.files[0])})}}";

/// How such a script ends.
const PIN_END: &str = "</script></body></html>";

/// The system page's own part of the script: the four actions. A token
/// is held to its rule before a PIN is asked for. The access-point
/// button is absent on the access point.
const SYSTEM_JS: &str = "up('/ota');\
$('c').onclick=function(){var v=k.value;\
if(v&&!/^[!-~]{8,32}$/.test(v)){at(this);say('A token is 8 to 32 printable characters without spaces.',1);return}\
if(!v&&!confirm('Clear the token? The scripts will be refused until one is set again.'))return;\
ask(this,function(pin){var x=go('/token',pin,function(m){say(m+'.');dis(0)});\
x.setRequestHeader('Content-Type','application/x-www-form-urlencoded');x.send('token='+encodeURIComponent(v))})};\
$('r').onclick=function(){ask(this,function(pin){go('/reload',pin,back).send()})};\
if(a)a.onclick=function(){if(!confirm('Leave the network? The clock becomes an access point until a network is saved there, or both buttons are held for ten seconds.'))return;\
ask(this,function(pin){go('/ap',pin,function(m){say(m+'. Join the clock\\'s own network and open http://192.168.4.1/.')}).send()})};";

/// The PIN's form and the status line under it, at the end of a page
/// whose actions need the PIN; the script moves the two (`w`) under
/// the button that was clicked. The form is hidden until a PIN has
/// been issued; the browser holds the field to four digits.
const PIN_FORM: &str = "<div id=w><form id=q hidden><label>PIN the clock is showing\
<input type=text inputmode=numeric pattern=[0-9]{4} maxlength=4 required autocomplete=off id=t></label>\
<p><button>OK</button> <button type=button id=x>Cancel</button></p></form><p id=s role=status></p></div>";

/// The system page, `GET /system`: what needs someone who can see the
/// clock. It has four actions, each the request a script makes with
/// `Bearer`, sent with `PIN` instead: the firmware image as the raw
/// body of `/ota`, a new API token to `/token` (empty clears it, after
/// the browser's `confirm()`, since that closes the script paths),
/// `/reload` and `/ap`. A click on an action's button puts a
/// four-digit PIN on the panel and the field for it under the button
/// (`PIN_JS`); a PIN is one action, and the next click draws another. The page says little: what the token is for and
/// its rules are the manual's (`MANUAL.md`), and a bad one is refused with them. `token_set` says whether a token is set now.
/// On the access point (`st.mode`) the button
/// that leads there is left out.
pub fn write_system_page(
    out: &mut impl core::fmt::Write,
    hostname: &str,
    st: &Status,
    token_set: bool,
) -> core::fmt::Result {
    let ap = st.mode == "ap";
    write_top(out, hostname, Page::System, ap, Note::None)?;
    out.write_str("<p>Each action here asks for a PIN, which the clock then shows on its panel.</p><fieldset><legend>Firmware update</legend>")?;
    write_firmware_line(out, st, false)?;
    write!(
        out,
        "<label>Image file (<code>onebitdawn-ota-*.bin</code> from the <a href=https://github.com/anarsoul/onebitdawn/releases target=_blank rel=noopener>releases</a>)<input type=file id=f></label>\
<p><button id=b>Upload</button></p><progress id=p value=0 max=1 hidden></progress>\
<p><small>Uploading asks for a PIN, which the clock then shows on its panel. The panel goes dark while the image is written. Do not unplug.</small></p></fieldset>\
<fieldset><legend>API token</legend>\
<label>New token, empty to clear ({})<input type=text maxlength={TOKEN_MAX} autocomplete=off id=k></label>\
<p><button id=c>Set token</button></p></fieldset>\
<fieldset><legend>Restart</legend><p style=display:flex;justify-content:space-between><button id=r>Reboot</button>{}</p></fieldset>",
        if token_set { "one is set" } else { "none is set" },
        if ap { "" } else { "<button id=a>Access point</button>" },
    )?;
    out.write_str(PIN_FORM)?;
    out.write_str(PIN_JS)?;
    out.write_str(SYSTEM_JS)?;
    out.write_str(PIN_END)
}

/// The clips page, `GET /clips`: where the clips come from now, the
/// pack in the `clips` partition or the firmware, with their labels,
/// and the upload of a pack, the raw body of `POST /clips` with the
/// PIN, asked for at the click as the system page's upload has it. A clock whose partition
/// table has no `clips` partition (`st.clips_room`) is told so, and
/// has nothing to upload to: no field, no script, no PIN asked for.
pub fn write_clips_page(out: &mut impl core::fmt::Write, hostname: &str, st: &Status) -> core::fmt::Result {
    write_top(out, hostname, Page::Clips, st.mode == "ap", Note::None)?;
    out.write_str("<fieldset><legend>Clip pack</legend><p>")?;
    let kb = |bytes: u32| bytes.div_ceil(1024);
    match (melody::pack(), st.clips_room) {
        (Some(pack), room) => {
            write!(out, "Pack in flash: {} clips, {} KB", pack.count(), kb(pack.len() as u32))?;
            if let Some(room) = room {
                write!(out, " of {}", kb(room))?;
            }
            out.write_str(".")?;
        }
        (None, Some(room)) => write!(out, "No pack in flash ({} KB for one): the clips are those built into the firmware.", kb(room))?,
        (None, None) => out.write_str(
            "This clock's partition table has no <code>clips</code> partition, so its clips are those built into the firmware. \
A pack needs the clock flashed once over USB or from the web installer.",
        )?,
    }
    out.write_str("</p><p>")?;
    let mut clips = melody::all().filter(|m| m.clip.is_some()).peekable();
    if clips.peek().is_none() {
        out.write_str("No clips: the note melodies only.")?;
    }
    for (i, m) in clips.enumerate() {
        write!(out, "{}{}", if i == 0 { "" } else { " · " }, HtmlStr(m.label))?;
    }
    out.write_str("</p>")?;
    if st.clips_room.is_none() {
        return out.write_str("</fieldset></body></html>");
    }
    out.write_str(
        "<label>Pack file (<code>onebitdawn-clips-*.bin</code> from the <a href=https://github.com/anarsoul/onebitdawn/releases target=_blank rel=noopener>releases</a>, or <a href=https://anarsoul.github.io/onebitdawn/pack/ target=_blank rel=noopener>make one</a>)<input type=file id=f></label>\
<p><button id=b>Upload</button></p><progress id=p value=0 max=1 hidden></progress>\
<p><b>A new pack replaces all the clips of the one in flash; it does not add to them.</b></p>\
<p><small>Uploading asks for a PIN, which the clock then shows on its panel. The panel goes dark while the pack is written, and the clock restarts. Do not unplug.</small></p></fieldset>",
    )?;
    out.write_str(PIN_FORM)?;
    out.write_str(PIN_JS)?;
    out.write_str("up('/clips');")?;
    out.write_str(PIN_END)
}

/// Bytes the setup page's list of networks may take, strongest first.
/// A plain 32-byte SSID is about 104 bytes of `<option>`; one of 32 `"`
/// is about 424, since `HtmlStr` writes it twice. Twelve plain ones fit;
/// the test `provision_page_budget_is_covered` locks this against the
/// rest of the page and `PAGE_MAX`.
pub const NETWORK_LIST_BUDGET: usize = 1900;

/// A `Write` that only measures. What an `<option>` will cost before it
/// is written, exact by construction, so no arithmetic repeats the
/// format string.
#[derive(Default)]
struct Counter(usize);

impl core::fmt::Write for Counter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0 += s.len();
        Ok(())
    }
}

/// One network of the setup page's list.
fn write_network_option(out: &mut impl core::fmt::Write, n: &Network, stored: &str) -> core::fmt::Result {
    write!(
        out,
        "<option value=\"{}\"{}>{} ({} dBm{})</option>",
        HtmlStr(n.ssid.as_str()),
        if n.ssid.as_str() == stored { " selected" } else { "" },
        HtmlStr(n.ssid.as_str()),
        n.rssi,
        if n.open { ", open" } else { "" },
    )
}

/// The provisioning page, what `GET /` serves while the clock is an
/// access point: the networks the scan saw as a list, a field for one
/// it did not (hidden, or out of range at the time), the password, and
/// the timezone and the hostname since they are typed too; nothing the
/// panel cannot show without a time. The password field is empty
/// whatever is stored (write-only); empty on submit means an open
/// network. The Rescan button posts the form to `/scan`, which scans
/// and comes back here. A refused form says which field, as the
/// station's pages do (`Note`). The list stops at `NETWORK_LIST_BUDGET` bytes,
/// so a few hostile SSIDs of 32 `"` cannot push the page past the
/// response buffer; what was left out is counted under the list and
/// can be typed.
pub fn write_provision_page(
    out: &mut impl core::fmt::Write,
    s: &Settings,
    st: &Status,
    networks: &[Network],
    note: Note,
) -> core::fmt::Result {
    out.write_str(HEAD)?;
    out.write_str("<title>onebitdawn setup</title></head><body><h1>onebitdawn setup</h1>")?;
    match note {
        Note::None | Note::Saved => out.write_str("<p>The clock has no network to join, or was asked to wait for one. Give it yours and it restarts into it.</p>")?,
        Note::Refused(FormError::BadValue, None) => out.write_str("<p class=err>Nothing was saved: no network was named, or a secured network was given no password.</p>")?,
        Note::Refused(FormError::Forbidden | FormError::BrokerPassword, _) => out.write_str("<p class=err>Nothing was saved.</p>")?,
        refused => write_note(out, refused)?,
    }
    let stored = s.wifi.ssid.as_str();
    out.write_str("<form method=post action=/><label>Network <select name=ssid_pick id=n>")?;
    let mut room = NETWORK_LIST_BUDGET;
    let mut shown = 0;
    let mut listed = false;
    for n in networks {
        let mut cost = Counter::default();
        write_network_option(&mut cost, n, stored)?;
        if cost.0 > room {
            break;
        }
        room -= cost.0;
        write_network_option(out, n, stored)?;
        shown += 1;
        listed |= n.ssid.as_str() == stored;
    }
    write!(
        out,
        "<option value=\"\"{}>Other, typed below</option></select></label>\
<label id=o>Other network name <input type=text name=ssid id=i maxlength={smax} value=\"{}\"></label>\
<label>Password, {} <input type=password name=password id=k minlength={pmin} maxlength={pmax} autocomplete=off></label>\
<label id=l hidden><input type=checkbox id=v> Show the password</label>",
        if listed { "" } else { " selected" },
        HtmlStr(if listed { "" } else { stored }),
        if stored.is_empty() { "empty for an open network" } else { "empty to keep the stored one" },
        smax = SSID_MAX,
        pmin = PASSWORD_MIN,
        pmax = PASSWORD_MAX,
    )?;
    write_tz_field(out, s)?;
    write!(
        out,
        "<label>Hostname, the clock's name on the network <input type=text name=hostname maxlength={hmax} pattern=\"[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?\" value=\"{}\"></label>\
{}<p><small>The power the clock joins the network with: {dflt} dBm unless its antenna needs less.</small></p>\
<p class=c><button>Save and join</button> <button formaction=/scan formnovalidate id=r>Rescan</button></p></form>",
        s.hostname.as_str(),
        TxPowerField(s.tx_power_qdbm),
        dflt = TX_POWER_DEFAULT,
        hmax = HOSTNAME_MAX,
    )?;
    write_tz_script(out, st)?;
    out.write_str("<div class=st>")?;
    if networks.is_empty() {
        out.write_str("<p><small>No networks seen yet; rescan, or type the name.</small></p>")?;
    } else if shown < networks.len() {
        write!(out, "<p><small>{} more not shown; type the name.</small></p>", networks.len() - shown)?;
    }
    // The typed-name field only when the list's choice is "Other":
    // disabled as well as hidden, since a hidden input is still posted.
    // The password's checkbox has no name, so it is not posted, and is
    // shown by the script that makes it work. Rescan by script keeps
    // what was typed: it posts `/scan`, whose answer is the redirect
    // to this form, and takes only the list out of that, with the
    // pick kept if it is still listed. A refused form comes back by a
    // redirect, as a new page: what was in it, the password apart, is
    // kept in the tab's session storage at the submit and put back
    // when the page says it was refused.
    out.write_str(
        "<script>var $=function(i){return document.getElementById(i)},n=$('n'),o=$('o'),i=$('i'),k=$('k'),v=$('v');\
function f(){o.hidden=i.disabled=n.value!=''}n.onchange=f;\
try{var F=document.forms[0],S=sessionStorage,K=['ssid_pick','ssid','tz','hostname','tx_power'];\
if(/[?&]error=/.test(location.search)){for(var x of K){var y=S.getItem(x);if(y!=null)F[x].value=y}if(n.selectedIndex<0)n.value=''}\
F.onsubmit=function(){for(var x of K)S.setItem(x,F[x].value)}}catch(e){}f();\
$('l').hidden=false;v.onchange=function(){k.type=v.checked?'text':'password'};\
$('r').onclick=function(e){e.preventDefault();var b=this,p=n.value;b.disabled=true;b.textContent='Scanning…';\
fetch('/scan',{method:'POST'}).then(function(r){return r.text()}).then(function(h){\
n.innerHTML=new DOMParser().parseFromString(h,'text/html').getElementById('n').innerHTML;\
for(var o of n.options)if(o.value==p)n.value=p;f()}).catch(function(){}).then(function(){b.disabled=false;b.textContent='Rescan'})}</script>",
    )?;
    write_firmware_line(out, st, true)?;
    out.write_str("</div></body></html>")
}

/// The answer to a provisioning save: the clock is about to restart
/// into the named network.
pub fn write_saved_page(out: &mut impl core::fmt::Write, s: &Settings) -> core::fmt::Result {
    out.write_str(HEAD)?;
    write!(
        out,
        "<title>onebitdawn setup</title></head><body><h1>Saved</h1>\
<p>The clock restarts now and joins <b>{}</b>. Join that network yourself and open \
<a href=\"http://{}/\">http://{}/</a>, or the address the clock shows when both buttons are held.</p>\
<p><small>If it does not appear within a minute the password was wrong: the panel stays blank, and holding both buttons for ten seconds brings this page back.</small></p>\
</body></html>",
        HtmlStr(s.wifi.ssid.as_str()),
        s.hostname.as_str(),
        s.hostname.as_str(),
    )
}

/// The default transmit power as the pages name it, in dBm.
const TX_POWER_DEFAULT: &str = match tx_power_name(DEFAULT_TX_POWER_QDBM) {
    Some(name) => name,
    None => panic!("the default transmit power is not a level"),
};

/// The transmit power select with its label, of the settings page and
/// the setup form.
struct TxPowerField(i8);

impl core::fmt::Display for TxPowerField {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("<label>Wi-Fi transmit power, dBm <select name=tx_power>")?;
        for (name, qdbm) in TX_POWER_CHOICES {
            write!(f, "<option{}>{name}</option>", if *qdbm == self.0 { " selected" } else { "" })?;
        }
        f.write_str("</select></label>")
    }
}

/// The `<option>`s of the temperature-on-the-panel select.
struct TempShowOptions(TempShow);

impl core::fmt::Display for TempShowOptions {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (name, label) in TempShow::NAMES.iter().zip(TempShow::LABELS) {
            write!(f, "<option value={name}{}>{label}</option>", if *name == self.0.name() { " selected" } else { "" })?;
        }
        Ok(())
    }
}

/// The `<option>`s of the sound output select.
struct AudioOutOptions(AudioOut);

impl core::fmt::Display for AudioOutOptions {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (name, label) in AudioOut::NAMES.iter().zip(AudioOut::LABELS) {
            write!(f, "<option value={name}{}>{label}</option>", if *name == self.0.name() { " selected" } else { "" })?;
        }
        Ok(())
    }
}

/// The alarm's days as seven checkboxes, all named `days`: `apply`
/// reads the repeats of one body as a union. Above them a select of
/// the presets, which ticks the boxes and follows them.
struct DayChecks(u8);

impl core::fmt::Display for DayChecks {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The presets, above the boxes: a select with no name, so it is
        // not posted, whose value is the mask it ticks; shown by the
        // script that makes it work (`CLOCK_JS`).
        f.write_str("<select id=y hidden>")?;
        let presets = [(DAYS_ALL, "Every day"), (DAYS_WEEKDAYS, "Weekdays"), (DAYS_WEEKEND, "Weekend")];
        for (mask, label) in presets {
            write!(f, "<option value={mask}{}>{label}</option>", if self.0 == mask { " selected" } else { "" })?;
        }
        let custom = presets.iter().all(|(mask, _)| *mask != self.0);
        write!(f, "<option value=\"\"{}>Custom</option></select><br>", if custom { " selected" } else { "" })?;
        for (i, name) in DAY_NAMES.iter().enumerate() {
            let checked = if self.0 & (1 << i) != 0 { " checked" } else { "" };
            write!(f, "<label><input type=checkbox name=days value={name}{checked}> {}</label>", WEEKDAY_LABELS[i])?;
        }
        Ok(())
    }
}

/// When the alarm rings next, by the weekday and the minute of the
/// page's local time: the weekday it rings on and, with `skip_next`
/// set, the weekday of the one skipped before it. `None` while it is
/// disarmed or has no day. The minute it rings in counts as past.
pub fn next_alarm(s: &Settings, now: &LocalNow) -> Option<(u8, Option<u8>)> {
    if !s.alarm_armed {
        return None;
    }
    let past = (now.hour, now.minute) >= (s.alarm_hour, s.alarm_minute);
    let mut skipped = None;
    for ahead in u8::from(past)..15 {
        let day = (now.weekday % 7 + ahead) % 7;
        if s.alarm_days & (1 << day) == 0 {
            continue;
        }
        if s.skip_next && skipped.is_none() {
            skipped = Some(day);
            continue;
        }
        return Some((day, skipped));
    }
    None
}

/// The line under the skip checkbox that says so; nothing before the
/// time is known.
struct NextAlarm<'a>(&'a Settings, Option<LocalNow>);

impl core::fmt::Display for NextAlarm<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Some((day, skipped)) = self.1.and_then(|now| next_alarm(self.0, &now)) else { return Ok(()) };
        write!(f, "<p><small>Next alarm: {} {:02}:{:02}", WEEKDAY_LABELS[day as usize], self.0.alarm_hour, self.0.alarm_minute)?;
        if let Some(skipped) = skipped {
            write!(f, " ({} skipped)", WEEKDAY_LABELS[skipped as usize])?;
        }
        f.write_str(".</small></p>")
    }
}

/// Why the alarm beeps where a clip was chosen: shown while the clips
/// are not played (`Status::clips_paused`), whether or not an alarm is
/// armed, since Play beeps as well.
struct ClipsPaused(bool);

impl core::fmt::Display for ClipsPaused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 {
            f.write_str("<p><small>Clips paused: one hung the player. Upload a pack or update the firmware to try again.</small></p>")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::PAGE_MAX;

    fn status(heal: bool) -> Status {
        Status {
            version: "v0.1.0-123-gabcdef0-dirty-and-long",
            slot: "ota_1",
            mode: "sta",
            uptime_secs: 46_296 * 86_400 + 3 * 3600 + 59 * 60,
            heals: 4_000_000_000,
            watchdog: 16_777_215,
            clips_paused: true,
            last_heal: heal.then_some(HealStamp { unix_secs: 1_800_000_000, weekday: 2, hour: 23, minute: 59, ago_secs: Some(400 * 86_400) }),
            temp_dc: Some(-200),
            light: Some(100),
            rssi: Some(-100),
            mqtt: Link::Connecting { last_error: Some("connection refused by the broker") },
            ringing: true,
            clips_room: Some(1 << 20),
            local: Some(LocalNow { weekday: 4, hour: 14, minute: 32, offset_secs: -8 * 3600, dst: false }),
        }
    }

    fn decode(src: &str) -> Option<String> {
        let mut out = [0u8; VALUE_MAX];
        let n = percent_decode(src.as_bytes(), &mut out)?;
        Some(String::from_utf8(out[..n].to_vec()).unwrap())
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(decode("a+b%20c%2Fd").as_deref(), Some("a b c/d"));
        assert_eq!(decode("%zz"), None);
        assert_eq!(decode("%2"), None);
        assert_eq!(decode("%"), None);
        assert_eq!(decode(&"a".repeat(VALUE_MAX)).map(|s| s.len()), Some(VALUE_MAX));
        assert_eq!(decode(&"a".repeat(VALUE_MAX + 1)), None);
    }

    #[test]
    fn tenths_parse() {
        assert_eq!(parse_tenths("-0.7"), Some(-7));
        assert_eq!(parse_tenths("0.7"), Some(7));
        assert_eq!(parse_tenths("+2.5"), Some(25));
        assert_eq!(parse_tenths("1"), Some(10));
        assert_eq!(parse_tenths("-10"), Some(-100));
        assert_eq!(parse_tenths("0"), Some(0));
        assert_eq!(parse_tenths("-0.0"), Some(0));
        assert_eq!(parse_tenths("3276.7"), Some(32767));
        for bad in ["", "-", "+", ".", ".5", "-.5", "1.", "0.75", "1.5.1", "abc", "1e1", " 1", "32768", "3276.8", "99999", "-1-", "1.x"] {
            assert_eq!(parse_tenths(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn days_parse_and_print() {
        for (text, mask) in [("all", DAYS_ALL), ("weekdays", DAYS_WEEKDAYS), ("weekend", DAYS_WEEKEND), ("mon,wed", 0b0000101), ("sun", 0x40)] {
            assert_eq!(parse_days(text), Some(mask), "{text}");
            let mut out = String::new();
            write_days(&mut out, mask).unwrap();
            assert_eq!(out, text);
        }
        assert_eq!(parse_days(" tue , fri"), Some(0b0010010));
        assert_eq!(parse_days(""), None);
        assert_eq!(parse_days("mon,xyz"), None);
        assert_eq!(parse_days("mon,"), None);
    }

    #[test]
    fn apply_sets_every_key() {
        let mut s = Settings::DEFAULT;
        let body = b"color=cyan&brightness=25&alarm=06%3A30&armed=on&days=weekend&skip=1&twelve_hour=true&tz=-420&temp_offset=-0.7&temp_show=f&hostname=Bedroom-Clock&tx_power=8.5&output=line&melody=random";
        apply(&mut s, body, Allow::NONE).unwrap();
        assert_eq!(s.color, PALETTE_NAMES.iter().position(|n| *n == "cyan").unwrap());
        assert_eq!(s.brightness_pct(), 25);
        assert_eq!((s.alarm_hour, s.alarm_minute), (6, 30));
        assert!(s.alarm_armed && s.skip_next && s.twelve_hour);
        assert_eq!(s.alarm_days, DAYS_WEEKEND);
        assert_eq!(s.tz, tz::fixed(-420), "minutes become a fixed rule");
        assert_eq!(s.temp_offset_dc, -7);
        assert_eq!(s.temp_show, TempShow::Fahrenheit);
        assert_eq!(s.hostname.as_str(), "bedroom-clock");
        assert_eq!(s.tx_power_qdbm, 34);
        assert_eq!(s.output, AudioOut::Line);
        assert_eq!(s.melody, melody::RANDOM);
        assert!(s.is_valid());
        // The broker keys, with the page's allowance.
        apply(&mut s, b"mqtt_host=10.1.1.5&mqtt_port=1884&mqtt_user=clock&mqtt_password=p%26ss+word", Allow::PAGE).unwrap();
        assert_eq!((s.mqtt.host.as_str(), s.mqtt.port, s.mqtt.user.as_str(), s.mqtt.password.0.as_str()), ("10.1.1.5", 1884, "clock", "p&ss word"));
        assert!(s.is_valid());
        // An empty password keeps the stored one; an empty host clears everything and sets the flag.
        apply(&mut s, b"mqtt_password=", Allow::PAGE).unwrap();
        assert_eq!(s.mqtt.password.0.as_str(), "p&ss word");
        assert!(!s.mqtt_cleared);
        apply(&mut s, b"mqtt_host=", Allow::PAGE).unwrap();
        assert!(s.mqtt.is_none() && s.mqtt.user.is_empty() && s.mqtt.password.0.is_empty() && s.mqtt_cleared);
        assert_eq!(s.mqtt.port, 1884, "the port stays");
        apply(&mut s, b"mqtt_host=broker.lan", Allow::PAGE).unwrap();
        assert!(!s.mqtt_cleared);
        assert!(s.mqtt.password.0.is_empty(), "a host set after a clear has no password to hand on");
        // The alarm as Home Assistant's time entity sends it.
        apply(&mut s, b"alarm=07%3A45%3A00", Allow::NONE).unwrap();
        assert_eq!((s.alarm_hour, s.alarm_minute), (7, 45));
        // A key that is not sent is left alone.
        apply(&mut s, b"tz=0", Allow::NONE).unwrap();
        assert!(s.alarm_armed && s.skip_next && s.twelve_hour);
        // An empty body, stray separators and a key without a value.
        apply(&mut s, b"", Allow::NONE).unwrap();
        apply(&mut s, b"&&tz=15&", Allow::NONE).unwrap();
        assert_eq!(s.tz, tz::fixed(15));
        // A rule, as the page posts it and with a quoted name.
        apply(&mut s, b"tz=PST8PDT%2CM3.2.0%2CM11.1.0", Allow::NONE).unwrap();
        assert_eq!(s.tz.as_str(), "PST8PDT,M3.2.0,M11.1.0");
        apply(&mut s, b"tz=%3C%2B0545%3E-5%3A45", Allow::NONE).unwrap();
        assert_eq!(s.tz.as_str(), "<+0545>-5:45");
        assert!(s.is_valid());
        assert_eq!(apply(&mut s, b"armed", Allow::NONE), Ok(()));
        assert!(!s.alarm_armed);
    }

    #[test]
    fn apply_refuses_bad_input() {
        let s0 = Settings::DEFAULT;
        let try_body = |body: &[u8]| {
            let mut s = s0;
            apply(&mut s, body, Allow::NONE)
        };
        assert_eq!(try_body(b"colour=red"), Err(FormError::UnknownKey));
        assert_eq!(try_body(b"color=pink"), Err(FormError::BadValue));
        assert_eq!(try_body(b"brightness=33"), Err(FormError::BadValue));
        assert_eq!(try_body(b"brightness=10"), Ok(()));
        assert_eq!(try_body(b"tz=America%2FVancouver"), Err(FormError::BadValue));
        assert_eq!(try_body(b"tz=PST"), Err(FormError::BadValue));
        assert_eq!(try_body(b"tz=900"), Err(FormError::BadValue), "minutes past the limit");
        assert_eq!(try_body(b"tz="), Err(FormError::BadValue));
        assert_eq!(try_body(b"alarm=24:00"), Err(FormError::BadValue));
        assert_eq!(try_body(b"alarm=23:60"), Err(FormError::BadValue));
        assert_eq!(try_body(b"alarm=7"), Err(FormError::BadValue));
        assert_eq!(try_body(b"alarm=07:45:60"), Err(FormError::BadValue));
        assert_eq!(try_body(b"alarm=07:45:x"), Err(FormError::BadValue));
        assert_eq!(try_body(b"alarm=07:45:00"), Ok(()));
        assert_eq!(try_body(b"tz=841"), Err(FormError::BadValue));
        assert_eq!(try_body(b"tz=-841"), Err(FormError::BadValue));
        assert_eq!(try_body(b"tz=840"), Ok(()));
        assert_eq!(try_body(b"tz=%zz"), Err(FormError::BadValue));
        assert_eq!(try_body(b"temp_offset=10.1"), Err(FormError::BadValue));
        assert_eq!(try_body(b"temp_offset=-10.1"), Err(FormError::BadValue));
        assert_eq!(try_body(b"temp_offset=10.0"), Ok(()));
        assert_eq!(try_body(b"temp_offset=-10"), Ok(()));
        assert_eq!(try_body(b"temp_offset=0.75"), Err(FormError::BadValue));
        assert_eq!(try_body(b"temp_offset=abc"), Err(FormError::BadValue));
        assert_eq!(try_body(b"temp_show=k"), Err(FormError::BadValue));
        assert_eq!(try_body(b"temp_show=off"), Ok(()));
        assert_eq!(try_body(b"tx_power=16"), Ok(()));
        assert_eq!(try_body(b"tx_power=2"), Ok(()));
        assert_eq!(try_body(b"tx_power=18"), Err(FormError::BadValue), "above the ceiling");
        assert_eq!(try_body(b"tx_power=12"), Err(FormError::BadValue), "not a level the driver has");
        assert_eq!(try_body(b"tx_power=68"), Err(FormError::BadValue), "dBm, not quarters");
        assert_eq!(try_body(b"output=buzzer"), Ok(()));
        assert_eq!(try_body(b"output=split"), Ok(()));
        assert_eq!(try_body(b"output=gpio20"), Err(FormError::BadValue));
        assert_eq!(try_body(b"%zz=1"), Err(FormError::UnknownKey));
        assert_eq!(try_body(b"hostname=-bad"), Err(FormError::BadValue));
        assert_eq!(try_body(b"days="), Err(FormError::BadValue));
        assert_eq!(try_body(b"melody=9"), Err(FormError::BadValue));
        assert_eq!(try_body(b"version=1"), Err(FormError::UnknownKey), "a status key is not a setting");
        assert_eq!(try_body(b"uptime=1"), Err(FormError::UnknownKey));
        // Oversized key and value are reported before parsing.
        let long_key = format!("{}=1", "k".repeat(17));
        assert_eq!(try_body(long_key.as_bytes()), Err(FormError::UnknownKey));
        let long_value = format!("hostname={}", "h".repeat(VALUE_MAX + 1));
        assert_eq!(try_body(long_value.as_bytes()), Err(FormError::BadValue));
    }

    #[test]
    fn broker_keys_need_the_allowance_and_their_rules() {
        let page = |body: &[u8]| {
            let mut s = Settings::DEFAULT;
            apply(&mut s, body, Allow::PAGE).map(|()| s)
        };
        for body in [b"mqtt_host=x".as_slice(), b"mqtt_port=1883", b"mqtt_user=", b"mqtt_password=", b"tz=0&mqtt_host="] {
            let mut s = Settings::DEFAULT;
            assert_eq!(apply(&mut s, body, Allow::NONE), Err(FormError::Forbidden), "{:?}", core::str::from_utf8(body));
            assert!(apply(&mut s, body, Allow::ALL).is_ok());
        }
        assert_eq!(page(b"ssid=x").err(), Some(FormError::Forbidden), "the page still may not set the network");
        // A new host from the page must come with its password typed:
        // the stored one is for the broker it was typed for.
        let mut with = Settings::DEFAULT;
        apply(&mut with, b"mqtt_host=10.1.1.5&mqtt_user=clock&mqtt_password=secret", Allow::PAGE).unwrap();
        let moved = |body: &[u8], allow| {
            let mut s = with;
            apply(&mut s, body, allow).map(|()| s)
        };
        assert_eq!(moved(b"mqtt_host=attacker.lan&mqtt_password=", Allow::PAGE).err(), Some(FormError::BrokerPassword));
        assert_eq!(moved(b"mqtt_password=&mqtt_host=attacker.lan", Allow::PAGE).err(), Some(FormError::BrokerPassword), "whatever the order");
        assert_eq!(moved(b"mqtt_host=attacker.lan", Allow::PAGE).err(), Some(FormError::BrokerPassword), "or without the field");
        let typed = moved(b"mqtt_host=other.lan&mqtt_password=new", Allow::PAGE).unwrap();
        assert_eq!((typed.mqtt.host.as_str(), typed.mqtt.password.0.as_str()), ("other.lan", "new"));
        let same = moved(b"mqtt_host=10.1.1.5&mqtt_port=1884&mqtt_user=other&mqtt_password=", Allow::PAGE).unwrap();
        assert_eq!((same.mqtt.port, same.mqtt.user.as_str(), same.mqtt.password.0.as_str()), (1884, "other", "secret"), "the same host keeps it");
        let cleared = moved(b"mqtt_host=&mqtt_password=", Allow::PAGE).unwrap();
        assert!(cleared.mqtt.is_none() && cleared.mqtt.password.0.is_empty(), "an empty host clears it");
        let token = moved(b"mqtt_host=other.lan&mqtt_password=", Allow::ALL).unwrap();
        assert_eq!((token.mqtt.host.as_str(), token.mqtt.password.0.as_str()), ("other.lan", "secret"), "the token may move it");
        let mut s = with;
        s.mqtt.password = Secret(Text::EMPTY);
        apply(&mut s, b"mqtt_host=other.lan&mqtt_password=", Allow::PAGE).unwrap();
        assert_eq!(s.mqtt.host.as_str(), "other.lan", "nothing stored, nothing to protect");
        assert_eq!(page(b"mqtt_host=a+b").err(), Some(FormError::BadValue));
        assert_eq!(page(b"mqtt_host=a%3A1883").err(), Some(FormError::BadValue));
        assert_eq!(page(b"mqtt_port=0").err(), Some(FormError::BadValue));
        assert_eq!(page(b"mqtt_port=65536").err(), Some(FormError::BadValue));
        assert_eq!(page(b"mqtt_port=abc").err(), Some(FormError::BadValue));
        assert_eq!(page(b"mqtt_user=a+b").err(), Some(FormError::BadValue));
        assert_eq!(page(b"mqtt_password=%09").err(), Some(FormError::BadValue));
        let long_host = format!("mqtt_host={}", "h".repeat(BROKER_HOST_MAX + 1));
        assert_eq!(page(long_host.as_bytes()).err(), Some(FormError::BadValue));
        let long_user = format!("mqtt_user={}", "u".repeat(BROKER_USER_MAX + 1));
        assert_eq!(page(long_user.as_bytes()).err(), Some(FormError::BadValue));
        let long_password = format!("mqtt_password={}", "p".repeat(BROKER_PASSWORD_MAX + 1));
        assert_eq!(page(long_password.as_bytes()).err(), Some(FormError::BadValue), "VALUE_MAX is the password's length");
        assert_eq!(page(b"mqtt_host=homeassistant.local&mqtt_port=65535").map(|s| s.mqtt.port), Ok(65535));
    }

    #[test]
    fn network_keys_need_the_credentials_flag() {
        let mut s = Settings::DEFAULT;
        s.wifi.ssid = Text::new("home").unwrap();
        // Refused on the key alone, even when the value is what is stored.
        for body in [b"ssid=home".as_slice(), b"ssid_pick=home", b"password=", b"tz=0&password=12345678"] {
            let mut t = s;
            assert_eq!(apply(&mut t, body, Allow::NONE), Err(FormError::Forbidden), "{:?}", core::str::from_utf8(body));
        }
        let mut t = s;
        apply(&mut t, b"ssid_pick=&ssid=Caf%C3%A9+Net&password=p%26ss+word", Allow::ALL).unwrap();
        assert_eq!(t.wifi.ssid.as_str(), "Café Net");
        assert_eq!(t.wifi.password.0.as_str(), "p&ss word");
        // Empty SSID leaves the network alone; the password rule applies.
        let mut u = t;
        apply(&mut u, b"ssid=&ssid_pick=", Allow::ALL).unwrap();
        assert_eq!(u.wifi.ssid, t.wifi.ssid);
        assert_eq!(apply(&mut u, b"password=1234567", Allow::ALL), Err(FormError::BadValue));
        apply(&mut u, b"password=", Allow::ALL).unwrap();
        assert!(u.wifi.password.0.is_empty(), "empty is an open network");
        let long = format!("ssid={}", "s".repeat(SSID_MAX + 1));
        assert_eq!(apply(&mut u, long.as_bytes(), Allow::ALL), Err(FormError::BadValue));
    }

    /// The list's pick wins over the typed name in either order; the
    /// typed name alone, or the pick alone, sets the network.
    #[test]
    fn the_pick_wins_over_the_typed_name() {
        for body in [b"ssid_pick=new&ssid=old&password=12345678".as_slice(), b"ssid=old&ssid_pick=new&password=12345678"] {
            let mut s = Settings::DEFAULT;
            apply(&mut s, body, Allow::ALL).unwrap();
            assert_eq!(s.wifi.ssid.as_str(), "new", "{:?}", core::str::from_utf8(body));
            assert_eq!(s.wifi.password.0.as_str(), "12345678");
        }
        let mut s = Settings::DEFAULT;
        apply(&mut s, b"ssid_pick=&ssid=typed", Allow::ALL).unwrap();
        assert_eq!(s.wifi.ssid.as_str(), "typed");
        apply(&mut s, b"ssid_pick=picked&ssid=", Allow::ALL).unwrap();
        assert_eq!(s.wifi.ssid.as_str(), "picked");
        // A bad pick is still refused, and the key still needs the flag.
        assert_eq!(apply(&mut s, b"ssid_pick=x&ssid=%zz", Allow::ALL), Err(FormError::BadValue));
        assert_eq!(apply(&mut s, b"ssid_pick=x", Allow::NONE), Err(FormError::Forbidden));
    }

    #[test]
    fn settle_password_keeps_or_refuses() {
        let stored = Credentials { ssid: Text::new("home").unwrap(), password: Secret(Text::new("homepass").unwrap()) };
        let networks = [
            Network { ssid: Text::new("home").unwrap(), rssi: -50, open: false },
            Network { ssid: Text::new("cafe").unwrap(), rssi: -60, open: true },
            Network { ssid: Text::new("office").unwrap(), rssi: -70, open: false },
        ];
        let posted = |ssid: &str, password: &str| Settings {
            wifi: Credentials { ssid: Text::new(ssid).unwrap(), password: Secret(Text::new(password).unwrap()) },
            ..Settings::DEFAULT
        };
        // The stored network with no password keeps the stored one, listed or not.
        for networks in [networks.as_slice(), &[]] {
            let mut t = posted("home", "");
            assert_eq!(settle_password(&mut t, &stored, networks), Ok(()));
            assert_eq!(t.wifi, stored);
        }
        // Unless the scan says it is open now.
        let open_home = [Network { ssid: Text::new("home").unwrap(), rssi: -50, open: true }];
        let mut t = posted("home", "");
        assert_eq!(settle_password(&mut t, &stored, &open_home), Ok(()));
        assert!(t.wifi.password.0.is_empty());
        // A typed password is always taken.
        let mut t = posted("home", "newpass12");
        assert_eq!(settle_password(&mut t, &stored, &networks), Ok(()));
        assert_eq!(t.wifi.password.0.as_str(), "newpass12");
        // Another listed network: open takes an empty password, secured refuses it.
        let mut t = posted("cafe", "");
        assert_eq!(settle_password(&mut t, &stored, &networks), Ok(()));
        assert!(t.wifi.password.0.is_empty());
        let mut t = posted("office", "");
        assert_eq!(settle_password(&mut t, &stored, &networks), Err(FormError::BadValue));
        let mut t = posted("office", "officepw");
        assert_eq!(settle_password(&mut t, &stored, &networks), Ok(()));
        // An unlisted network is taken on trust.
        let mut t = posted("hidden", "");
        assert_eq!(settle_password(&mut t, &stored, &networks), Ok(()));
        assert!(t.wifi.password.0.is_empty());
        // With nothing stored, the stored-network branch is moot.
        let mut t = posted("hidden", "");
        assert_eq!(settle_password(&mut t, &Credentials::NONE, &networks), Ok(()));
        assert!(t.wifi.password.0.is_empty());
    }

    #[test]
    fn token_body() {
        assert_eq!(parse_token(b"token=abcdefgh").map(|t| t.0.as_str().to_string()), Ok("abcdefgh".to_string()));
        assert!(parse_token(b"token=").unwrap().0.is_empty(), "empty clears");
        assert_eq!(parse_token(b"token=short"), Err(FormError::BadValue));
        assert_eq!(parse_token(b"token=has%20space1"), Err(FormError::BadValue));
        assert_eq!(parse_token(b"color=red"), Err(FormError::UnknownKey));
        assert_eq!(parse_token(b""), Err(FormError::BadValue));
    }

    #[test]
    fn json_hides_the_secrets_and_escapes_the_ssid() {
        let mut s = Settings::DEFAULT;
        s.wifi.ssid = Text::new("my \"net\"\\\x01").unwrap();
        s.wifi.password = Secret(Text::new("secret-pw").unwrap());
        s.token = Secret(Text::new("secret-token").unwrap());
        let mut out = String::new();
        write_json(&mut out, &s, &status(true)).unwrap();
        assert!(out.contains(r#""ssid":"my \"net\"\\\u0001""#), "{out}");
        assert!(out.contains(r#""password_set":true,"token_set":true"#));
        assert!(!out.contains("secret"));
        assert!(out.contains(r#""mqtt_host":"","mqtt_port":1883,"mqtt_user":"","mqtt_password_set":false,"mqtt":"connecting","ringing":true,"version""#), "{out}");
        s.mqtt.host = Text::new("broker.lan").unwrap();
        s.mqtt.user = Text::new("cl\"ock").unwrap();
        s.mqtt.password = Secret(Text::new("secret-mqtt").unwrap());
        let mut with = String::new();
        write_json(&mut with, &s, &status(true)).unwrap();
        assert!(with.contains(r#""mqtt_host":"broker.lan","mqtt_port":1883,"mqtt_user":"cl\"ock","mqtt_password_set":true"#), "{with}");
        assert!(!with.contains("secret"));
        assert!(out.contains(r#""temp_c":-20.0,"temp_f":-4.0,"light":100,"rssi":-100,"utc_offset":-480,"dst":false,"last_heal""#), "{out}");
        s.tz = Text::new("<+0545>-5:45").unwrap();
        let mut rule = String::new();
        write_json(&mut rule, &s, &status(true)).unwrap();
        assert!(rule.contains(r#""twelve_hour":false,"tz":"<+0545>-5:45","temp_offset""#), "{rule}");
        assert!(out.contains(r#""temp_offset":-2.5,"temp_show":"c","hostname""#), "{out}");
        assert!(out.contains(&format!(r#","tx_power":{},"output":"buzzer","ssid""#, TX_POWER_DEFAULT)), "{out}");
        let mut low = String::new();
        write_json(&mut low, &Settings { tx_power_qdbm: 34, ..s }, &status(true)).unwrap();
        assert!(low.contains(r#","tx_power":8.5,"output":"buzzer","ssid""#), "{low}");
        assert!(out.contains(r#""last_heal":1800000000}"#));
        assert!(out.ends_with("}\n"));
        let mut none = String::new();
        let mut unread = status(false);
        (unread.temp_dc, unread.light, unread.rssi, unread.local) = (None, None, None, None);
        write_json(&mut none, &Settings::DEFAULT, &unread).unwrap();
        assert!(none.contains(r#""temp_c":null,"temp_f":null,"light":null,"rssi":null,"utc_offset":null,"dst":null"#), "{none}");
        assert!(none.contains(r#""password_set":false,"token_set":false"#));
        assert!(none.ends_with(r#""last_heal":null}"#.to_string().as_str() ) || none.ends_with("null}\n"));
    }

    #[test]
    fn escaping() {
        assert_eq!(format!("{}", HtmlStr("a<b>&\"c\"")), "a&lt;b&gt;&amp;&quot;c&quot;");
        assert_eq!(format!("{}", JsonStr("a\"b\\c\n")), "a\\\"b\\\\c\\u000a");
    }

    /// Settings with every string at its limit and every box ticked:
    /// what each page is measured with.
    fn worst_settings() -> Settings {
        let mut s = Settings::DEFAULT;
        s.hostname = Hostname::new(&"h".repeat(HOSTNAME_MAX)).unwrap();
        s.alarm_days = 0b1010101;
        s.alarm_armed = true;
        s.skip_next = true;
        s.twelve_hour = true;
        s.tz = Text::new(&"<".repeat(TZ_MAX)).unwrap(); // escaped into the field, four bytes each
        s.melody = melody::RANDOM;
        s.wifi.ssid = Text::new(&"\"".repeat(SSID_MAX)).unwrap(); // escaped into the Wi-Fi line
        s.mqtt.host = Text::new(&"h".repeat(BROKER_HOST_MAX)).unwrap(); // the field and the status line
        s.mqtt.port = 65535;
        s.mqtt.user = Text::new(&"\"".repeat(BROKER_USER_MAX)).unwrap(); // escaped into the field
        s
    }

    /// The JSON at its longest, for `JSON_MAX`: every string full and
    /// of the bytes that escape longest (a control character is six),
    /// whether or not a rule lets them into that field, the longest
    /// name of every choice, a full pack's longest clip name, and a
    /// status of extremes.
    #[test]
    fn the_json_fits_the_state_buffer() {
        melody::install(Some(crate::pack::fullest()));
        let mut st = status(true);
        st.slot = "factory";
        st.uptime_secs = u64::MAX;
        st.heals = u32::MAX;
        st.watchdog = u32::MAX;
        st.clips_paused = false;
        st.temp_dc = Some(i16::MIN);
        st.rssi = Some(i8::MIN);
        st.ringing = false;
        st.last_heal = Some(HealStamp { unix_secs: i64::MIN, weekday: 2, hour: 23, minute: 59, ago_secs: None });
        st.local = Some(LocalNow { weekday: 4, hour: 14, minute: 32, offset_secs: i32::MIN, dst: false });
        let len = |s: &Settings, st: &Status| {
            let mut out = String::new();
            write_json(&mut out, s, st).unwrap();
            out.len()
        };
        let mut s = worst_settings();
        let control = |n: usize| "\u{1}".repeat(n);
        s.tz = Text::new(&control(TZ_MAX)).unwrap();
        s.wifi.ssid = Text::new(&control(SSID_MAX)).unwrap();
        s.mqtt.user = Text::new(&control(BROKER_USER_MAX)).unwrap();
        s.alarm_armed = false;
        s.skip_next = false;
        s.twelve_hour = false;
        s.temp_offset_dc = i16::MIN;
        // Each choice at its longest name, one field at a time.
        let mut pick = |set: &dyn Fn(&mut Settings, usize), n: usize| {
            let best = (0..n).max_by_key(|i| { let mut t = s; set(&mut t, *i); len(&t, &st) }).unwrap();
            set(&mut s, best);
        };
        pick(&|s, i| s.color = i, PALETTE_NAMES.len());
        pick(&|s, i| s.alarm_days = i as u8, 128);
        pick(&|s, i| s.melody = i, melody::count());
        pick(&|s, i| s.temp_show = [TempShow::Off, TempShow::Celsius, TempShow::Fahrenheit][i], 3);
        for link in [Link::Off, Link::Connected, Link::Connecting { last_error: None }] {
            let mut other = st;
            other.mqtt = link;
            assert!(len(&s, &other) <= len(&s, &st));
        }
        let n = len(&s, &st);
        melody::install(None);
        println!("the JSON at its longest: {n} bytes of {JSON_MAX}");
        assert!(n <= JSON_MAX, "{n} bytes");
    }

    /// The five pages as served, in `Page::ALL`'s order.
    fn pages(s: &Settings, st: &Status, note: Note) -> [String; 5] {
        let mut out = [String::new(), String::new(), String::new(), String::new(), String::new()];
        write_page(&mut out[0], s, st, note).unwrap();
        write_settings_page(&mut out[1], s, st, note).unwrap();
        write_ha_page(&mut out[2], s, st, note).unwrap();
        write_clips_page(&mut out[3], s.hostname.as_str(), st).unwrap();
        write_system_page(&mut out[4], s.hostname.as_str(), st, true).unwrap();
        out
    }

    /// The notice that takes the most of a page.
    fn longest_note() -> Note {
        let mut notes = vec![Note::Saved];
        for e in [FormError::BadValue, FormError::UnknownKey, FormError::Forbidden, FormError::BrokerPassword] {
            notes.push(Note::Refused(e, None));
            notes.extend(FIELDS.iter().map(|(k, _)| Note::Refused(e, Some(*k))));
        }
        let len = |n: &Note| {
            let mut out = String::new();
            write_note(&mut out, *n).unwrap();
            out.len()
        };
        *notes.iter().max_by_key(|n| len(n)).unwrap()
    }

    /// The names of a page's form controls, in order, repeats dropped.
    fn field_names(page: &str) -> Vec<&str> {
        let body = &page[page.find("</head>").unwrap()..];
        let mut names: Vec<&str> = Vec::new();
        for part in body.split(" name=").skip(1) {
            let name = &part[..part.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap()];
            if !names.contains(&name) {
                names.push(name);
            }
        }
        names
    }

    /// A value as a browser puts it in a form's body
    /// (`application/x-www-form-urlencoded`): letters, digits and
    /// `*-._` as they are, a space as `+`, any other byte as `%XX`.
    fn urlencoded(value: &str) -> String {
        let mut out = String::new();
        for b in value.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => out.push(b as char),
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    /// A form's body from its pairs, in the order given.
    fn form_body(pairs: &[(&str, String)]) -> String {
        pairs.iter().map(|(k, v)| format!("{k}={}", urlencoded(v))).collect::<Vec<_>>().join("&")
    }

    /// The names of `pairs`, each once: what `field_names` gives for
    /// the page, so a field added to a page fails here until it is
    /// counted.
    fn names_of<'a>(pairs: &[(&'a str, String)]) -> Vec<&'a str> {
        let mut names: Vec<&str> = Vec::new();
        for (k, _) in pairs {
            if !names.contains(k) {
                names.push(k);
            }
        }
        names
    }

    /// The body of each of the four forms at its longest against
    /// `http::BODY_MAX`, over which the firmware closes the socket
    /// with no answer. Every field holds the longest value its
    /// `maxlength` and the setting's rule allow, in bytes that are all
    /// escaped where the rule lets them be: a quote for the network's
    /// names and the passwords, `<` for the timezone (not a rule that
    /// parses, but a refused form is read too, to be answered). Every
    /// box is ticked, so the hidden `0` and the `on` are both sent.
    #[test]
    fn forms_fit_the_request_body() {
        use crate::http::BODY_MAX;
        let q = |n: usize| "\"".repeat(n);
        let longest = |names: &mut dyn Iterator<Item = &str>| names.max_by_key(|n| urlencoded(n).len()).unwrap().to_string();
        let tz = "<".repeat(TZ_MAX);
        let hostname = "h".repeat(HOSTNAME_MAX);
        let tx_power = longest(&mut TX_POWER_CHOICES.iter().map(|(n, _)| *n));
        let on = || "on".to_string();
        let zero = || "0".to_string();

        let setup = [
            ("ssid_pick", q(SSID_MAX)),
            ("ssid", q(SSID_MAX)),
            ("password", q(PASSWORD_MAX)),
            ("tz", tz.clone()),
            ("hostname", hostname.clone()),
            ("tx_power", tx_power.clone()),
        ];
        let mut clock = vec![("armed", zero()), ("armed", on()), ("alarm", "23:59".to_string())];
        clock.extend(DAY_NAMES.iter().map(|d| ("days", d.to_string())));
        clock.extend([
            ("skip", zero()),
            ("skip", on()),
            ("melody", "m".repeat(crate::pack::NAME_MAX)),
            ("color", longest(&mut PALETTE_NAMES.iter().copied())),
            ("brightness", "100".to_string()),
        ]);
        let settings = [
            ("tz", tz),
            ("twelve_hour", zero()),
            ("twelve_hour", on()),
            ("temp_show", longest(&mut TempShow::NAMES.iter().copied())),
            ("temp_offset", format!("-{}.{}", TEMP_OFFSET_LIMIT_DC / 10, TEMP_OFFSET_LIMIT_DC % 10)),
            ("hostname", hostname),
            ("tx_power", tx_power),
            ("output", longest(&mut AudioOut::NAMES.iter().copied())),
        ];
        let ha = [
            ("mqtt_host", "h".repeat(BROKER_HOST_MAX)),
            ("mqtt_port", "65535".to_string()),
            ("mqtt_user", q(BROKER_USER_MAX)),
            ("mqtt_password", q(BROKER_PASSWORD_MAX)),
        ];

        // Each list is the page's own fields.
        let out = pages(&Settings::DEFAULT, &status(true), Note::None);
        let mut provision = String::new();
        write_provision_page(&mut provision, &Settings::DEFAULT, &status(false), &[], Note::None).unwrap();
        assert_eq!(names_of(&setup), field_names(&provision));
        assert_eq!(names_of(&clock), field_names(&out[0]));
        assert_eq!(names_of(&settings), field_names(&out[1]));
        assert_eq!(names_of(&ha), field_names(&out[2]));

        let sizes = [
            ("setup", form_body(&setup).len()),
            ("clock", form_body(&clock).len()),
            ("settings", form_body(&settings).len()),
            ("ha", form_body(&ha).len()),
        ];
        println!("form bodies at their worst, of {BODY_MAX}: {sizes:?}");
        for (form, size) in sizes {
            assert!(size <= BODY_MAX, "the {form} form's body can be {size} bytes, over {BODY_MAX}");
        }
    }

    /// Every page at its longest: every string at its limit, the notice,
    /// a custom day list, every checkbox, the longest status.
    #[test]
    fn pages_fit_the_response_buffer() {
        let out = pages(&worst_settings(), &status(true), longest_note());
        for (page, out) in Page::ALL.iter().zip(&out) {
            println!("{page:?} page: {} bytes of {PAGE_MAX}", out.len());
            assert!(out.len() <= PAGE_MAX, "{page:?}: {} bytes", out.len());
            assert!(out.ends_with("</html>"), "{page:?}");
        }
        // And with the fullest pack there can be in the table: sixteen
        // clips, every name and label at its limit.
        melody::install(Some(crate::pack::fullest()));
        let full = pages(&worst_settings(), &status(true), longest_note());
        for (page, full) in Page::ALL.iter().zip(&full) {
            println!("{page:?} page with a full pack: {} bytes of {PAGE_MAX}", full.len());
            assert!(full.len() <= PAGE_MAX, "{page:?}: {} bytes", full.len());
        }
        assert_eq!(full[0].matches("<option value=").count() - out[0].matches("<option value=").count() + melody::CLIP_COUNT, crate::pack::MAX_CLIPS);
        melody::install(None);
        // Play and Stop, each posted by the script to its own address
        // without leaving the page.
        // Side by side as one control, each half the row, a gap between.
        assert!(out[0].contains("<p style=display:flex;gap:1em><button formaction=/play id=p style=flex:1>Play</button><button formaction=/stop id=q style=flex:1>Stop</button></p>"));
        assert!(out[0].contains("for(let i of'pq')") && out[0].contains("P(b.formAction,"));
        // With script they are plain buttons, so Enter in the form is Save.
        assert!(out[0].contains("b.type='button'"));
        assert!(out[0].contains(&format!("Wi-Fi <b>{}</b>, -100 dBm.", "&quot;".repeat(SSID_MAX))), "{}", out[0]);
    }

    /// A refused form names its field: `apply_naming` answers the key,
    /// the page writes that key's label, and a key that is not a
    /// page's own is not written at all.
    #[test]
    fn a_refused_form_names_its_field() {
        let mut s = Settings::DEFAULT;
        assert_eq!(apply_naming(&mut s, b"armed=on&tz=nonsense%2C", Allow::PAGE), Err((FormError::BadValue, Some("tz"))));
        assert_eq!(apply_naming(&mut s, b"hostname=a_b", Allow::PAGE), Err((FormError::BadValue, Some("hostname"))));
        assert_eq!(apply_naming(&mut s, b"tz=%zz", Allow::PAGE), Err((FormError::BadValue, Some("tz"))), "a value that does not decode");
        assert_eq!(apply_naming(&mut s, b"nonsense=1", Allow::PAGE), Err((FormError::UnknownKey, None)));
        assert_eq!(apply_naming(&mut s, b"ssid=x", Allow::PAGE), Err((FormError::Forbidden, Some("ssid"))));
        assert_eq!(apply_naming(&mut s, b"tz=0", Allow::PAGE), Ok(()));
        // Every key of the table is one `apply_one` knows.
        for (key, _) in FIELDS {
            assert_ne!(apply_one(&mut s, key, "", Allow::ALL), Err(FormError::UnknownKey), "{key}");
            assert_eq!(field(key), Some(key));
        }
        assert_eq!(field("<script>"), None);
        let page = |note| {
            let mut out = String::new();
            write_settings_page(&mut out, &Settings::DEFAULT, &status(true), note).unwrap();
            out
        };
        assert!(page(Note::Refused(FormError::BadValue, Some("tz"))).contains("<p class=err>Nothing was saved: bad value for <b>Timezone</b>.</p>"));
        assert!(page(Note::Refused(FormError::BadValue, None)).contains("<p class=err>Nothing was saved: a field had a bad value.</p>"));
        assert!(page(Note::Refused(FormError::BadValue, Some("no such key"))).contains("a field had a bad value"));
        assert!(page(Note::Saved).contains("</nav><p class=c><b>Saved.</b></p><form"));
        assert!(!page(Note::None).contains("Saved") && !page(Note::None).contains("class=err>"));
        // The setup form names a field too, and keeps its own words for
        // what has none.
        let setup = |note| {
            let mut out = String::new();
            write_provision_page(&mut out, &Settings::DEFAULT, &status(false), &[], note).unwrap();
            out
        };
        assert!(setup(Note::Refused(FormError::BadValue, Some("password"))).contains("bad value for <b>Password</b>"));
        assert!(setup(Note::Refused(FormError::BadValue, None)).contains("no network was named, or a secured network was given no password"));
        // Its password can be shown: a checkbox with no name, so it is
        // not posted, hidden until the script that makes it work.
        let out = setup(Note::None);
        assert!(out.contains("<label id=l hidden><input type=checkbox id=v> Show the password</label>"));
        assert!(out.contains("$('l').hidden=false;v.onchange=function(){k.type=v.checked?'text':'password'}"));
        assert_eq!(field_names(&out), ["ssid_pick", "ssid", "password", "tz", "hostname", "tx_power"]);
    }

    /// The alarm's days: seven checkboxes under one name, read back as
    /// a union, with `/api`'s lists and words as they were.
    #[test]
    fn days_are_seven_checkboxes() {
        let mut s = Settings::DEFAULT;
        s.alarm_days = 0b0010101;
        let mut out = String::new();
        write_page(&mut out, &s, &status(true), Note::None).unwrap();
        assert_eq!(out.matches("<input type=checkbox name=days value=").count(), 7);
        assert!(out.contains("<div class=d>Alarm days<select id=y hidden><option value=127>Every day</option><option value=31>Weekdays</option><option value=96>Weekend</option><option value=\"\" selected>Custom</option></select><br>\
<label><input type=checkbox name=days value=mon checked> Mon</label><label><input type=checkbox name=days value=tue> Tue</label>"), "{out}");
        // The presets are not a field: only the boxes are posted.
        assert!(out.contains("y.oninput=function(){if(y.value)C.forEach(function(c,i){c.checked=y.value>>i&1})}"));
        s.alarm_days = DAYS_WEEKDAYS;
        let mut preset = String::new();
        write_page(&mut preset, &s, &status(true), Note::None).unwrap();
        assert!(preset.contains("<option value=31 selected>Weekdays</option>") && !preset.contains("\" selected>Custom"));
        assert_eq!((DAYS_ALL, DAYS_WEEKDAYS, DAYS_WEEKEND), (127, 31, 96));
        s.alarm_days = 0b0010101;
        assert_eq!(out.split("name=days").skip(1).filter(|p| p.split('>').next().unwrap().ends_with(" checked")).count(), 3);
        // What a browser posts for Tuesday and Thursday ticked.
        apply(&mut s, b"armed=0&alarm=07%3A00&days=tue&days=thu&skip=0", Allow::PAGE).unwrap();
        assert_eq!(s.alarm_days, 0b0001010);
        // The first of a body replaces, whatever was stored.
        apply(&mut s, b"days=sun", Allow::PAGE).unwrap();
        assert_eq!(s.alarm_days, 0b1000000);
        apply(&mut s, b"days=mon,wed&days=weekend", Allow::NONE).unwrap();
        assert_eq!(s.alarm_days, 0b1100101);
        apply(&mut s, b"days=weekdays", Allow::NONE).unwrap();
        assert_eq!(s.alarm_days, DAYS_WEEKDAYS);
        // None ticked sends no key: nothing changes.
        apply(&mut s, b"armed=0&alarm=07%3A00&skip=0", Allow::PAGE).unwrap();
        assert_eq!(s.alarm_days, DAYS_WEEKDAYS);
        assert_eq!(apply_naming(&mut s, b"days=mon&days=funday", Allow::PAGE), Err((FormError::BadValue, Some("days"))));
    }

    /// When the alarm rings next, and what the page says of it.
    #[test]
    fn the_page_says_when_the_alarm_rings_next() {
        let now = |weekday, hour, minute| LocalNow { weekday, hour, minute, offset_secs: 0, dst: false };
        let mut s = Settings::DEFAULT;
        (s.alarm_hour, s.alarm_minute, s.alarm_days) = (7, 30, DAYS_WEEKDAYS);
        assert_eq!(next_alarm(&s, &now(0, 6, 0)), None, "disarmed");
        s.alarm_armed = true;
        assert_eq!(next_alarm(&s, &now(0, 7, 29)), Some((0, None)), "later today");
        assert_eq!(next_alarm(&s, &now(0, 7, 30)), Some((1, None)), "the minute it rings in is past");
        assert_eq!(next_alarm(&s, &now(4, 8, 0)), Some((0, None)), "Friday after it: Monday");
        assert_eq!(next_alarm(&s, &now(5, 6, 0)), Some((0, None)), "Saturday: Monday");
        s.skip_next = true;
        assert_eq!(next_alarm(&s, &now(4, 6, 0)), Some((0, Some(4))), "Friday's skipped");
        s.alarm_days = 0b0000100;
        assert_eq!(next_alarm(&s, &now(2, 8, 0)), Some((2, Some(2))), "one day a week, after it: the week after next");
        assert_eq!(next_alarm(&s, &now(2, 6, 0)), Some((2, Some(2))));
        s.skip_next = false;
        assert_eq!(next_alarm(&s, &now(2, 8, 0)), Some((2, None)));
        s.alarm_days = 0;
        assert_eq!(next_alarm(&s, &now(2, 8, 0)), None, "no day");
        // On the page, under the skip box: `status` is Friday 14:32.
        let page = |s: &Settings, st: &Status| {
            let mut out = String::new();
            write_page(&mut out, s, st, Note::None).unwrap();
            out
        };
        let st = status(true);
        let l = st.local.unwrap();
        s.alarm_days = DAYS_ALL;
        let today = (l.hour, l.minute) < (7, 30);
        let day = WEEKDAY_LABELS[(l.weekday as usize + usize::from(!today)) % 7];
        assert!(page(&s, &st).contains(&format!(" Skip the next alarm</label><p><small>Next alarm: {day} 07:30.</small></p>")), "{}", page(&s, &st));
        s.skip_next = true;
        assert!(page(&s, &st).contains(&format!("({day} skipped).</small></p>")));
        s.alarm_armed = false;
        assert!(!page(&s, &st).contains("Next alarm"));
        s.alarm_armed = true;
        let mut unsynced = st;
        unsynced.local = None;
        assert!(!page(&s, &unsynced).contains("Next alarm"));
    }

    /// The clock page says why the alarm beeps while the clips are not
    /// played, armed or not, and nothing of it otherwise; the JSON has
    /// the flag either way.
    #[test]
    fn the_page_says_when_the_clips_are_paused() {
        let line = "<p><small>Clips paused: one hung the player. Upload a pack or update the firmware to try again.</small></p>";
        let page = |s: &Settings, st: &Status| pages(s, st, Note::None)[0].clone();
        let mut s = Settings { alarm_armed: true, ..Settings::DEFAULT };
        let mut st = status(true);
        st.clips_paused = true;
        let out = page(&s, &st);
        // Under the next alarm, above the sound it explains.
        assert!(out.contains(&format!(".</small></p>{line}<label>Alarm sound")), "{out}");
        s.alarm_armed = false;
        assert!(page(&s, &st).contains(&format!(" Skip the next alarm</label>{line}<label>Alarm sound")), "shown with no alarm armed");
        let mut json = String::new();
        write_json(&mut json, &s, &st).unwrap();
        assert!(json.contains(r#""watchdog":16777215,"clips_paused":true,"temp_c""#), "{json}");

        st.clips_paused = false;
        assert!(!page(&s, &st).contains("Clips paused"));
        for out in pages(&s, &st, Note::None) {
            assert!(!out.contains("Clips paused"));
        }
        let mut json = String::new();
        write_json(&mut json, &s, &st).unwrap();
        assert!(json.contains(r#""clips_paused":false"#));
    }

    /// A form is posted by its script, which keeps what was typed when
    /// it is refused; the broker page follows the connection; Rescan
    /// keeps the setup form.
    #[test]
    fn scripts_keep_what_was_typed() {
        let out = pages(&Settings::DEFAULT, &status(true), Note::None);
        for out in &out[..3] {
            // Only the names that were changed are posted, each with
            // all its pairs.
            assert!(out.contains("f.onsubmit=function(e){e.preventDefault();E(1);var s=G(),b=new URLSearchParams;new FormData(f).forEach(function(v,k){if(s.has(k))b.append(k,v)});fetch(f.action,{method:'POST',body:b})"), "{out}");
            assert!(out.contains("X='p.err:not([id])'") && out.contains("if(!n){location.replace(r.url);return}") && out.contains(".catch(function(){E(0)})"));
            // A post with no answer says so above Save and is never
            // followed by the browser's own submit, which would post
            // the whole form, a stale tab's colour with it.
            assert!(!out.contains("f.submit()"));
            assert!(out.contains("<p class=err id=e role=alert hidden>The clock did not answer; try again.</p><p class=c><button>Save</button>"), "{out}");
        }
        // The broker page: whether a password is stored, and the poll
        // with the state the page was served in.
        assert!(out[2].contains("<label>Password (none is stored) <input type=password name=mqtt_password"));
        assert!(out[2].contains(&format!("var M='{}',c=24,", status(true).mqtt.name())) && out[2].contains("if(j.mqtt!=M&&u.hidden)location.replace('/ha')"));
        let mut s = Settings::DEFAULT;
        apply(&mut s, b"mqtt_host=10.1.1.5&mqtt_password=secret", Allow::PAGE).unwrap();
        let with = pages(&s, &status(true), Note::None);
        assert!(with[2].contains("<label>Password (one is stored; empty keeps it for the same host) <input"));
        // The token is typed in the open: it is copied into scripts.
        assert!(out[4].contains("<input type=text maxlength=32 autocomplete=off id=k>") && out[4].contains("<p id=s role=status></p>"));
        let mut setup = String::new();
        write_provision_page(&mut setup, &Settings::DEFAULT, &status(false), &[], Note::None).unwrap();
        assert!(setup.contains("<p class=c><button>Save and join</button> <button formaction=/scan formnovalidate id=r>Rescan</button></p></form>"));
        assert!(setup.contains("fetch('/scan',{method:'POST'})") && setup.contains("getElementById('n').innerHTML;for(var o of n.options)if(o.value==p)n.value=p;f()"));
        assert!(setup.ends_with("</small></p></div></body></html>") && !setup.contains("Scriptable"));
        // Every page follows the browser's light or dark.
        for out in out.iter().chain([&setup]) {
            assert!(out.contains(":root{color-scheme:light dark}") && out.contains("a{color:LinkText}") && out.contains("h1{font-size:1.6em;text-align:center}"));
            // The links, Save and the status under the form are centred.
            assert!(out.contains("nav{margin:1em 0;text-align:center}.c,p.err:not(#s){text-align:center}") && out.contains(".st{text-align:center}"));
        }
        for out in out[..3].iter().chain([&setup]) {
            assert!(out.contains("<p class=c><button>Save") && out.contains("<div class=st><p><small>") && out.ends_with("</small></p></div></body></html>"), "{out}");
        }
    }

    /// Each form carries its own keys and nothing of another page's,
    /// which is what makes its post a partial update; between them the
    /// forms have every key the one page had.
    #[test]
    fn each_page_has_its_own_fields() {
        let out = pages(&Settings::DEFAULT, &status(true), Note::None);
        assert_eq!(field_names(&out[0]), ["armed", "alarm", "days", "skip", "melody", "color", "brightness"]);
        assert_eq!(field_names(&out[1]), ["tz", "twelve_hour", "temp_show", "temp_offset", "hostname", "tx_power", "output"]);
        assert_eq!(field_names(&out[2]), ["mqtt_host", "mqtt_port", "mqtt_user", "mqtt_password"]);
        assert!(field_names(&out[3]).is_empty() && field_names(&out[4]).is_empty(), "the clips and system pages post by script");
        // One form with Save under both fieldsets; the display's
        // selects keep their names, so Save posts them with the alarm
        // and nothing is applied before it.
        assert_eq!(out[0].matches("<form").count(), 1);
        assert!(out[0].contains("</select></label></fieldset><p class=err id=e role=alert hidden>The clock did not answer; try again.</p><p class=c><button>Save</button>"), "{}", out[0]);
        assert!(out[0].ends_with("</small></p></div></body></html>") && !out[0].contains("Scriptable"), "the page ends with the status lines");
        assert!(out[0].contains("<option value=orange selected>Orange</option>"));
        assert!(out[0].contains("<fieldset><legend>Display</legend><label>Colour <select name=color>"));
        assert!(!out[0].contains("c.name=''") && !out[0].contains("P('/api'"), "the display waits for Save");
        // Play or Stop that failed says so.
        assert!(out[0].contains("if(!r.ok&&r.type!='opaqueredirect')throw 0"));
        // Each form posts to its own page, where a refusal is shown.
        assert!(out[1].contains("<form method=post action=/settings>") && out[2].contains("<form method=post action=/ha>"));
        // What each page shows of the status.
        assert!(out[1].contains("Local time Fri 14:32") && out[2].contains("MQTT off.") && out[0].contains("Self-heal resets"));
    }

    /// A box unticked is the hidden `0`, a ticked one the `on` after it,
    /// and a form without the box leaves the setting alone.
    #[test]
    fn checkboxes_are_posted_either_way() {
        let mut s = Settings::DEFAULT;
        s.alarm_armed = true;
        let mut out = String::new();
        write_page(&mut out, &s, &status(true), Note::None).unwrap();
        assert!(out.contains("<input type=hidden name=armed value=0><input type=checkbox name=armed checked> Alarm armed"), "{out}");
        assert!(out.contains("<input type=hidden name=skip value=0><input type=checkbox name=skip> Skip the next alarm"), "{out}");
        s.twelve_hour = true;
        apply(&mut s, b"armed=0&armed=on&alarm=07%3A00&skip=0", Allow::PAGE).unwrap();
        assert!(s.alarm_armed && !s.skip_next && s.twelve_hour, "the alarm form leaves 12 hour alone");
        apply(&mut s, b"armed=0&skip=0&skip=on", Allow::PAGE).unwrap();
        assert!(!s.alarm_armed && s.skip_next && s.twelve_hour);
        apply(&mut s, b"tz=0&twelve_hour=0", Allow::PAGE).unwrap();
        assert!(!s.twelve_hour && s.skip_next, "and the settings form leaves the alarm's alone");
    }

    /// Every page links to the other three and names itself in bold;
    /// on the access point the system page links to the setup form only.
    #[test]
    fn pages_link_to_each_other() {
        let out = pages(&Settings::DEFAULT, &status(true), Note::None);
        for (here, out) in Page::ALL.iter().zip(&out) {
            for page in Page::ALL {
                let link = format!("<a href={}>{}</a>", page.path(), page.label());
                assert_eq!(out.contains(&link), page != *here, "{here:?} to {page:?}");
            }
            assert!(out.contains(&format!("<b>{}</b>", here.label())), "{here:?}");
            let host = Settings::DEFAULT.hostname.as_str();
            // The heading is the clock's; the title also says which page.
            assert!(out.contains(&format!("<title>{host}: {} - onebitdawn</title>", here.title())) && out.contains(&format!("<h1>onebitdawn: {host}</h1>")), "{out}");
        }
        let mut ap = status(false);
        ap.mode = "ap";
        let mut out = String::new();
        write_system_page(&mut out, "clock", &ap, false).unwrap();
        assert!(out.contains("<nav><a href=/>Setup</a> · <a href=/clips>Clips</a> · <b>System</b></nav>"), "{out}");
        assert!(!out.contains("id=a>") && out.contains("id=r>Reboot</button></p>"), "no way to the access point from it");
        let mut setup = String::new();
        write_provision_page(&mut setup, &Settings::DEFAULT, &ap, &[], Note::None).unwrap();
        assert!(setup.contains("<a href=/system>Update</a>"));
    }

    #[test]
    fn pages_load_the_zone_script_with_the_version() {
        let tag = "<script src=\"/tz.js?v=v0.1.0-123-gabcdef0-dirty-and-long\"></script>";
        let mut out = String::new();
        write_settings_page(&mut out, &Settings::DEFAULT, &status(true), Note::None).unwrap();
        assert!(out.contains(tag) && out.contains("<input type=text name=tz id=z maxlength=48 value=\""), "{out}");
        assert!(out.find("</form>").unwrap() < out.find(tag).unwrap(), "the script comes after the form");
        let mut setup = String::new();
        write_provision_page(&mut setup, &Settings::DEFAULT, &status(false), &[], Note::None).unwrap();
        assert!(setup.contains(tag) && setup.contains("id=b>Use this browser's zone</button>"), "{setup}");
        assert!(setup.find("</form>").unwrap() < setup.find(tag).unwrap());
    }

    #[test]
    fn pages_mark_unsaved_changes() {
        // The mark is hidden as served and the script compares every
        // named control of the first form with the defaults the page
        // came with.
        let out = pages(&Settings::DEFAULT, &status(true), Note::None);
        for out in &out[..3] {
            assert!(out.contains("<p class=c><button>Save</button><br><b id=u hidden>Changes not saved yet.</b></p>"), "{out}");
            assert_eq!(out.matches(" id=e ").count(), 1);
            assert!(out.contains("history.replaceState(0,'',location.pathname)"), "the notice's query leaves the address");
            assert_eq!(out.matches("<script>").count(), 1);
            assert!(out.contains("f=D.forms[0]") && out.contains("f.oninput=function(){u.hidden=!G().size}"), "{out}");
            for check in ["e.checked!=e.defaultChecked", "e.options[e.selectedIndex].defaultSelected", "e.value!=e.defaultValue", "s.add(e.name)"] {
                assert!(out.contains(check), "{check}");
            }
            // Every select marks its stored option, which the comparison needs.
            assert_eq!(out.matches("<select").count(), out.matches(" selected").count());
        }
    }

    /// The system page: the four actions, each to its route with the PIN
    /// in the header, and one PIN form, asked for at the click.
    #[test]
    fn system_page_has_the_four_actions() {
        let mut out = String::new();
        write_system_page(&mut out, "clock", &status(true), true).unwrap();
        assert_eq!(out.matches("id=t>").count(), 1, "one PIN field");
        for route in ["up('/ota');", "go(u,pin,back)", "go('/token',pin,", "go('/reload',pin,", "go('/ap',pin,"] {
            assert!(out.contains(route), "{route}");
        }
        assert!(out.contains("x.setRequestHeader('Authorization','PIN '+pin)"));
        assert!(out.contains("from the <a href=https://github.com/anarsoul/onebitdawn/releases target=_blank rel=noopener>releases</a>)<input type=file id=f>"));
        assert!(out.contains("<p><small>Uploading asks for a PIN, which the clock then shows on its panel. The panel goes dark while the image is written. Do not unplug.</small></p>"));
        assert!(out.contains("(one is set)") && out.contains("<p>Each action here asks for a PIN, which the clock then shows on its panel.</p>"));
        // The PIN's form is hidden, after the last fieldset (so its
        // buttons are not among the actions'), and nothing tells the
        // visitor to reload: every action asks for its own PIN.
        assert!(out.contains("</fieldset><div id=w><form id=q hidden>") && out.contains("pattern=[0-9]{4} maxlength=4 required"));
        assert_eq!(out.matches("fetch('/pin',{method:'POST'})").count(), 1);
        assert!(out.contains("function ask(b,run){at(b);dis(1);say('');fetch('/pin'"), "the PIN is asked for by a click");
        assert!(!out.contains("Reload this page"));
        // A token is held to its rule before a PIN is drawn for it.
        assert!(out.contains("/^[!-~]{8,32}$/.test(v)"));
        for token in ["abcdefgh", "a".repeat(TOKEN_MAX).as_str(), "!~!~!~!~"] {
            assert!(valid_token(token), "{token}");
        }
        assert!(!valid_token("abcdefg") && !valid_token("abc defgh") && !valid_token(&"a".repeat(TOKEN_MAX + 1)));
        assert_eq!(TOKEN_MAX, 32);
        // Reboot at the left edge, the access point at the right.
        assert!(out.contains("<p style=display:flex;justify-content:space-between><button id=r>Reboot</button><button id=a>Access point</button></p>"));
        // The page asks for its PIN by a post of its own: a `GET` issues none.
        assert!(out.contains("fetch('/pin',{method:'POST'})"));
        out.clear();
        write_system_page(&mut out, "clock", &status(true), false).unwrap();
        assert!(out.contains("(none is set)"));
    }

    /// The clips page: where the clips come from, their labels, and the
    /// upload with the PIN; nothing to upload to without the partition.
    #[test]
    fn clips_page_says_where_the_clips_are() {
        let page = |st: &Status| {
            let mut out = String::new();
            write_clips_page(&mut out, "clock", st).unwrap();
            out
        };
        let st = status(true);
        let out = page(&st);
        assert!(out.contains("No pack in flash (1024 KB for one): the clips are those built into the firmware."), "{out}");
        assert_eq!(out.matches("id=t>").count(), 1, "one PIN field");
        assert!(out.contains("<p><b>A new pack replaces all the clips of the one in flash; it does not add to them.</b></p>"));
        assert!(out.contains("up('/clips');") && out.contains("fetch('/pin',{method:'POST'})") && out.contains("<input type=file id=f>"));
        assert!(!out.contains("go('/token'") && !out.contains("up('/ota')"), "none of the system page's actions");
        for (_, label) in melody::CLIP_NAMES {
            assert!(out.contains(label), "{label}");
        }
        let mut none = st;
        none.clips_room = None;
        let out = page(&none);
        assert!(out.contains("has no <code>clips</code> partition") && out.ends_with("</fieldset></body></html>"), "{out}");
        assert!(!out.contains("<script>") && !out.contains("id=t>") && !out.contains("id=f>"), "nothing to upload to, no PIN asked for");
        let data = crate::pack::build(&[("one", "One's own", true, &[128; 8000]), ("two", "Two", false, &[128; 8000])]);
        let len = data.len();
        melody::install(Some(crate::pack::Pack::parse(Vec::leak(data)).unwrap()));
        let out = page(&st);
        assert!(out.contains(&format!("Pack in flash: 2 clips, {} KB of 1024.", len.div_ceil(1024))), "{out}");
        assert!(out.contains("<p>One's own · Two</p>"), "{out}");
        let mut json = String::new();
        write_json(&mut json, &Settings::DEFAULT, &st).unwrap();
        assert!(json.contains(r#""slot":"ota_1","clips":"pack","mode":"sta""#), "{json}");
        melody::install(None);
        json.clear();
        write_json(&mut json, &Settings::DEFAULT, &st).unwrap();
        assert!(json.contains(r#""clips":"builtin""#), "{json}");
    }

    #[test]
    fn wifi_line_says_why_not() {
        let mut st = status(false);
        st.rssi = None;
        let mut s = Settings::DEFAULT;
        s.wifi.ssid = Text::new("home").unwrap();
        let mut out = String::new();
        write_status(&mut out, &s, &st).unwrap();
        assert!(out.contains("Wi-Fi <b>home</b>: not connected."), "{out}");
        assert!(out.contains("MQTT off."), "{out}");
        out.clear();
        write_status(&mut out, &Settings::DEFAULT, &st).unwrap();
        assert!(out.contains("Wi-Fi: no network stored."), "{out}");
        // The broker line: connecting with the last error, then connected.
        s.mqtt.host = Text::new("10.1.1.5").unwrap();
        out.clear();
        write_status(&mut out, &s, &st).unwrap();
        assert!(out.contains("MQTT connecting <b>10.1.1.5:1883</b>, last: connection refused by the broker."), "{out}");
        st.mqtt = Link::Connected;
        out.clear();
        write_status(&mut out, &s, &st).unwrap();
        assert!(out.contains("MQTT connected <b>10.1.1.5:1883</b>."), "{out}");
    }

    /// The setup page without a list and everything else at its worst:
    /// what the list's budget is measured against.
    fn provision_page_fixed_worst() -> usize {
        let mut s = Settings::DEFAULT;
        s.hostname = Hostname::new(&"h".repeat(HOSTNAME_MAX)).unwrap();
        s.wifi.ssid = Text::new(&"\"".repeat(SSID_MAX)).unwrap(); // not listed: escaped into the field
        s.tz = Text::new(&"<".repeat(TZ_MAX)).unwrap(); // escaped into the field, four bytes each
        // More networks than the budget holds, none of them the stored one,
        // so the "more not shown" line is there and the field is full.
        let n = Network { ssid: Text::new(&format!("x{}", "\"".repeat(SSID_MAX - 1))).unwrap(), rssi: -100, open: true };
        let huge = [n; 100];
        let mut out = String::new();
        write_provision_page(&mut out, &s, &status(false), &huge, Note::Refused(FormError::BadValue, None)).unwrap();
        assert!(out.contains("<option value=\"\" selected>Other"), "the stored network is not listed, so Other is selected");
        let shown = out.matches("<option value=\"x").count();
        assert!(shown > 0 && shown < 100);
        assert!(out.contains(&format!("{} more not shown", 100 - shown)));
        let mut one = Counter::default();
        write_network_option(&mut one, &n, s.wifi.ssid.as_str()).unwrap();
        assert!(shown * one.0 <= NETWORK_LIST_BUDGET && (shown + 1) * one.0 > NETWORK_LIST_BUDGET, "the list stops at the budget");
        let fixed = out.len() - shown * one.0;
        println!("setup page without a list: {fixed} bytes; a hostile option is {} bytes", one.0);
        fixed
    }

    #[test]
    fn provision_page_budget_is_covered() {
        assert!(provision_page_fixed_worst() + NETWORK_LIST_BUDGET <= PAGE_MAX);
    }

    /// Hostile SSIDs cost five or six bytes a character, twice: the list
    /// stops within its budget and the page within the buffer.
    #[test]
    fn provision_page_survives_hostile_ssids() {
        let mut s = Settings::DEFAULT;
        s.wifi.ssid = Text::new(&"&".repeat(SSID_MAX)).unwrap();
        let hostile: Vec<Network> = (0..12)
            .map(|i| Network { ssid: Text::new(&format!("{i:02}{}", "\"".repeat(SSID_MAX - 2))).unwrap(), rssi: -(40 + i), open: false })
            .chain(core::iter::once(Network { ssid: s.wifi.ssid, rssi: -99, open: false }))
            .collect();
        let mut out = String::new();
        write_provision_page(&mut out, &s, &status(false), &hostile, Note::None).unwrap();
        println!("setup page, hostile: {} bytes of {PAGE_MAX}", out.len());
        assert!(out.len() <= PAGE_MAX, "{} bytes", out.len());
        let shown = out.matches("<option value=\"0").count();
        assert!((1..12).contains(&shown), "{shown} shown");
        assert!(out.contains(&format!("{} more not shown", 13 - shown)));
        assert!(out.contains("<option value=\"00&quot;"), "the strongest is listed first");
        // The stored network was past the budget: it is typed in, and Other is selected.
        assert!(out.contains("<option value=\"\" selected>Other"));
        assert!(out.contains(&format!("name=ssid id=i maxlength={SSID_MAX} value=\"{}\"", "&amp;".repeat(SSID_MAX))));
    }

    /// A refused setup form is filled in again from the tab's session
    /// storage: every field of the form is kept there but the password.
    #[test]
    fn setup_page_keeps_every_field_but_the_password() {
        let mut page = String::new();
        write_provision_page(&mut page, &Settings::DEFAULT, &status(false), &[], Note::None).unwrap();
        let list = &page[page.find("K=[").unwrap() + 3..];
        let kept: Vec<&str> = list[..list.find(']').unwrap()].split(',').map(|k| k.trim_matches('\'')).collect();
        let fields: Vec<&str> = field_names(&page).into_iter().filter(|n| *n != "password").collect();
        assert_eq!(kept, fields);
    }

    /// The setup page with the twelve networks the scan keeps, every
    /// SSID at its longest; all of them within the budget.
    #[test]
    fn provision_page_fits_the_response_buffer() {
        let mut s = Settings::DEFAULT;
        s.hostname = Hostname::new(&"h".repeat(HOSTNAME_MAX)).unwrap();
        s.wifi.ssid = Text::new(&"o".repeat(SSID_MAX)).unwrap(); // stored but not listed: shown in the field
        let networks: Vec<Network> = (0..12)
            .map(|i| Network { ssid: Text::new(&format!("{i:02}{}", "n".repeat(SSID_MAX - 2))).unwrap(), rssi: -100, open: i % 2 == 0 })
            .collect();
        let mut out = String::new();
        write_provision_page(&mut out, &s, &status(false), &networks, Note::Refused(FormError::BadValue, None)).unwrap();
        println!("setup page: {} bytes of {PAGE_MAX}", out.len());
        assert!(out.len() <= PAGE_MAX, "{} bytes", out.len());
        assert_eq!(out.matches("<option value=\"").count(), 13, "twelve networks and Other");
        assert!(!out.contains("more not shown"));
        assert!(out.contains("Other, typed below</option>"));
        // The stored network, when listed, is selected and the field is empty.
        s.wifi.ssid = networks[3].ssid;
        let mut again = String::new();
        write_provision_page(&mut again, &s, &status(false), &networks, Note::None).unwrap();
        assert!(again.contains(&format!("<option value=\"{}\" selected>", networks[3].ssid.as_str())));
        assert!(again.contains("<option value=\"\">Other"));
        assert!(again.contains(&format!("name=ssid id=i maxlength={SSID_MAX} value=\"\"")));
        assert!(again.contains("Password, empty to keep the stored one"), "a network is stored");
        assert!(again.contains("o.hidden=i.disabled=n.value!=''"), "the typed field is disabled, not only hidden");
        let mut empty = String::new();
        write_provision_page(&mut empty, &Settings::DEFAULT, &status(false), &[], Note::None).unwrap();
        assert!(empty.contains("No networks seen yet"));
        assert!(empty.contains("Password, empty for an open network"), "nothing stored");
        assert!(out.contains(&format!("value=\"{}\"", "o".repeat(SSID_MAX))), "the unlisted stored network is typed in");
        let mut saved = String::new();
        s.wifi.ssid = Text::new("a<b").unwrap();
        write_saved_page(&mut saved, &s).unwrap();
        assert!(saved.contains("<b>a&lt;b</b>"));
        assert!(saved.len() <= PAGE_MAX);
    }
}

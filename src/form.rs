//! The settings as key=value pairs: parsing a form body or query string
//! into changes, and the names the page and the API use. Pure code.

use crate::melody;
use crate::settings::{
    BRIGHTNESS_STEPS, DAY_NAMES, DAYS_ALL, DAYS_WEEKDAYS, DAYS_WEEKEND, HOSTNAME_MAX, Hostname, PALETTE_NAMES, Settings,
    TZ_LIMIT_MINUTES,
};

/// What a bad key or value looks like to the caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FormError {
    UnknownKey,
    BadValue,
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

/// Apply one key and value to `s`.
pub fn apply_one(s: &mut Settings, key: &str, value: &str) -> Result<(), FormError> {
    match key {
        "color" => {
            s.color = PALETTE_NAMES.iter().position(|n| *n == value).ok_or(FormError::BadValue)?;
        }
        "brightness" => {
            let pct = parse_u8(value).ok_or(FormError::BadValue)?;
            s.brightness = BRIGHTNESS_STEPS.iter().position(|p| *p == pct).ok_or(FormError::BadValue)?;
        }
        "alarm" => {
            let (h, m) = value.split_once(':').ok_or(FormError::BadValue)?;
            let (h, m) = (parse_u8(h).ok_or(FormError::BadValue)?, parse_u8(m).ok_or(FormError::BadValue)?);
            if h >= 24 || m >= 60 {
                return Err(FormError::BadValue);
            }
            s.alarm_hour = h;
            s.alarm_minute = m;
        }
        "armed" => s.alarm_armed = truthy(value),
        "twelve_hour" => s.twelve_hour = truthy(value),
        "skip" => s.skip_next = truthy(value),
        "tz" => {
            let tz: i16 = value.parse().map_err(|_| FormError::BadValue)?;
            if !(-TZ_LIMIT_MINUTES..=TZ_LIMIT_MINUTES).contains(&tz) {
                return Err(FormError::BadValue);
            }
            s.tz_offset_minutes = tz;
        }
        "hostname" => s.hostname = Hostname::new(value).ok_or(FormError::BadValue)?,
        "days" => s.alarm_days = parse_days(value).ok_or(FormError::BadValue)?,
        "melody" => s.melody = melody::index_of(value).ok_or(FormError::BadValue)?,
        _ => return Err(FormError::UnknownKey),
    }
    Ok(())
}

/// Apply every `key=value` pair of a form body or query string to `s`.
/// With `checkboxes_absent_off`, as for the page's form, the boolean
/// fields are turned off unless the body turns them on. The first bad
/// pair stops and is reported; earlier pairs stay applied.
pub fn apply(s: &mut Settings, body: &[u8], checkboxes_absent_off: bool) -> Result<(), FormError> {
    if checkboxes_absent_off {
        s.alarm_armed = false;
        s.twelve_hour = false;
        s.skip_next = false;
    }
    for pair in body.split(|&c| c == b'&').filter(|p| !p.is_empty()) {
        let (k, v) = match pair.iter().position(|&c| c == b'=') {
            Some(i) => (&pair[..i], &pair[i + 1..]),
            None => (pair, &pair[pair.len()..]),
        };
        let mut kbuf = [0u8; 16];
        let mut vbuf = [0u8; 32];
        let kn = percent_decode(k, &mut kbuf).ok_or(FormError::UnknownKey)?;
        let vn = percent_decode(v, &mut vbuf).ok_or(FormError::BadValue)?;
        let key = core::str::from_utf8(&kbuf[..kn]).map_err(|_| FormError::UnknownKey)?;
        let value = core::str::from_utf8(&vbuf[..vn]).map_err(|_| FormError::BadValue)?;
        apply_one(s, key, value)?;
    }
    Ok(())
}

/// The settings as JSON.
pub fn write_json(out: &mut impl core::fmt::Write, s: &Settings) -> core::fmt::Result {
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
    writeln!(
        out,
        "\",\"skip\":{},\"melody\":\"{}\",\"twelve_hour\":{},\"tz\":{},\"hostname\":\"{}\"}}",
        s.skip_next,
        melody::get(s.melody).name,
        s.twelve_hour,
        s.tz_offset_minutes,
        s.hostname.as_str()
    )
}

/// The settings page.
pub fn write_page(out: &mut impl core::fmt::Write, s: &Settings) -> core::fmt::Result {
    out.write_str(
        "<!doctype html><html><head><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\">\
<title>Clock</title><style>body{font-family:sans-serif;max-width:22em;margin:2em auto;padding:0 1em}\
label{display:block;margin:.8em 0}select,input[type=time],input[type=number],input[type=text]{font-size:1em;width:100%;box-sizing:border-box}\
button{font-size:1em;padding:.5em 1.5em;margin-top:1em}</style></head><body><h1>Clock</h1><form method=post action=/>",
    )?;
    out.write_str("<label>Colour <select name=color>")?;
    for (i, name) in PALETTE_NAMES.iter().enumerate() {
        write!(out, "<option value={name}{}>{name}</option>", if i == s.color { " selected" } else { "" })?;
    }
    out.write_str("</select></label><label>Brightness <select name=brightness>")?;
    for (i, pct) in BRIGHTNESS_STEPS.iter().enumerate() {
        write!(out, "<option value={pct}{}>{pct} %</option>", if i == s.brightness { " selected" } else { "" })?;
    }
    out.write_str("</select></label><label>Alarm sound <select name=melody>")?;
    for (i, m) in melody::MELODIES.iter().enumerate() {
        write!(out, "<option value={}{}>{}</option>", m.name, if i == s.melody { " selected" } else { "" }, m.label)?;
    }
    writeln!(
        out,
        "</select></label><label>Alarm <input type=time name=alarm value={:02}:{:02}></label>\
<label><input type=checkbox name=armed{}> Alarm armed</label>\
<label>Alarm days <select name=days>{}</select></label>\
<label><input type=checkbox name=skip{}> Skip the next alarm</label>\
<label><input type=checkbox name=twelve_hour{}> 12 hour clock</label>\
<label>Timezone, minutes east of UTC <input type=number name=tz min=-{lim} max={lim} step=15 value={}></label>\
<label>Hostname, for the DHCP server <input type=text name=hostname maxlength={hmax} pattern=\"[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?\" value=\"{}\"></label>\
<button>Save</button> <button formaction=/play>Play</button></form><p><small>Scriptable: <code>GET /api</code>, <code>POST /api</code> with the same fields.</small></p></body></html>",
        s.alarm_hour,
        s.alarm_minute,
        if s.alarm_armed { " checked" } else { "" },
        DayOptions(s.alarm_days),
        if s.skip_next { " checked" } else { "" },
        if s.twelve_hour { " checked" } else { "" },
        s.tz_offset_minutes,
        s.hostname.as_str(),
        lim = TZ_LIMIT_MINUTES,
        hmax = HOSTNAME_MAX,
    )
}

/// The options of the alarm-days select: the three presets, and the
/// current list as a fourth when it is none of them.
struct DayOptions(u8);

impl core::fmt::Display for DayOptions {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (mask, label) in [(DAYS_ALL, "Every day"), (DAYS_WEEKDAYS, "Weekdays"), (DAYS_WEEKEND, "Weekend")] {
            write!(f, "<option value=")?;
            write_days(f, mask)?;
            write!(f, "{}>{label}</option>", if self.0 == mask { " selected" } else { "" })?;
        }
        if ![DAYS_ALL, DAYS_WEEKDAYS, DAYS_WEEKEND].contains(&self.0) {
            write!(f, "<option value=")?;
            write_days(f, self.0)?;
            write!(f, " selected>")?;
            write_days(f, self.0)?;
            write!(f, "</option>")?;
        }
        Ok(())
    }
}

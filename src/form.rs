//! The settings as key=value pairs: parsing a form body or query string
//! into changes, and the names the page and the API use. Pure code.

use crate::melody;
use crate::settings::{
    BRIGHTNESS_STEPS, DAY_NAMES, DAYS_ALL, DAYS_WEEKDAYS, DAYS_WEEKEND, HOSTNAME_MAX, Hostname, PALETTE_NAMES, Settings,
    TZ_LIMIT_MINUTES,
};

/// Read-only status shown under the form and in the JSON. Not settings:
/// a posted status key is refused as unknown like any other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Status {
    /// What the firmware calls itself, and the app slot it runs from.
    pub version: &'static str,
    pub slot: &'static str,
    pub uptime_secs: u64,
    /// Self-heal resets since power-up.
    pub heals: u32,
    /// When the last self-heal happened, if one did and the time was known.
    pub last_heal: Option<HealStamp>,
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

/// The settings as JSON, followed by the read-only status keys `version`,
/// `slot`, `uptime` (seconds), `heals` and `last_heal` (Unix seconds or
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
        "\",\"skip\":{},\"melody\":\"{}\",\"twelve_hour\":{},\"tz\":{},\"hostname\":\"{}\",\"version\":\"{}\",\"slot\":\"{}\",\"uptime\":{},\"heals\":{},\"last_heal\":",
        s.skip_next,
        melody::get(s.melody).name,
        s.twelve_hour,
        s.tz_offset_minutes,
        s.hostname.as_str(),
        st.version,
        st.slot,
        st.uptime_secs,
        st.heals,
    )?;
    match st.last_heal {
        Some(h) => writeln!(out, "{}}}", h.unix_secs),
        None => writeln!(out, "null}}"),
    }
}

/// The status lines under the form: the firmware with a link to the
/// update page, then uptime and self-heals.
fn write_status(out: &mut impl core::fmt::Write, st: &Status) -> core::fmt::Result {
    write_firmware_line(out, st)?;
    write!(out, "<p><small>Up {}. Self-heal resets since power-up: {}", Span(st.uptime_secs as i64), st.heals)?;
    if let Some(h) = st.last_heal {
        write!(out, ", last {} {:02}:{:02}", WEEKDAY_LABELS[h.weekday as usize % 7], h.hour, h.minute)?;
        if let Some(ago) = h.ago_secs {
            write!(out, " ({} ago)", Span(ago))?;
        }
    }
    out.write_str(".</small></p>")
}

fn write_firmware_line(out: &mut impl core::fmt::Write, st: &Status) -> core::fmt::Result {
    write!(out, "<p><small>Firmware {} in {}. <a href=/update>Update</a>.</small></p>", st.version, st.slot)
}

/// Shared `<head>`: the viewport and the styles of both pages.
const HEAD: &str = "<!doctype html><html><head><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\">\
<style>body{font-family:sans-serif;max-width:22em;margin:2em auto;padding:0 1em}\
label{display:block;margin:.8em 0}select,input{font-size:1em;width:100%;box-sizing:border-box}input[type=checkbox]{width:auto}\
button{font-size:1em;padding:.5em 1.5em;margin-top:1em}p.err{color:#b00}progress{width:100%}</style>";

/// The firmware update page: a file input and the PIN the clock is
/// showing, posted as the raw body of `/ota` by script with the PIN in
/// the header, which is what the curl path sends with its token, so the
/// server needs no multipart. `XMLHttpRequest` for the progress bar;
/// after a `200` the page polls `/api` until the clock is back.
/// `lockout_secs` is set when no PIN was issued because of a wrong
/// attempt just before.
pub fn write_update_page(out: &mut impl core::fmt::Write, st: &Status, lockout_secs: Option<u64>) -> core::fmt::Result {
    out.write_str(HEAD)?;
    out.write_str("<title>Clock update</title></head><body><h1>Clock update</h1>")?;
    write_firmware_line(out, st)?;
    match lockout_secs {
        Some(secs) => write!(
            out,
            "<p class=err>A wrong PIN was entered. Reload this page in {secs} s to get a new one.</p>"
        )?,
        None => out.write_str("<p>The clock is showing a four-digit PIN. Enter it below.</p>")?,
    }
    out.write_str(
        "<label>Image file (<code>scripts/ota.sh</code> makes it)<input type=file id=f></label>\
<label>PIN on the clock<input type=text inputmode=numeric pattern=[0-9]* maxlength=4 autocomplete=off id=t></label>\
<button id=b>Upload</button><progress id=p value=0 max=1 hidden></progress>\
<p id=s>The panel goes dark for about ten seconds while the image is written. Do not unplug.</p>\
<p><a href=/>Back to the settings</a></p>\
<script>\
var $=function(i){return document.getElementById(i)},f=$('f'),t=$('t'),b=$('b'),p=$('p'),s=$('s');\
function say(m,e){s.textContent=m;s.className=e?'err':''}\
function poll(n){fetch('/api',{cache:'no-store'}).then(function(x){return x.json()})\
.then(function(j){say('Back up: firmware '+j.version+' in '+j.slot+'.')})\
.catch(function(){if(n>0)setTimeout(function(){poll(n-1)},2000);else say('The clock has not come back; check it.',1)})}\
b.onclick=function(){if(!f.files[0]){say('Choose an image file.',1);return}\
if(!/^[0-9]{4}$/.test(t.value)){say('Enter the four digits the clock is showing.',1);return}\
var x=new XMLHttpRequest();x.open('POST','/ota');x.setRequestHeader('Authorization','PIN '+t.value);\
x.upload.onprogress=function(e){if(e.lengthComputable)p.value=e.loaded/e.total};\
x.onload=function(){b.disabled=false;if(x.status==200){say(x.responseText.trim()+' Waiting for the clock to come back.');setTimeout(function(){poll(20)},5000)}\
else say(x.status+': '+x.responseText.trim(),1)};\
x.onerror=function(){b.disabled=false;say('Upload failed: connection lost.',1)};\
b.disabled=true;p.hidden=false;p.value=0;say('Uploading.');x.send(f.files[0])}\
</script></body></html>",
    )
}

/// The settings page. `error` is why the last form post was refused, if
/// it was; the page then says so above the form.
pub fn write_page(
    out: &mut impl core::fmt::Write,
    s: &Settings,
    st: &Status,
    error: Option<FormError>,
) -> core::fmt::Result {
    out.write_str(HEAD)?;
    out.write_str("<title>Clock</title></head><body><h1>Clock</h1>")?;
    match error {
        None => {}
        Some(FormError::BadValue) => out.write_str("<p class=err>Nothing was saved: a field had a bad value.</p>")?,
        Some(FormError::UnknownKey) => out.write_str("<p class=err>Nothing was saved: an unknown field was sent.</p>")?,
    }
    out.write_str("<form method=post action=/>")?;
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
<button>Save</button> <button formaction=/play>Play</button></form>",
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
    )?;
    write_status(out, st)?;
    out.write_str(
        "<p><small>Scriptable: <code>GET /api</code>, <code>POST /api</code> with the same fields.</small></p></body></html>",
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

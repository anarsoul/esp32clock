//! The settings as key=value pairs: parsing a form body or query string
//! into changes, and the names the page and the API use. Pure code.

use core::fmt::Write as _;

use crate::melody;
use crate::settings::{
    BRIGHTNESS_STEPS, DAY_NAMES, DAYS_ALL, DAYS_WEEKDAYS, DAYS_WEEKEND, HOSTNAME_MAX, Hostname, PALETTE_NAMES,
    PASSWORD_MAX, PASSWORD_MIN, SSID_MAX, Secret, Settings, TOKEN_MAX, TZ_LIMIT_MINUTES, Text, valid_password,
    valid_token,
};

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
    /// A credential key (`ssid`, `password`) from a request that did not
    /// carry the token.
    Forbidden,
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

/// Apply one key and value to `s`. `credentials` says whether the
/// request may set the network (`ssid`, `password`): only with the
/// token, or on the access point's own page. The check is on the key,
/// never on whether the value would change anything, so a request
/// without the token learns nothing about the stored password.
pub fn apply_one(s: &mut Settings, key: &str, value: &str, credentials: bool) -> Result<(), FormError> {
    match key {
        "ssid" | "ssid_pick" | "password" if !credentials => return Err(FormError::Forbidden),
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
/// fields are turned off unless the body turns them on. `credentials`
/// is passed to `apply_one`. The first bad pair stops and is reported;
/// earlier pairs stay applied, so the caller works on a copy.
pub fn apply(s: &mut Settings, body: &[u8], checkboxes_absent_off: bool, credentials: bool) -> Result<(), FormError> {
    if checkboxes_absent_off {
        s.alarm_armed = false;
        s.twelve_hour = false;
        s.skip_next = false;
    }
    for (key, value) in Pairs(body) {
        let mut kbuf = [0u8; 16];
        let mut vbuf = [0u8; VALUE_MAX];
        let kn = percent_decode(key, &mut kbuf).ok_or(FormError::UnknownKey)?;
        let vn = percent_decode(value, &mut vbuf).ok_or(FormError::BadValue)?;
        let key = core::str::from_utf8(&kbuf[..kn]).map_err(|_| FormError::UnknownKey)?;
        let value = core::str::from_utf8(&vbuf[..vn]).map_err(|_| FormError::BadValue)?;
        apply_one(s, key, value, credentials)?;
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

/// The settings as JSON (the SSID, then whether a password and a token
/// are set, never their values), followed by the read-only status keys
/// `version`, `slot`, `mode`, `uptime` (seconds), `heals` and `last_heal`
/// (Unix seconds or null).
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
        "\",\"skip\":{},\"melody\":\"{}\",\"twelve_hour\":{},\"tz\":{},\"hostname\":\"{}\",\"ssid\":\"{}\",\"password_set\":{},\"token_set\":{},\"version\":\"{}\",\"slot\":\"{}\",\"mode\":\"{}\",\"uptime\":{},\"heals\":{},\"last_heal\":",
        s.skip_next,
        melody::name(s.melody),
        s.twelve_hour,
        s.tz_offset_minutes,
        s.hostname.as_str(),
        JsonStr(s.wifi.ssid.as_str()),
        !s.wifi.password.0.is_empty(),
        !s.token.0.is_empty(),
        st.version,
        st.slot,
        st.mode,
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
button{font-size:1em;padding:.5em 1.5em;margin-top:1em}p.err{color:#b00}progress{width:100%}[hidden]{display:none}</style>";

/// The firmware update page: a file input and the PIN the clock is
/// showing, posted as the raw body of `/ota` by script with the PIN in
/// the header, which is what the curl path sends with its token, so the
/// server needs no multipart. `XMLHttpRequest` for the progress bar;
/// after a `200` the page polls `/api` until the clock is back. A
/// second form sets the API token the same way, `POST /token` with the
/// PIN in the header; `token_set` says whether one is set now.
/// `lockout_secs` is set when no PIN was issued because of a wrong
/// attempt just before.
pub fn write_update_page(
    out: &mut impl core::fmt::Write,
    st: &Status,
    token_set: bool,
    lockout_secs: Option<u64>,
) -> core::fmt::Result {
    out.write_str(HEAD)?;
    out.write_str("<title>Clock update</title></head><body><h1>Clock update</h1>")?;
    write_firmware_line(out, st)?;
    match lockout_secs {
        Some(secs) => write!(
            out,
            "<p class=err>A wrong PIN was entered. Reload this page in {secs} s to get a new one.</p>"
        )?,
        None => out.write_str("<p>The clock is showing a four-digit PIN. Enter it below. One PIN is one action; reload for another.</p>")?,
    }
    out.write_str(
        "<label>PIN on the clock<input type=text inputmode=numeric pattern=[0-9]* maxlength=4 autocomplete=off id=t></label>\
<label>Image file (<code>scripts/ota.sh</code> makes it)<input type=file id=f></label>\
<button id=b>Upload</button><progress id=p value=0 max=1 hidden></progress>\
<p id=s>The panel goes dark for about ten seconds while the image is written. Do not unplug.</p>",
    )?;
    write!(
        out,
        "<h2>API token</h2><p><small>What the scripts send as <code>Bearer</code>; never shown. {}. Empty clears it.</small></p>\
<label>New token, 8 to 32 characters<input type=password maxlength={} autocomplete=new-password id=k></label>\
<button id=c>Set token</button>",
        if token_set { "One is set" } else { "None is set" },
        TOKEN_MAX
    )?;
    out.write_str(
        "<p><a href=/>Back to the settings</a></p>\
<script>\
var $=function(i){return document.getElementById(i)},f=$('f'),t=$('t'),b=$('b'),p=$('p'),s=$('s'),k=$('k'),c=$('c');\
function say(m,e){s.textContent=m;s.className=e?'err':''}\
function pin(){if(/^[0-9]{4}$/.test(t.value))return true;say('Enter the four digits the clock is showing.',1);return false}\
c.onclick=function(){if(!pin())return;var y=new XMLHttpRequest();y.open('POST','/token');\
y.setRequestHeader('Authorization','PIN '+t.value);y.setRequestHeader('Content-Type','application/x-www-form-urlencoded');\
y.onload=function(){say(y.status+': '+y.responseText.trim(),y.status!=200)};y.onerror=function(){say('Request failed.',1)};\
y.send('token='+encodeURIComponent(k.value))};\
function poll(n){fetch('/api',{cache:'no-store'}).then(function(x){return x.json()})\
.then(function(j){say('Back up: firmware '+j.version+' in '+j.slot+'.')})\
.catch(function(){if(n>0)setTimeout(function(){poll(n-1)},2000);else say('The clock has not come back; check it.',1)})}\
b.onclick=function(){if(!f.files[0]){say('Choose an image file.',1);return}\
if(!pin())return;\
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
        Some(FormError::Forbidden) => out.write_str("<p class=err>Nothing was saved: the network is set with the token.</p>")?,
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
    write!(
        out,
        "<option value={}{}>Random tune</option>",
        melody::RANDOM_NAME,
        if s.melody == melody::RANDOM { " selected" } else { "" }
    )?;
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

/// The provisioning page, what `GET /` serves while the clock is an
/// access point: the networks the scan saw as a list, a field for one
/// it did not (hidden, or out of range at the time), the password, and
/// the timezone and the hostname since they are typed too; nothing the
/// panel cannot show without a time. The password field is empty
/// whatever is stored (write-only); empty on submit means an open
/// network. The Rescan button posts the form to `/scan`, which scans
/// and comes back here.
pub fn write_provision_page(
    out: &mut impl core::fmt::Write,
    s: &Settings,
    st: &Status,
    networks: &[Network],
    error: Option<FormError>,
) -> core::fmt::Result {
    out.write_str(HEAD)?;
    out.write_str("<title>Clock setup</title></head><body><h1>Clock setup</h1>")?;
    match error {
        None => out.write_str("<p>The clock has no network to join, or was asked to wait for one. Give it yours and it restarts into it.</p>")?,
        Some(FormError::BadValue) => out.write_str("<p class=err>Nothing was saved: a field had a bad value, or no network was named.</p>")?,
        Some(FormError::UnknownKey) => out.write_str("<p class=err>Nothing was saved: an unknown field was sent.</p>")?,
        Some(FormError::Forbidden) => out.write_str("<p class=err>Nothing was saved.</p>")?,
    }
    let stored = s.wifi.ssid.as_str();
    let listed = networks.iter().any(|n| n.ssid.as_str() == stored);
    out.write_str("<form method=post action=/><label>Network <select name=ssid_pick id=n>")?;
    for n in networks {
        write!(
            out,
            "<option value=\"{}\"{}>{} ({} dBm{})</option>",
            HtmlStr(n.ssid.as_str()),
            if n.ssid.as_str() == stored { " selected" } else { "" },
            HtmlStr(n.ssid.as_str()),
            n.rssi,
            if n.open { ", open" } else { "" },
        )?;
    }
    write!(
        out,
        "<option value=\"\"{}>Other, typed below</option></select></label>\
<label id=o>Other network name <input type=text name=ssid maxlength={smax} value=\"{}\"></label>\
<label>Password, empty for an open network <input type=password name=password minlength={pmin} maxlength={pmax} autocomplete=off></label>\
<label>Timezone, minutes east of UTC <input type=number name=tz min=-{lim} max={lim} step=15 value={}></label>\
<label>Hostname, for the DHCP server <input type=text name=hostname maxlength={hmax} pattern=\"[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?\" value=\"{}\"></label>\
<button>Save and join</button> <button formaction=/scan formnovalidate>Rescan</button></form>",
        if listed { "" } else { " selected" },
        HtmlStr(if listed { "" } else { stored }),
        s.tz_offset_minutes,
        s.hostname.as_str(),
        smax = SSID_MAX,
        pmin = PASSWORD_MIN,
        pmax = PASSWORD_MAX,
        lim = TZ_LIMIT_MINUTES,
        hmax = HOSTNAME_MAX,
    )?;
    if networks.is_empty() {
        out.write_str("<p><small>No networks seen yet; rescan, or type the name.</small></p>")?;
    }
    // The typed-name field only when the list's choice is "Other".
    out.write_str(
        "<script>var n=document.getElementById('n'),o=document.getElementById('o');\
function f(){o.hidden=n.value!=''}n.onchange=f;f()</script>",
    )?;
    write_firmware_line(out, st)?;
    out.write_str(
        "<p><small>Scriptable: <code>POST /api</code> with <code>ssid</code> and <code>password</code>.</small></p></body></html>",
    )
}

/// The answer to a provisioning save: the clock is about to restart
/// into the named network.
pub fn write_saved_page(out: &mut impl core::fmt::Write, s: &Settings) -> core::fmt::Result {
    out.write_str(HEAD)?;
    write!(
        out,
        "<title>Clock setup</title></head><body><h1>Saved</h1>\
<p>The clock restarts now and joins <b>{}</b>. Join that network yourself and open \
<a href=\"http://{}/\">http://{}/</a>, or the address the clock shows when both buttons are held.</p>\
<p><small>If it does not appear within a minute the password was wrong: the panel stays blank, and holding both buttons for ten seconds brings this page back.</small></p>\
</body></html>",
        HtmlStr(s.wifi.ssid.as_str()),
        s.hostname.as_str(),
        s.hostname.as_str(),
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
            last_heal: heal.then_some(HealStamp { unix_secs: 1_800_000_000, weekday: 2, hour: 23, minute: 59, ago_secs: Some(400 * 86_400) }),
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
        let body = b"color=cyan&brightness=25&alarm=06%3A30&armed=on&days=weekend&skip=1&twelve_hour=true&tz=-420&hostname=Bedroom-Clock&melody=random";
        apply(&mut s, body, true, false).unwrap();
        assert_eq!(s.color, PALETTE_NAMES.iter().position(|n| *n == "cyan").unwrap());
        assert_eq!(s.brightness_pct(), 25);
        assert_eq!((s.alarm_hour, s.alarm_minute), (6, 30));
        assert!(s.alarm_armed && s.skip_next && s.twelve_hour);
        assert_eq!(s.alarm_days, DAYS_WEEKEND);
        assert_eq!(s.tz_offset_minutes, -420);
        assert_eq!(s.hostname.as_str(), "bedroom-clock");
        assert_eq!(s.melody, melody::RANDOM);
        assert!(s.is_valid());
        // The page's form: absent checkboxes turn off; the API's does not.
        apply(&mut s, b"tz=0", true, false).unwrap();
        assert!(!s.alarm_armed && !s.skip_next && !s.twelve_hour);
        s.alarm_armed = true;
        apply(&mut s, b"tz=60", false, false).unwrap();
        assert!(s.alarm_armed);
        // An empty body, stray separators and a key without a value.
        apply(&mut s, b"", false, false).unwrap();
        apply(&mut s, b"&&tz=15&", false, false).unwrap();
        assert_eq!(s.tz_offset_minutes, 15);
        assert_eq!(apply(&mut s, b"armed", false, false), Ok(()));
        assert!(!s.alarm_armed);
    }

    #[test]
    fn apply_refuses_bad_input() {
        let s0 = Settings::DEFAULT;
        let try_body = |body: &[u8]| {
            let mut s = s0;
            apply(&mut s, body, false, false)
        };
        assert_eq!(try_body(b"colour=red"), Err(FormError::UnknownKey));
        assert_eq!(try_body(b"color=pink"), Err(FormError::BadValue));
        assert_eq!(try_body(b"brightness=33"), Err(FormError::BadValue));
        assert_eq!(try_body(b"brightness=10"), Ok(()));
        assert_eq!(try_body(b"alarm=24:00"), Err(FormError::BadValue));
        assert_eq!(try_body(b"alarm=23:60"), Err(FormError::BadValue));
        assert_eq!(try_body(b"alarm=7"), Err(FormError::BadValue));
        assert_eq!(try_body(b"tz=841"), Err(FormError::BadValue));
        assert_eq!(try_body(b"tz=-841"), Err(FormError::BadValue));
        assert_eq!(try_body(b"tz=840"), Ok(()));
        assert_eq!(try_body(b"tz=%zz"), Err(FormError::BadValue));
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
    fn network_keys_need_the_credentials_flag() {
        let mut s = Settings::DEFAULT;
        s.wifi.ssid = Text::new("home").unwrap();
        // Refused on the key alone, even when the value is what is stored.
        for body in [b"ssid=home".as_slice(), b"ssid_pick=home", b"password=", b"tz=0&password=12345678"] {
            let mut t = s;
            assert_eq!(apply(&mut t, body, false, false), Err(FormError::Forbidden), "{:?}", core::str::from_utf8(body));
        }
        let mut t = s;
        apply(&mut t, b"ssid_pick=&ssid=Caf%C3%A9+Net&password=p%26ss+word", false, true).unwrap();
        assert_eq!(t.wifi.ssid.as_str(), "Café Net");
        assert_eq!(t.wifi.password.0.as_str(), "p&ss word");
        // Empty SSID leaves the network alone; the password rule applies.
        let mut u = t;
        apply(&mut u, b"ssid=&ssid_pick=", false, true).unwrap();
        assert_eq!(u.wifi.ssid, t.wifi.ssid);
        assert_eq!(apply(&mut u, b"password=1234567", false, true), Err(FormError::BadValue));
        apply(&mut u, b"password=", false, true).unwrap();
        assert!(u.wifi.password.0.is_empty(), "empty is an open network");
        let long = format!("ssid={}", "s".repeat(SSID_MAX + 1));
        assert_eq!(apply(&mut u, long.as_bytes(), false, true), Err(FormError::BadValue));
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
        assert!(out.contains(r#""last_heal":1800000000}"#));
        assert!(out.ends_with("}\n"));
        let mut none = String::new();
        write_json(&mut none, &Settings::DEFAULT, &status(false)).unwrap();
        assert!(none.contains(r#""password_set":false,"token_set":false"#));
        assert!(none.ends_with(r#""last_heal":null}"#.to_string().as_str() ) || none.ends_with("null}\n"));
    }

    #[test]
    fn escaping() {
        assert_eq!(format!("{}", HtmlStr("a<b>&\"c\"")), "a&lt;b&gt;&amp;&quot;c&quot;");
        assert_eq!(format!("{}", JsonStr("a\"b\\c\n")), "a\\\"b\\\\c\\u000a");
    }

    /// The settings page at its longest: every string at its limit, the
    /// notice, a custom day list, every checkbox, the longest status.
    #[test]
    fn settings_page_fits_the_response_buffer() {
        let mut s = Settings::DEFAULT;
        s.hostname = Hostname::new(&"h".repeat(HOSTNAME_MAX)).unwrap();
        s.alarm_days = 0b1010101;
        s.alarm_armed = true;
        s.skip_next = true;
        s.twelve_hour = true;
        s.tz_offset_minutes = -TZ_LIMIT_MINUTES;
        s.melody = melody::RANDOM;
        let mut out = String::new();
        write_page(&mut out, &s, &status(true), Some(FormError::Forbidden)).unwrap();
        println!("settings page: {} bytes of {PAGE_MAX}", out.len());
        assert!(out.len() <= PAGE_MAX, "{} bytes", out.len());
        assert!(out.ends_with("</html>"));
    }

    #[test]
    fn update_page_fits_the_response_buffer() {
        let mut out = String::new();
        write_update_page(&mut out, &status(true), true, Some(10)).unwrap();
        println!("update page: {} bytes of {PAGE_MAX}", out.len());
        assert!(out.len() <= PAGE_MAX, "{} bytes", out.len());
        assert!(out.ends_with("</html>"));
    }

    /// The setup page with the twelve networks the scan keeps, every
    /// SSID at its longest. (An SSID of 32 `"` would double its share
    /// through `HtmlStr`; see PLAN.md.)
    #[test]
    fn provision_page_fits_the_response_buffer() {
        let mut s = Settings::DEFAULT;
        s.hostname = Hostname::new(&"h".repeat(HOSTNAME_MAX)).unwrap();
        s.wifi.ssid = Text::new(&"o".repeat(SSID_MAX)).unwrap(); // stored but not listed: shown in the field
        let networks: Vec<Network> = (0..12)
            .map(|i| Network { ssid: Text::new(&format!("{i:02}{}", "n".repeat(SSID_MAX - 2))).unwrap(), rssi: -100, open: i % 2 == 0 })
            .collect();
        let mut out = String::new();
        write_provision_page(&mut out, &s, &status(false), &networks, Some(FormError::BadValue)).unwrap();
        println!("setup page: {} bytes of {PAGE_MAX}", out.len());
        assert!(out.len() <= PAGE_MAX, "{} bytes", out.len());
        assert!(out.contains("Other, typed below</option>"));
        assert!(out.contains(&format!("value=\"{}\"", "o".repeat(SSID_MAX))), "the unlisted stored network is typed in");
        let mut saved = String::new();
        s.wifi.ssid = Text::new("a<b").unwrap();
        write_saved_page(&mut saved, &s).unwrap();
        assert!(saved.contains("<b>a&lt;b</b>"));
        assert!(saved.len() <= PAGE_MAX);
    }
}

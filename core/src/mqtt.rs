// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! Home Assistant over MQTT, the pure half: the clock's id and its
//! topics, the discovery payload that tells Home Assistant what the
//! clock is, the routing of what arrives and the commands it may
//! carry. The task that owns the socket is `src/mqtt.rs` in the
//! firmware.
//!
//! The state the clock publishes is the `/api` JSON (`form::write_json`)
//! byte for byte, so every entity here reads a key of that JSON through
//! a value template, and a command on `set/<key>` is the same key and
//! value `/api` takes, through `form::apply_one` on a copy. Nothing
//! that moves the clock to another network or broker is taken here.

use core::fmt::Write as _;

use crate::form::{self, Allow, FormError, JsonStr};
use crate::melody;
use crate::sensors::Tenths;
use crate::settings::{BRIGHTNESS_STEPS, PALETTE_NAMES, Settings, TEMP_OFFSET_LIMIT_DC, TempShow};

/// Where the clock publishes: `esp32clock/<id>/…`.
pub const PREFIX: &str = "esp32clock";

/// Home Assistant's own status topic and its birth payload: when it
/// comes up it says `online` here, and the clock resends discovery.
pub const HA_STATUS_TOPIC: &str = "homeassistant/status";
pub const HA_ONLINE: &str = "online";

/// The availability payloads on the status topic; `OFFLINE` is also the
/// will.
pub const ONLINE: &str = "online";
pub const OFFLINE: &str = "offline";

/// The repository, for the origin block.
const REPO_URL: &str = "https://github.com/anarsoul/esp32clock";

/// Longest topic the clock uses: the discovery topic is 40 bytes, a
/// `set/` command with the longest key 39.
pub type TopicBuf = heapless::String<48>;

/// Longest discovery payload the firmware's buffer must hold; the test
/// prints the worst case.
pub const DISCOVERY_MAX: usize = 4608;

/// What the MQTT task is doing, for the page and the JSON.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Link {
    /// No broker stored.
    Off,
    /// Between connections, with the last failure if there was one.
    Connecting { last_error: Option<&'static str> },
    Connected,
}

impl Link {
    pub const fn name(self) -> &'static str {
        match self {
            Link::Off => "off",
            Link::Connecting { .. } => "connecting",
            Link::Connected => "connected",
        }
    }
}

/// The clock's id on the bus: `clock-` and the last three bytes of its
/// MAC in hex. Stable across hostname changes, so no retained message
/// goes stale when the clock is renamed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeviceId([u8; 12]);

const HEX: &[u8; 16] = b"0123456789abcdef";

impl DeviceId {
    pub fn from_mac(mac: [u8; 6]) -> Self {
        let mut b = *b"clock-000000";
        for (i, byte) in mac[3..].iter().enumerate() {
            b[6 + 2 * i] = HEX[(byte >> 4) as usize];
            b[7 + 2 * i] = HEX[(byte & 0xf) as usize];
        }
        DeviceId(b)
    }

    pub fn as_str(&self) -> &str {
        // Only ASCII gets in.
        core::str::from_utf8(&self.0).unwrap_or("clock-")
    }

    /// `esp32clock/<id>/<suffix>`. The suffixes are this module's
    /// constants and fit by construction.
    pub fn topic(&self, suffix: &str) -> TopicBuf {
        let mut t = TopicBuf::new();
        let _ = write!(t, "{PREFIX}/{}/{suffix}", self.as_str());
        t
    }

    /// `online` or `offline`, retained; also the will.
    pub fn status_topic(&self) -> TopicBuf {
        self.topic("status")
    }

    /// The `/api` JSON, retained.
    pub fn state_topic(&self) -> TopicBuf {
        self.topic("state")
    }

    /// The one subscription for every `set/<key>`.
    pub fn set_filter(&self) -> TopicBuf {
        self.topic("set/+")
    }

    /// `play`, `silence` or `reload`.
    pub fn cmd_topic(&self) -> TopicBuf {
        self.topic("cmd")
    }

    /// Where the device discovery payload goes.
    pub fn discovery_topic(&self) -> TopicBuf {
        let mut t = TopicBuf::new();
        let _ = write!(t, "homeassistant/device/{}/config", self.as_str());
        t
    }

    /// What a topic the clock is subscribed to means.
    pub fn route<'t>(&self, topic: &'t str) -> Route<'t> {
        if topic == HA_STATUS_TOPIC {
            return Route::HaStatus;
        }
        let rest = topic
            .strip_prefix(PREFIX)
            .and_then(|r| r.strip_prefix('/'))
            .and_then(|r| r.strip_prefix(self.as_str()))
            .and_then(|r| r.strip_prefix('/'));
        match rest {
            Some("cmd") => Route::Command,
            Some(rest) => match rest.strip_prefix("set/") {
                Some(key) if !key.is_empty() && !key.contains('/') => Route::Set(key),
                _ => Route::Other,
            },
            None => Route::Other,
        }
    }
}

/// Where an incoming publication belongs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route<'t> {
    /// `set/<key>`: the payload is the value, as for `/api`.
    Set(&'t str),
    /// `cmd`: the payload is a `Command`.
    Command,
    /// Home Assistant's status: `online` asks for discovery again.
    HaStatus,
    Other,
}

/// What the `cmd` topic takes; the buttons' `pl_prs` payloads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    /// Preview the stored melody once, as `POST /play` does.
    Play,
    /// End a ring, as a press does.
    Silence,
    /// `POST /reload`: the deliberate reset with the carry record.
    Reload,
}

impl Command {
    pub const ALL: [Command; 3] = [Command::Play, Command::Silence, Command::Reload];

    pub fn parse(payload: &str) -> Option<Command> {
        Self::ALL.iter().copied().find(|c| c.name() == payload.trim())
    }

    pub const fn name(self) -> &'static str {
        match self {
            Command::Play => "play",
            Command::Silence => "silence",
            Command::Reload => "reload",
        }
    }
}

/// The keys `set/<key>` takes: the clock's own settings, each with an
/// entity. The timezone, the hostname, the network, the token and the
/// broker are not among them: they are the page's, and the bus is no
/// way to move the clock elsewhere.
pub const SET_KEYS: [&str; 10] = [
    "color",
    "brightness",
    "alarm",
    "armed",
    "days",
    "skip",
    "melody",
    "twelve_hour",
    "temp_show",
    "temp_offset",
];

/// Apply a `set/<key>` command to a copy of the settings: the same
/// parser as `/api`, after the allow list. `Forbidden` for a key not on
/// it, known to the form or not.
pub fn apply_set(s: &mut Settings, key: &str, value: &str) -> Result<(), FormError> {
    if !SET_KEYS.contains(&key) {
        return Err(FormError::Forbidden);
    }
    form::apply_one(s, key, value, Allow::NONE)
}

/// What the discovery payload says about the clock.
pub struct Device<'a> {
    pub id: &'a DeviceId,
    /// The hostname: the device's name in Home Assistant.
    pub name: &'a str,
    /// The clock's address, for the configuration URL.
    pub ip: Option<[u8; 4]>,
    /// The firmware version.
    pub version: &'a str,
}

/// The components of the payload, written one after the other.
struct Components<'o, W: core::fmt::Write> {
    out: &'o mut W,
    id: &'o str,
    first: bool,
}

impl<W: core::fmt::Write> Components<'_, W> {
    /// `"<key>":{"p":"<platform>","name":"<name>","uniq_id":"<id>_<key>"`;
    /// the caller adds the rest and `close`s.
    fn open(&mut self, key: &str, platform: &str, name: &str) -> core::fmt::Result {
        if !self.first {
            self.out.write_char(',')?;
        }
        self.first = false;
        write!(self.out, "\"{key}\":{{\"p\":\"{platform}\",\"name\":\"{name}\",\"uniq_id\":\"{}_{key}\"", self.id)
    }

    fn close(&mut self) -> core::fmt::Result {
        self.out.write_char('}')
    }

    /// The state is the key of the `/api` JSON.
    fn template(&mut self, key: &str) -> core::fmt::Result {
        write!(self.out, ",\"val_tpl\":\"{{{{ value_json.{key} }}}}\"")
    }

    /// A boolean key of the JSON as `on`/`off`, which is what the switch
    /// then sends and `form::truthy` reads.
    fn on_off(&mut self, key: &str) -> core::fmt::Result {
        write!(self.out, ",\"val_tpl\":\"{{{{ 'on' if value_json.{key} else 'off' }}}}\",\"pl_on\":\"on\",\"pl_off\":\"off\"")
    }

    /// Commands go to `set/<key>`.
    fn set(&mut self, key: &str) -> core::fmt::Result {
        write!(self.out, ",\"cmd_t\":\"{PREFIX}/{}/set/{key}\"", self.id)
    }

    /// A button: its press is `command` on the `cmd` topic.
    fn press(&mut self, command: Command) -> core::fmt::Result {
        write!(self.out, ",\"cmd_t\":\"{PREFIX}/{}/cmd\",\"pl_prs\":\"{}\"", self.id, command.name())
    }

    /// The options of a select.
    fn options<'n>(&mut self, names: impl IntoIterator<Item = &'n str>) -> core::fmt::Result {
        self.out.write_str(",\"ops\":[")?;
        for (i, n) in names.into_iter().enumerate() {
            if i > 0 {
                self.out.write_char(',')?;
            }
            write!(self.out, "\"{n}\"")?;
        }
        self.out.write_char(']')
    }

    fn diagnostic(&mut self) -> core::fmt::Result {
        self.out.write_str(",\"ent_cat\":\"diagnostic\"")
    }

    fn config(&mut self) -> core::fmt::Result {
        self.out.write_str(",\"ent_cat\":\"config\"")
    }

    fn icon(&mut self, icon: &str) -> core::fmt::Result {
        write!(self.out, ",\"ic\":\"mdi:{icon}\"")
    }
}

/// The device discovery payload: one device named after the hostname,
/// and every entity, each reading the state topic through a template
/// and commanding `set/<key>` or `cmd`. Keys are Home Assistant's
/// documented abbreviations. Not retained: resent at every connect and
/// on Home Assistant's birth message.
pub fn write_discovery(out: &mut impl core::fmt::Write, d: &Device<'_>) -> core::fmt::Result {
    let id = d.id.as_str();
    write!(
        out,
        "{{\"dev\":{{\"ids\":[\"{id}\"],\"name\":\"{}\",\"mf\":\"esp32clock\",\"mdl\":\"HU-058 on ESP32-C3\",\"sw\":\"{}\"",
        JsonStr(d.name),
        JsonStr(d.version)
    )?;
    if let Some(ip) = d.ip {
        write!(out, ",\"cu\":\"http://{}.{}.{}.{}/\"", ip[0], ip[1], ip[2], ip[3])?;
    }
    write!(
        out,
        "}},\"o\":{{\"name\":\"esp32clock\",\"sw\":\"{}\",\"url\":\"{REPO_URL}\"}},\
\"stat_t\":\"{PREFIX}/{id}/state\",\"avty_t\":\"{PREFIX}/{id}/status\",\"qos\":0,\"cmps\":{{",
        JsonStr(d.version)
    )?;
    let mut c = Components { out, id, first: true };

    c.open("color", "select", "Colour")?;
    c.template("color")?;
    c.set("color")?;
    c.options(PALETTE_NAMES)?;
    c.icon("palette")?;
    c.close()?;

    c.open("brightness", "select", "Brightness")?;
    c.template("brightness")?;
    c.set("brightness")?;
    c.out.write_str(",\"ops\":[")?;
    for (i, pct) in BRIGHTNESS_STEPS.iter().enumerate() {
        write!(c.out, "{}\"{pct}\"", if i > 0 { "," } else { "" })?;
    }
    c.out.write_char(']')?;
    c.icon("brightness-6")?;
    c.close()?;

    c.open("alarm", "time", "Alarm")?;
    c.template("alarm")?;
    c.set("alarm")?;
    c.close()?;

    c.open("armed", "switch", "Alarm armed")?;
    c.on_off("armed")?;
    c.set("armed")?;
    c.icon("alarm")?;
    c.close()?;

    c.open("skip", "switch", "Skip next alarm")?;
    c.on_off("skip")?;
    c.set("skip")?;
    c.icon("alarm-snooze")?;
    c.close()?;

    c.open("days", "text", "Alarm days")?;
    c.template("days")?;
    c.set("days")?;
    c.icon("calendar-week")?;
    c.close()?;

    c.open("melody", "select", "Melody")?;
    c.template("melody")?;
    c.set("melody")?;
    c.options(melody::MELODIES.iter().map(|m| m.name).chain([melody::RANDOM_NAME]))?;
    c.icon("music")?;
    c.close()?;

    c.open("twelve_hour", "switch", "12 hour clock")?;
    c.on_off("twelve_hour")?;
    c.set("twelve_hour")?;
    c.config()?;
    c.close()?;

    c.open("temp_show", "select", "Temperature on the panel")?;
    c.template("temp_show")?;
    c.set("temp_show")?;
    c.options(TempShow::NAMES)?;
    c.config()?;
    c.close()?;

    c.open("temp_offset", "number", "Thermometer offset")?;
    c.template("temp_offset")?;
    c.set("temp_offset")?;
    // Home Assistant sends a float; the parser takes one decimal.
    write!(
        c.out,
        ",\"cmd_tpl\":\"{{{{ '%.1f' % value }}}}\",\"min\":-{lim},\"max\":{lim},\"step\":0.1,\"mode\":\"box\",\"unit_of_meas\":\"°C\"",
        lim = Tenths(TEMP_OFFSET_LIMIT_DC)
    )?;
    c.config()?;
    c.close()?;

    c.open("temp_c", "sensor", "Temperature")?;
    c.template("temp_c")?;
    c.out.write_str(",\"dev_cla\":\"temperature\",\"unit_of_meas\":\"°C\",\"stat_cla\":\"measurement\"")?;
    c.close()?;

    c.open("light", "sensor", "Light")?;
    c.template("light")?;
    c.out.write_str(",\"unit_of_meas\":\"%\"")?;
    c.icon("brightness-5")?;
    c.close()?;

    c.open("rssi", "sensor", "Signal")?;
    c.template("rssi")?;
    c.out.write_str(",\"dev_cla\":\"signal_strength\",\"unit_of_meas\":\"dBm\"")?;
    c.diagnostic()?;
    c.close()?;

    c.open("uptime", "sensor", "Uptime")?;
    c.template("uptime")?;
    c.out.write_str(",\"dev_cla\":\"duration\",\"unit_of_meas\":\"s\"")?;
    c.diagnostic()?;
    c.close()?;

    c.open("heals", "sensor", "Self-heal resets")?;
    c.template("heals")?;
    c.icon("restart-alert")?;
    c.diagnostic()?;
    c.close()?;

    c.open("ringing", "binary_sensor", "Ringing")?;
    c.on_off("ringing")?;
    c.icon("alarm-light")?;
    c.close()?;

    c.open("play", "button", "Play melody")?;
    c.press(Command::Play)?;
    c.icon("music-note")?;
    c.close()?;

    c.open("silence", "button", "Silence")?;
    c.press(Command::Silence)?;
    c.icon("volume-off")?;
    c.close()?;

    c.open("reload", "button", "Reload")?;
    c.press(Command::Reload)?;
    c.out.write_str(",\"dev_cla\":\"restart\"")?;
    c.diagnostic()?;
    c.close()?;

    out.write_str("}}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::form::{HealStamp, Status, write_json};
    use crate::settings::HOSTNAME_MAX;
    use serde_json::Value;

    fn status() -> Status {
        Status {
            version: "v0.1.0",
            slot: "ota_0",
            mode: "sta",
            uptime_secs: 100,
            heals: 0,
            last_heal: Some(HealStamp { unix_secs: 1_800_000_000, weekday: 2, hour: 23, minute: 59, ago_secs: None }),
            temp_dc: None,
            light: Some(50),
            rssi: Some(-60),
            mqtt: Link::Connected,
            ringing: false,
            local: None,
        }
    }

    #[test]
    fn id_topics_and_routes() {
        let id = DeviceId::from_mac([0x10, 0x20, 0x30, 0xa1, 0xb2, 0xc3]);
        assert_eq!(id.as_str(), "clock-a1b2c3");
        assert_eq!(id.status_topic().as_str(), "esp32clock/clock-a1b2c3/status");
        assert_eq!(id.state_topic().as_str(), "esp32clock/clock-a1b2c3/state");
        assert_eq!(id.set_filter().as_str(), "esp32clock/clock-a1b2c3/set/+");
        assert_eq!(id.cmd_topic().as_str(), "esp32clock/clock-a1b2c3/cmd");
        assert_eq!(id.discovery_topic().as_str(), "homeassistant/device/clock-a1b2c3/config");
        assert_eq!(id.route("esp32clock/clock-a1b2c3/set/color"), Route::Set("color"));
        assert_eq!(id.route("esp32clock/clock-a1b2c3/set/temp_offset"), Route::Set("temp_offset"));
        assert_eq!(id.route("esp32clock/clock-a1b2c3/cmd"), Route::Command);
        assert_eq!(id.route(HA_STATUS_TOPIC), Route::HaStatus);
        for t in [
            "esp32clock/clock-a1b2c3/set/",
            "esp32clock/clock-a1b2c3/set/a/b",
            "esp32clock/clock-ffffff/set/color",
            "esp32clock/clock-a1b2c3/state",
            "esp32clock/clock-a1b2c3",
            "homeassistant/device/clock-a1b2c3/config",
            "",
        ] {
            assert_eq!(id.route(t), Route::Other, "{t}");
        }
        // The longest topics fit the buffer.
        let longest_key = SET_KEYS.iter().max_by_key(|k| k.len()).unwrap();
        let mut t = TopicBuf::new();
        write!(t, "{}/{longest_key}", id.topic("set").as_str()).unwrap();
        assert!(id.discovery_topic().len() <= TopicBuf::new().capacity());

        assert_eq!(Command::parse("play"), Some(Command::Play));
        assert_eq!(Command::parse(" reload\n"), Some(Command::Reload));
        assert_eq!(Command::parse("silence"), Some(Command::Silence));
        assert_eq!(Command::parse("PLAY"), None);
        assert_eq!(Command::parse(""), None);
        for c in Command::ALL {
            assert_eq!(Command::parse(c.name()), Some(c));
        }
    }

    /// The payload at its worst fits the buffer, parses as JSON, and
    /// every entity reads a key the state JSON has and commands a key
    /// the bus takes; every select option is a value the parser takes
    /// and the default state is one of them.
    #[test]
    fn discovery_fits_parses_and_matches_the_state() {
        let id = DeviceId::from_mac([0xff; 6]);
        let name = "h".repeat(HOSTNAME_MAX);
        let version = "v0.1.0-123-gabcdef0-dirty-and-long";
        let d = Device { id: &id, name: &name, ip: Some([255, 255, 255, 255]), version };
        let mut out = String::new();
        write_discovery(&mut out, &d).unwrap();
        println!("discovery payload: {} bytes of {DISCOVERY_MAX}", out.len());
        assert!(out.len() <= DISCOVERY_MAX, "{} bytes", out.len());
        let v: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        assert_eq!(v["dev"]["ids"][0], "clock-ffffff");
        assert_eq!(v["dev"]["name"], name.as_str());
        assert_eq!(v["dev"]["sw"], version);
        assert_eq!(v["dev"]["cu"], "http://255.255.255.255/");
        assert_eq!(v["o"]["name"], "esp32clock");
        assert_eq!(v["stat_t"], "esp32clock/clock-ffffff/state");
        assert_eq!(v["avty_t"], "esp32clock/clock-ffffff/status");
        let cmps = v["cmps"].as_object().unwrap();
        assert_eq!(cmps.len(), 19);

        let mut json = String::new();
        write_json(&mut json, &Settings::DEFAULT, &status()).unwrap();
        let state: Value = serde_json::from_str(&json).unwrap();

        let mut ids = std::collections::HashSet::new();
        let mut commanded = std::collections::HashSet::new();
        for (key, c) in cmps {
            assert!(c["p"].is_string(), "{key}: no platform");
            let uid = c["uniq_id"].as_str().unwrap();
            assert!(ids.insert(uid.to_string()), "{key}: duplicate {uid}");
            assert_eq!(uid, format!("clock-ffffff_{key}"));
            match c["val_tpl"].as_str() {
                Some(tpl) => {
                    let k = tpl.split("value_json.").nth(1).unwrap().split(|ch: char| !(ch.is_alphanumeric() || ch == '_')).next().unwrap();
                    assert!(state.get(k).is_some(), "{key}: the template reads {k}, which the state JSON lacks");
                }
                None => assert_eq!(c["p"], "button", "{key}: only a button has no template"),
            }
            if let Some(cmd) = c["cmd_t"].as_str() {
                match cmd.strip_prefix("esp32clock/clock-ffffff/set/") {
                    Some(k) => {
                        assert!(SET_KEYS.contains(&k), "{key}: commands {k}, not a key the bus takes");
                        commanded.insert(k);
                    }
                    None => {
                        assert_eq!(cmd, "esp32clock/clock-ffffff/cmd", "{key}");
                        assert!(Command::parse(c["pl_prs"].as_str().unwrap()).is_some(), "{key}: unknown press");
                    }
                }
            }
            if let Some(ops) = c["ops"].as_array() {
                let k = c["cmd_t"].as_str().unwrap().rsplit('/').next().unwrap();
                for o in ops {
                    let mut s = Settings::DEFAULT;
                    assert_eq!(apply_set(&mut s, k, o.as_str().unwrap()), Ok(()), "{key}: option {o} refused");
                }
                let shown = match &state[k] {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                assert!(ops.iter().any(|o| o == shown.as_str()), "{key}: the state {shown} is not an option");
            }
        }
        // Everything the bus takes has an entity, and the clock's three
        // commands all have a button.
        for k in SET_KEYS {
            assert!(commanded.contains(k), "{k} has no entity");
        }
        for c in Command::ALL {
            assert!(out.contains(&format!("\"pl_prs\":\"{}\"", c.name())), "{c:?} has no button");
        }
    }

    #[test]
    fn set_takes_the_clocks_keys_and_nothing_else() {
        let mut s = Settings::DEFAULT;
        for (k, v) in [
            ("color", "cyan"),
            ("brightness", "25"),
            ("alarm", "06:30:00"),
            ("armed", "on"),
            ("days", "weekend"),
            ("skip", "on"),
            ("melody", "random"),
            ("twelve_hour", "on"),
            ("temp_show", "f"),
            ("temp_offset", "-0.7"),
        ] {
            assert_eq!(apply_set(&mut s, k, v), Ok(()), "{k}");
        }
        assert!(s.alarm_armed && s.skip_next && s.twelve_hour);
        assert_eq!((s.alarm_hour, s.alarm_minute), (6, 30));
        assert_eq!(apply_set(&mut s, "armed", "off"), Ok(()));
        assert!(!s.alarm_armed);
        assert_eq!(apply_set(&mut s, "color", "pink"), Err(FormError::BadValue));
        for k in ["tz", "hostname", "ssid", "ssid_pick", "password", "token", "mqtt_host", "mqtt_port", "mqtt_user", "mqtt_password", "version", "bogus", ""] {
            assert_eq!(apply_set(&mut s, k, "x"), Err(FormError::Forbidden), "{k}");
        }
        // Every key on the list is one the form knows.
        for k in SET_KEYS {
            let mut t = Settings::DEFAULT;
            assert_ne!(form::apply_one(&mut t, k, "", Allow::NONE), Err(FormError::UnknownKey), "{k}");
        }
    }
}

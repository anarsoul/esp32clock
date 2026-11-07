// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! Build-time configuration.
//!
//! Credentials and the timezone come from environment variables when the
//! firmware is compiled, so nothing secret lives in the tree:
//!
//! ```text
//! WIFI_SSID=home WIFI_PASSWORD=secret TZ_OFFSET_MINUTES=-420 cargo run --release
//! ```
//!
//! `API_TOKEN` is the fourth: what a script must carry for a firmware
//! update, a reset, the Wi-Fi credentials or a new token; a build
//! without one can be given one later from the token page. The
//! credentials and the token are seeds: the first saved record carries
//! them from then on (`settings::Settings::seed`). `MAX_CURRENT_STEP` is
//! the fifth, optional: the ceiling on the panel's drive current, for a
//! supply that cannot carry the top steps.
//!
//! `MQTT_HOST`, `MQTT_PORT`, `MQTT_USER` and `MQTT_PASSWORD` seed the
//! Home Assistant broker the same way; a build without a host has MQTT
//! off until the page turns it on.
//!
//! `build.rs` re-runs the build when any of them changes.

/// Wi-Fi network name. Empty when the variable was not set at build
/// time, in which case a clock with no network in its record raises its
/// own access point to be given one.
pub const WIFI_SSID: &str = match option_env!("WIFI_SSID") {
    Some(s) => s,
    None => "",
};

/// Wi-Fi password. Empty means an open network.
pub const WIFI_PASSWORD: &str = match option_env!("WIFI_PASSWORD") {
    Some(s) => s,
    None => "",
};

/// Token a script must carry, as `Authorization: Bearer <token>`, for
/// `POST /ota`, `/reload`, `/token` and the Wi-Fi credentials on
/// `/api`. Empty when the variable was not set at build time, which
/// closes those paths until a token is set from the token page with
/// the PIN; the boot line says which. Must be empty or 8 to 32
/// printable characters without spaces (`settings::valid_token`,
/// checked at compile time). Compiled in as a string, so anyone with
/// the image file can read it out, the same standing as the Wi-Fi
/// password.
pub const API_TOKEN: &str = match option_env!("API_TOKEN") {
    Some(s) => s,
    None => "",
};

/// The MQTT broker the clock announces itself to for Home Assistant:
/// an IPv4 literal or a DNS name, empty for no MQTT. A seed like the
/// Wi-Fi credentials: the first saved record carries it from then on,
/// and a host cleared on the page stays cleared (`Settings::seed`).
pub const MQTT_HOST: &str = match option_env!("MQTT_HOST") {
    Some(s) => s,
    None => "",
};

/// The broker's port; 1883 is MQTT's plain port and the default.
pub const MQTT_PORT: u16 = match option_env!("MQTT_PORT") {
    Some(s) => match u16::from_str_radix(s, 10) {
        Ok(p) if p > 0 => p,
        _ => panic!("MQTT_PORT must be 1 to 65535"),
    },
    None => 1883,
};

/// The broker user and password; both empty for an anonymous broker.
/// The password is compiled in as a string like the others.
pub const MQTT_USER: &str = match option_env!("MQTT_USER") {
    Some(s) => s,
    None => "",
};
pub const MQTT_PASSWORD: &str = match option_env!("MQTT_PASSWORD") {
    Some(s) => s,
    None => "",
};

/// Password of the clock's own access point, the one it raises when it
/// has no network to join (or both buttons are held 10 s): WPA2, so
/// `AP_PASSWORD` at build time must be 8 to 64 bytes. Documented in
/// AGENTS.md; anyone who can join it can give the clock a network,
/// which is the point.
pub const AP_PASSWORD: &str = match option_env!("AP_PASSWORD") {
    Some(s) => s,
    None => "esp32clock",
};
const _: () = assert!(AP_PASSWORD.len() >= 8 && AP_PASSWORD.len() <= 64, "AP_PASSWORD must be 8 to 64 bytes");

/// The access point's name is this followed by the last two bytes of
/// its MAC in hex, `clock-3f2a`, so two clocks in one house differ.
pub const AP_SSID_PREFIX: &str = "clock-";

/// The access point's address and network (a /24), its channel, and
/// the lease its DHCP server hands out. The page is at
/// `http://192.168.4.1/`.
pub const AP_ADDRESS: [u8; 4] = [192, 168, 4, 1];
pub const AP_PREFIX_LEN: u8 = 24;
pub const AP_CHANNEL: u8 = 1;
pub const AP_LEASE_SECS: u32 = 3600;

/// Boots an updated image gets before the previous slot is booted again.
/// The upload sets the carried boot count to 1; each software reset
/// before the image has been confirmed good (a gateway probe answered or
/// an HTTP request served) bumps it, and this value rolls back. A power
/// cut clears the count and keeps the new image.
pub const OTA_BOOT_LIMIT: u32 = 3;

/// Minutes east of UTC at boot until the user sets it. No daylight saving
/// handling.
pub const DEFAULT_TZ_OFFSET_MINUTES: i16 = match option_env!("TZ_OFFSET_MINUTES") {
    Some(s) => parse_i32(s) as i16,
    None => 0,
};

/// Show 12 hour time with the AM mark instead of 24 hour time, until the
/// user sets it.
pub const DEFAULT_TWELVE_HOUR: bool = false;

/// Index into `settings::PALETTE` at boot. 1 is orange.
pub const DEFAULT_COLOR: usize = 1;

/// Index into `settings::BRIGHTNESS_STEPS` at boot. 2 is 50 %. A ceiling
/// that leaves fewer steps than that lands on the brightest one offered.
pub const DEFAULT_BRIGHTNESS: usize = 2;

/// Alarm time at boot, disarmed. These are only the defaults; the saved
/// settings in flash take precedence once the user has changed them.
pub const DEFAULT_ALARM: (u8, u8) = (7, 0);

/// Ceiling on the current step (1 to 15) whatever the brightness, from
/// `MAX_CURRENT_STEP` at build time. Brightness steps above the one that
/// maps to it are not offered, since they would all draw the same (see
/// `settings::BRIGHTNESS_STEPS`). Step 11 is the stock firmware's
/// "normal" setting, 30 mA per lit sink, and the drivers get hot above
/// it; the firmware ran there until the transmit ceiling went up, after
/// which a quick step to 75 % or 100 % brightness browned the board out
/// (2026-10-22). The default is 5, the 50 % step, about 15 mA per sink:
/// stable, and bright enough. A better supply can raise it.
pub const MAX_CURRENT_STEP: u8 = {
    let step = match option_env!("MAX_CURRENT_STEP") {
        Some(s) => parse_i32(s),
        None => 5,
    };
    assert!(step >= 1 && step <= 15, "MAX_CURRENT_STEP must be 1 to 15");
    step as u8
};

/// Ceiling on Wi-Fi transmit power, in quarter dBm, 8 to 84. esp-radio
/// starts at 20 (5 dBm), which does not reach a far access point. 34
/// (8.5 dBm) worked on the bench but not inside the clock's case, which
/// costs a few dB; 44 (11 dBm) connects and holds, but the access point
/// hears the clock 21 dB below what the clock hears of it, and the real
/// clock sat 7 dB above the AP's noise floor. The driver rounds the
/// setting down to a level: 44 is 11 dBm, 52 is 13, 56 is 14, 60 is 15,
/// 66 to 71 are 16, 72 to 79 are 18, 80 to 84 are 20 (the table in
/// esp-wifi-sys's `esp_wifi_set_max_tx_power` doc), so the boot log's
/// "max tx power" is the setting, not the level. 56 (14 dBm) and 68
/// (16 dBm) each read a full step better at the AP (-59, -55, -52 dBm)
/// with clean connects. 80 (20 dBm) read -46 but the link got worse: the
/// gateway round trip doubled, the first connect needed three attempts
/// and the far AP could not complete an association in 16 tries. The
/// ESP32-C3 SuperMini antenna is mismatched enough that full power
/// radiates louder and decodes worse. 72 (18 dBm) was already past the
/// knee: the near AP read -47 but jumping, the far AP read -73 against
/// -70 at 68, the probe took 70 ms and association took five tries. 68
/// is the ceiling.
pub const WIFI_MAX_TX_POWER_QDBM: i8 = 68;

/// Frames the Wi-Fi driver queues for the network stack before it drops
/// new ones. esp-radio's default of 5 overflowed on bursts of LAN
/// broadcasts (eight frames in 3 ms were seen), which cost the DHCP offer
/// and a 10 s retry at boot. The queue holds references to the driver's
/// own buffers, so a slot costs a few bytes of heap.
pub const WIFI_RX_QUEUE_FRAMES: usize = 32;

/// Consecutive gateway probe failures, with the link still up and after
/// the gateway has answered at least once since boot, before the firmware
/// blanks the panel and resets itself. Probes are 30 s apart, so 10 is
/// five minutes. The first-answer condition keeps a router that never
/// answers ping from rebooting the clock every five minutes.
pub const PROBE_RESET_FAILURES: u32 = 10;

/// The Wi-Fi driver's MTU, `ESP_RADIO_CONFIG_WIFI_MTU` in
/// `.cargo/config.toml` (esp-radio's default is 1492), read here too so
/// the probe follows it. The largest frame the clock sends: the access
/// point hears the clock 20 dB below what the clock hears of it, and a
/// marginal uplink passes short frames long after full ones fail, so the
/// frames are kept short (2026-11-01: 540, a TCP payload of 486). Only
/// what the clock sends is bounded; smoltcp applies the MTU when it
/// emits and the driver hands received frames over whole, so a peer may
/// still send larger UDP or ICMP, while a TCP peer is told the segment
/// size in the handshake and keeps to it.
pub const WIFI_MTU: usize = match option_env!("ESP_RADIO_CONFIG_WIFI_MTU") {
    Some(s) => parse_i32(s) as usize,
    None => 1492,
};

/// Payload of each gateway probe: the largest ping the stack can send
/// at `WIFI_MTU`, after the Ethernet, IP and ICMP headers, so the probe
/// frame is the largest frame the clock ever sends. A smaller ping said
/// "fine" while page segments were failing. `scripts/watch-clock.sh`
/// pings the clock with the same size from the host, and a larger ping
/// from the LAN goes unanswered, since the reply would not fit.
pub const PROBE_PAYLOAD_BYTES: usize = WIFI_MTU - 14 - 20 - 8;
const _: () = assert!(PROBE_PAYLOAD_BYTES >= 64 && WIFI_MTU <= 1492, "ESP_RADIO_CONFIG_WIFI_MTU must be 106 to 1492");

/// Consecutive failed connects after which the station is reset, and
/// the span from the first failure of the run that does the same. The
/// driver avoids the access point that last dropped it and will keep
/// trying another that cannot hear the clock; a reset clears that. It
/// fires whether or not a scan showed a candidate, so with every access
/// point down the clock resets every `CONNECT_RESET_SECS` until one is
/// back, which costs a dark blink and nothing else now that the time is
/// carried across the reset.
pub const CONNECT_RESET_ATTEMPTS: u32 = 10;
pub const CONNECT_RESET_SECS: u64 = 180;

/// NTP server, resolved through the DHCP-supplied DNS.
pub const NTP_SERVER: &str = "pool.ntp.org";

/// Seconds between successful syncs.
pub const NTP_RESYNC_SECS: u64 = 3600;

/// Seconds before retrying after a failed sync.
pub const NTP_RETRY_SECS: u64 = 30;

/// Name sent to the DHCP server until the user sets one.
pub const DEFAULT_HOSTNAME: &str = "clock";

/// TCP port of the settings page.
pub const HTTP_PORT: u16 = 80;

const fn parse_i32(s: &str) -> i32 {
    let b = s.as_bytes();
    let mut i = 0;
    let mut neg = false;
    if !b.is_empty() && (b[0] == b'-' || b[0] == b'+') {
        neg = b[0] == b'-';
        i = 1;
    }
    assert!(i < b.len(), "TZ_OFFSET_MINUTES and MAX_CURRENT_STEP must be integers");
    let mut v: i32 = 0;
    while i < b.len() {
        let c = b[i];
        assert!(c >= b'0' && c <= b'9', "TZ_OFFSET_MINUTES and MAX_CURRENT_STEP must be integers");
        v = v * 10 + (c - b'0') as i32;
        i += 1;
    }
    if neg { -v } else { v }
}

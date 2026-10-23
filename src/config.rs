//! Build-time configuration.
//!
//! Credentials and the timezone come from environment variables when the
//! firmware is compiled, so nothing secret lives in the tree:
//!
//! ```text
//! WIFI_SSID=home WIFI_PASSWORD=secret TZ_OFFSET_MINUTES=-420 cargo run --release
//! ```
//!
//! `OTA_TOKEN` is the fourth: a firmware update over the LAN must carry
//! it, and a build without one has no update path at all.
//!
//! `build.rs` re-runs the build when any of them changes.

/// Wi-Fi network name. Empty when the variable was not set at build time.
pub const WIFI_SSID: &str = match option_env!("WIFI_SSID") {
    Some(s) => s,
    None => "",
};

/// Wi-Fi password. Empty means an open network.
pub const WIFI_PASSWORD: &str = match option_env!("WIFI_PASSWORD") {
    Some(s) => s,
    None => "",
};

/// Token a firmware upload must carry, as `Authorization: Bearer <token>`
/// on `POST /ota`. Empty when the variable was not set at build time,
/// which disables updates over the air: every upload is refused and the
/// boot line says so. Compiled in as a string, so anyone with the image
/// file can read it out, the same standing as the Wi-Fi password.
pub const OTA_TOKEN: &str = match option_env!("OTA_TOKEN") {
    Some(s) => s,
    None => "",
};

/// Shortest token that enables updates. A shorter one counts as none.
pub const OTA_TOKEN_MIN: usize = 8;

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

/// Index into `settings::BRIGHTNESS_STEPS` at boot. 2 is 50 %.
pub const DEFAULT_BRIGHTNESS: usize = 2;

/// Alarm time at boot, disarmed. These are only the defaults; the saved
/// settings in flash take precedence once the user has changed them.
pub const DEFAULT_ALARM: (u8, u8) = (7, 0);

/// Ceiling on the current step whatever the brightness. Step 11 is the
/// stock firmware's "normal" setting, 30 mA per lit sink. The drivers get
/// hot at the top steps with many sinks lit.
pub const MAX_CURRENT_STEP: u8 = 11;

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

/// Payload of each gateway probe. A page is two full-size TCP segments,
/// and a marginal uplink passes short frames long after full ones start
/// failing, so a small ping said "fine" while the page crawled. This
/// makes the probe frame the size of a page segment: 1400 + 28 bytes of
/// headers, under the driver's 1492-byte MTU with margin.
pub const PROBE_PAYLOAD_BYTES: usize = 1400;

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
    assert!(i < b.len(), "TZ_OFFSET_MINUTES must be an integer");
    let mut v: i32 = 0;
    while i < b.len() {
        let c = b[i];
        assert!(c >= b'0' && c <= b'9', "TZ_OFFSET_MINUTES must be an integer");
        v = v * 10 + (c - b'0') as i32;
        i += 1;
    }
    if neg { -v } else { v }
}

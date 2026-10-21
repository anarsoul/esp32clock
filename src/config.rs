//! Build-time configuration.
//!
//! Credentials and the timezone come from environment variables when the
//! firmware is compiled, so nothing secret lives in the tree:
//!
//! ```text
//! WIFI_SSID=home WIFI_PASSWORD=secret TZ_OFFSET_MINUTES=-420 cargo run --release
//! ```
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

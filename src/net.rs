//! Wi-Fi station, network stack and SNTP sync.

use core::net::{IpAddr, SocketAddr};
use core::sync::atomic::{AtomicU32, Ordering};

use embassy_net::dns::DnsQueryType;
use embassy_net::icmp::ping::{PingManager, PingParams};
use embassy_net::icmp::PacketMetadata as IcmpPacketMetadata;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{ConfigV4, DhcpConfig, IpAddress, Runner, Stack};
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_radio::wifi::scan::{ScanConfig, ScanTypeConfig};
use esp_radio::wifi::sta::{ScanMethod, StationConfig};
use esp_radio::wifi::{AuthenticationMethodConfig, Config, Interface, WifiController};
use log::{error, info, warn};
use sntpc::{NtpContext, get_time};
use sntpc_net_embassy::UdpSocketWrapper;
use sntpc_time_embassy::EmbassyTimestampGenerator;

use crate::settings::Hostname;
use crate::{clock, config, heal, settings};

const NTP_PORT: u16 = 123;
const LOCAL_PORT: u16 = 12345;
const NTP_TIMEOUT: Duration = Duration::from_secs(5);
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// How often the station logs its signal while connected.
const STATUS_PERIOD: Duration = Duration::from_secs(60);

/// How often the gateway is pinged while the stack has an address, and
/// how long one reply may take. The probe is the only thing that can
/// tell a station the driver still calls connected from one that
/// actually passes traffic.
const PROBE_PERIOD: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Wait after the driver reports the station connected before DHCP
/// starts. The driver says connected before it can pass a frame, so a
/// discover sent at that moment is lost and smoltcp waits its full
/// `discover_timeout`, 10 s, before the next. Half a second was found
/// enough on another project with the same driver.
const LINK_SETTLE: Duration = Duration::from_millis(500);

/// Port of the throwaway socket in `poke`.
const POKE_PORT: u16 = 12346;

/// Quiet time after a hostname change before DHCP is renewed. The renewal
/// drops the address until the new lease arrives, which resets any open
/// connection, so the page's response and the redirect that follows it
/// must be done first.
const RENEW_SETTLE: Duration = Duration::from_secs(3);

/// A MAC address for the log.
struct Bssid([u8; 6]);

impl core::fmt::Display for Bssid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let b = self.0;
        write!(f, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", b[0], b[1], b[2], b[3], b[4], b[5])
    }
}

/// Build the station configuration from the build-time credentials.
///
/// The scan method matters with more than one access point on the SSID.
/// The driver's default, `Fast`, takes the first beacon it hears on any
/// channel, so it would happily pick a far AP at -82 dBm and fail to
/// authenticate. `AllChannels` scans every channel first and, with the
/// by-signal sort order esp-radio sets, connects to the strongest. It
/// costs a second or two more at connect time.
fn station_config() -> Result<Config, esp_radio::wifi::WifiError> {
    let ssid = config::WIFI_SSID.try_into()?;
    let auth = if config::WIFI_PASSWORD.is_empty() {
        AuthenticationMethodConfig::Open
    } else {
        AuthenticationMethodConfig::Wpa2Personal(config::WIFI_PASSWORD.try_into()?)
    };
    Ok(Config::Station(
        StationConfig::default()
            .with_ssid(ssid)
            .with_authentication(auth)
            .with_scan_method(ScanMethod::AllChannels),
    ))
}

/// Log every access point on our SSID with its channel and signal, so a
/// failed connect can be read against the choice the driver had. Costs a
/// scan of about two seconds before each connect: the default dwell of
/// 10 to 20 ms per channel missed every AP but a strong one, since a far
/// AP's probe response takes longer than that to arrive; ESP-IDF's own
/// default is 120 ms.
async fn log_candidates(controller: &mut WifiController<'static>) {
    let Ok(ssid) = config::WIFI_SSID.try_into() else { return };
    let scan = ScanConfig::default().with_ssid(ssid).with_scan_type(ScanTypeConfig::Active {
        min: esp_hal::time::Duration::from_millis(120),
        max: esp_hal::time::Duration::from_millis(200),
    });
    match controller.scan_async(&scan).await {
        Ok(aps) if aps.is_empty() => warn!("wifi: scan found no AP for {}", config::WIFI_SSID),
        Ok(aps) => {
            for ap in aps {
                info!("wifi: candidate {} channel {} rssi {}", Bssid(ap.bssid), ap.channel, ap.signal_strength);
            }
        }
        Err(e) => warn!("wifi: scan failed: {e:?}"),
    }
}

/// The signal and the AP as the driver sees them, once a minute while
/// connected. Both come from the last beacon received, so a station that
/// has stopped hearing its AP shows a stale RSSI here and a failing
/// gateway probe at the same time.
fn log_status(controller: &WifiController<'static>, since: Instant) {
    let up = since.elapsed().as_secs();
    match (controller.rssi(), controller.ap_info()) {
        (Ok(rssi), Ok(ap)) => {
            info!("wifi: up {up} s, rssi {rssi} dBm, bssid {} channel {}", Bssid(ap.bssid), ap.channel)
        }
        (rssi, ap) => warn!("wifi: up {up} s, rssi {rssi:?}, ap {ap:?}"),
    }
}

/// Keep the station connected, and run DHCP only while it is. The stack
/// starts with no IPv4 configuration; it is switched on `LINK_SETTLE`
/// after each connect and off at each disconnect, so the first discover
/// of every connection goes out on a link that can carry it. A run of
/// `config::CONNECT_RESET_ATTEMPTS` failed connects, or one lasting
/// `config::CONNECT_RESET_SECS`, resets the chip through `heal::reset`.
#[embassy_executor::task]
pub async fn wifi_task(mut controller: WifiController<'static>, stack: Stack<'static>) -> ! {
    if config::WIFI_SSID.is_empty() {
        error!("wifi: WIFI_SSID was not set at build time, staying offline");
        loop {
            Timer::after(Duration::from_secs(3600)).await;
        }
    }

    let cfg = match station_config() {
        Ok(c) => c,
        Err(e) => {
            error!("wifi: bad credentials: {e:?}");
            loop {
                Timer::after(Duration::from_secs(3600)).await;
            }
        }
    };

    match controller.set_max_tx_power(config::WIFI_MAX_TX_POWER_QDBM) {
        Ok(()) => info!("wifi: max tx power {} dBm", config::WIFI_MAX_TX_POWER_QDBM as f32 / 4.0),
        Err(e) => warn!("wifi: set_max_tx_power: {e:?}"),
    }

    // Failed connects in a row, and when the run of them started. The
    // driver avoids the access point that last dropped it and can keep
    // trying another one that cannot hear the clock; only a reset clears
    // that, so a run that reaches either limit ends in `heal::reset`.
    let mut failures: u32 = 0;
    let mut first_failure: Option<Instant> = None;
    loop {
        if let Err(e) = controller.set_config(&cfg) {
            error!("wifi: set_config: {e:?}");
            Timer::after(RECONNECT_DELAY).await;
            continue;
        }
        log_candidates(&mut controller).await;
        info!("wifi: connecting to {}", config::WIFI_SSID);
        match controller.connect_async().await {
            Ok(info) => {
                info!("wifi: connected to {} channel {}", Bssid(info.bssid), info.channel);
                failures = 0;
                first_failure = None;
            }
            Err(e) => {
                failures += 1;
                let since = first_failure.get_or_insert_with(Instant::now).elapsed();
                warn!(
                    "wifi: connect failed: {e:?} (attempt {failures} of {}, {} s of {})",
                    config::CONNECT_RESET_ATTEMPTS,
                    since.as_secs(),
                    config::CONNECT_RESET_SECS
                );
                if failures >= config::CONNECT_RESET_ATTEMPTS || since.as_secs() >= config::CONNECT_RESET_SECS {
                    heal::reset(format_args!("no connection after {failures} attempts over {} s", since.as_secs())).await
                }
                Timer::after(RECONNECT_DELAY).await;
                continue;
            }
        }
        let since = Instant::now();
        Timer::after(LINK_SETTLE).await;
        start_dhcp(stack);
        loop {
            // The disconnect wait subscribes to the driver's events anew
            // each time, so an event that lands while the status line is
            // being written is missed; the next wait then sees the station
            // already disconnected and returns the error at once.
            match select(controller.wait_for_disconnect_async(), Timer::after(STATUS_PERIOD)).await {
                Either::First(Ok(info)) => {
                    warn!(
                        "wifi: disconnected from {} after {} s: {:?}, rssi {}",
                        Bssid(info.bssid),
                        since.elapsed().as_secs(),
                        info.reason,
                        info.rssi
                    );
                    break;
                }
                Either::First(Err(e)) => {
                    warn!("wifi: disconnect wait failed after {} s: {e:?}", since.elapsed().as_secs());
                    break;
                }
                Either::Second(()) => log_status(&controller, since),
            }
        }
        stop_dhcp(stack);
        Timer::after(RECONNECT_DELAY).await;
    }
}

/// Start DHCP with the current hostname. The runner is poked because
/// embassy-net does not wake it for a configuration change, and with
/// nothing else pending it would send the discover only when the next
/// packet happened to arrive.
fn start_dhcp(stack: Stack<'static>) {
    stack.set_config_v4(dhcp_config(&settings::get().hostname));
    poke(stack);
}

/// Drop DHCP and the address with it.
fn stop_dhcp(stack: Stack<'static>) {
    stack.set_config_v4(ConfigV4::None);
}

/// Wake the stack runner. Any socket operation does; a bind on a
/// throwaway UDP socket is the cheapest.
fn poke(stack: Stack<'static>) {
    let mut rx_meta = [PacketMetadata::EMPTY; 1];
    let mut rx_buf = [0u8; 1];
    let mut tx_meta = [PacketMetadata::EMPTY; 1];
    let mut tx_buf = [0u8; 1];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    let _ = socket.bind(POKE_PORT);
}

/// Drive the network stack.
#[embassy_executor::task]
pub async fn net_task(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
}

/// Log every change of the link and of the address, so a lost lease
/// with the link still up, or a link that bounced, is on record with
/// its time.
#[embassy_executor::task]
pub async fn monitor_task(stack: Stack<'static>) -> ! {
    loop {
        stack.wait_link_up().await;
        info!("net: link up");
        loop {
            stack.wait_config_up().await;
            if let Some(cfg) = stack.config_v4() {
                info!("net: ip {} gateway {:?} dns {:?}", cfg.address, cfg.gateway, cfg.dns_servers);
            }
            match select(stack.wait_link_down(), stack.wait_config_down()).await {
                Either::First(()) => {
                    warn!("net: link down");
                    break;
                }
                Either::Second(()) => warn!("net: config down, link still up"),
            }
        }
    }
}

// Written by `probe_task` alone and read by the heartbeat; the target has
// no atomic add, so they are bumped with a load and a store.
static PROBE_SENT: AtomicU32 = AtomicU32::new(0);
static PROBE_FAILED: AtomicU32 = AtomicU32::new(0);
static PROBE_STREAK: AtomicU32 = AtomicU32::new(0);

/// Gateway probes so far: sent, failed, and failed in a row right now.
fn bump(counter: &AtomicU32) -> u32 {
    let v = counter.load(Ordering::Relaxed).wrapping_add(1);
    counter.store(v, Ordering::Relaxed);
    v
}

pub fn probe_stats() -> (u32, u32, u32) {
    (
        PROBE_SENT.load(Ordering::Relaxed),
        PROBE_FAILED.load(Ordering::Relaxed),
        PROBE_STREAK.load(Ordering::Relaxed),
    )
}

/// Room for one probe packet, request or reply: the payload, the 8-byte
/// ICMP header and slack. A reply that does not fit is dropped as
/// truncated, which would read as a failure.
const PROBE_BUF: usize = config::PROBE_PAYLOAD_BYTES + 64;

/// The probe payload; a pattern, the content does not matter. Static so
/// the task's future stays small.
static PROBE_PAYLOAD: [u8; config::PROBE_PAYLOAD_BYTES] = [0x5a; config::PROBE_PAYLOAD_BYTES];

/// Ping the gateway every `PROBE_PERIOD` while the stack has an address,
/// with a full-size frame (`config::PROBE_PAYLOAD_BYTES`) so the probe
/// fails when page-sized frames fail, not only when everything does.
/// Logs each failure and the recovery; the counts go in the heartbeat.
/// A router that never answers ping shows up as a failure every period
/// from the first minute, which is its own answer.
///
/// Self-heal: once the gateway has answered at least once, a run of
/// `config::PROBE_RESET_FAILURES` failures while the driver still reports
/// the link up means the station is connected but passes no traffic, and
/// the only thing known to clear that is a reset (`heal::reset`). With
/// the link down nothing is done: the Wi-Fi task is already reconnecting.
#[embassy_executor::task]
pub async fn probe_task(stack: Stack<'static>) -> ! {
    let mut armed = false;
    let mut rx_meta = [IcmpPacketMetadata::EMPTY; 2];
    let mut rx_buf = [0u8; PROBE_BUF];
    let mut tx_meta = [IcmpPacketMetadata::EMPTY; 2];
    let mut tx_buf = [0u8; PROBE_BUF];
    let mut pinger = PingManager::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    loop {
        stack.wait_config_up().await;
        let Some(gateway) = stack.config_v4().and_then(|c| c.gateway) else {
            Timer::after(PROBE_PERIOD).await;
            continue;
        };
        let mut params = PingParams::new(gateway);
        params
            .set_payload(&PROBE_PAYLOAD)
            .set_count(1)
            .set_timeout(PROBE_TIMEOUT)
            .set_rate_limit(Duration::from_millis(0));
        bump(&PROBE_SENT);
        match pinger.ping(&params).await {
            Ok(rtt) => {
                let streak = PROBE_STREAK.load(Ordering::Relaxed);
                PROBE_STREAK.store(0, Ordering::Relaxed);
                if streak > 0 {
                    info!("probe: gateway {gateway} back after {streak} failures, rtt {} ms", rtt.as_millis());
                }
                if !armed {
                    armed = true;
                    info!(
                        "probe: gateway {gateway} answers a {}-byte ping in {} ms; self-heal armed at {} failures",
                        config::PROBE_PAYLOAD_BYTES,
                        rtt.as_millis(),
                        config::PROBE_RESET_FAILURES
                    );
                }
            }
            Err(e) => {
                bump(&PROBE_FAILED);
                let streak = bump(&PROBE_STREAK);
                warn!("probe: gateway {gateway} unreachable: {e:?} ({streak} in a row)");
                if armed && streak >= config::PROBE_RESET_FAILURES && stack.is_link_up() {
                    heal::reset(format_args!(
                        "gateway {gateway} unreachable for {streak} probes ({} s) with the link up",
                        streak as u64 * PROBE_PERIOD.as_secs()
                    ))
                    .await
                }
            }
        }
        Timer::after(PROBE_PERIOD).await;
    }
}

/// DHCP with our name as option 12, so a router that publishes DHCP
/// names in its DNS resolves it.
fn dhcp_config(hostname: &Hostname) -> ConfigV4 {
    let mut cfg = DhcpConfig::default();
    // The hostname is validated shorter than the option, so this cannot fail.
    cfg.hostname = heapless::String::try_from(hostname.as_str()).ok();
    ConfigV4::Dhcp(cfg)
}

/// Re-run DHCP with the new name once the hostname setting has changed
/// and stayed put for `RENEW_SETTLE`. The lease restarts; the address
/// normally comes back the same. With the link down there is nothing to
/// renew: the next connect starts DHCP with the current name anyway.
#[embassy_executor::task]
pub async fn hostname_task(stack: Stack<'static>) -> ! {
    let mut rx = settings::receiver();
    let mut current = settings::get().hostname;
    loop {
        let mut next = rx.changed().await.hostname;
        loop {
            match select(Timer::after(RENEW_SETTLE), rx.changed()).await {
                Either::First(()) => break,
                Either::Second(s) => next = s.hostname,
            }
        }
        if next != current {
            current = next;
            if stack.is_link_up() {
                info!("net: hostname changed to {}, renewing dhcp", current.as_str());
                start_dhcp(stack);
            }
        }
    }
}

/// One SNTP exchange. Returns the Unix time in microseconds as of return.
async fn query_ntp(stack: Stack<'static>) -> Result<i64, &'static str> {
    let addrs = with_timeout(NTP_TIMEOUT, stack.dns_query(config::NTP_SERVER, DnsQueryType::A))
        .await
        .map_err(|_| "dns timeout")?
        .map_err(|_| "dns failed")?;
    let ip = match addrs.first() {
        Some(IpAddress::Ipv4(v4)) => IpAddr::V4(*v4),
        _ => return Err("no A record"),
    };

    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 256];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; 256];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    socket.bind(LOCAL_PORT).map_err(|_| "bind failed")?;
    let socket = UdpSocketWrapper::new(socket);

    let context = NtpContext::new(EmbassyTimestampGenerator::default());
    let result = with_timeout(NTP_TIMEOUT, get_time(SocketAddr::new(ip, NTP_PORT), &socket, context))
        .await
        .map_err(|_| "ntp timeout")?
        .map_err(|_| "ntp failed")?;
    let received_at = Instant::now();

    // Server transmit time plus half the round trip is our best estimate of
    // the time the reply arrived. The fraction is 2^-32 seconds.
    let frac_us = (result.seconds_fraction as u64 * 1_000_000) >> 32;
    let unix_us = result.seconds as i64 * 1_000_000 + frac_us as i64 + (result.roundtrip / 2) as i64;
    let elapsed = Instant::now().duration_since(received_at).as_micros() as i64;
    info!(
        "ntp: {} stratum {}, roundtrip {} us, offset {} us",
        config::NTP_SERVER,
        result.stratum,
        result.roundtrip,
        result.offset
    );
    Ok(unix_us + elapsed)
}

/// Sync the clock periodically.
#[embassy_executor::task]
pub async fn ntp_task(stack: Stack<'static>) -> ! {
    loop {
        stack.wait_config_up().await;
        match query_ntp(stack).await {
            Ok(unix_us) => {
                clock::set_unix_micros(unix_us);
                if let Some(t) = clock::now() {
                    info!("clock: synced, local time {:02}:{:02}:{:02}", t.hour, t.minute, t.second);
                }
                Timer::after(Duration::from_secs(config::NTP_RESYNC_SECS)).await;
            }
            Err(e) => {
                warn!("ntp: {e}");
                Timer::after(Duration::from_secs(config::NTP_RETRY_SECS)).await;
            }
        }
    }
}

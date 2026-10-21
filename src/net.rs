//! Wi-Fi station, network stack and SNTP sync.

use core::net::{IpAddr, SocketAddr};

use embassy_net::dns::DnsQueryType;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, Runner, Stack};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{AuthenticationMethodConfig, Config, Interface, WifiController};
use log::{error, info, warn};
use sntpc::{NtpContext, get_time};
use sntpc_net_embassy::UdpSocketWrapper;
use sntpc_time_embassy::EmbassyTimestampGenerator;

use crate::{clock, config};

const NTP_PORT: u16 = 123;
const LOCAL_PORT: u16 = 12345;
const NTP_TIMEOUT: Duration = Duration::from_secs(5);
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Build the station configuration from the build-time credentials.
fn station_config() -> Result<Config, esp_radio::wifi::WifiError> {
    let ssid = config::WIFI_SSID.try_into()?;
    let auth = if config::WIFI_PASSWORD.is_empty() {
        AuthenticationMethodConfig::Open
    } else {
        AuthenticationMethodConfig::Wpa2Personal(config::WIFI_PASSWORD.try_into()?)
    };
    Ok(Config::Station(
        StationConfig::default().with_ssid(ssid).with_authentication(auth),
    ))
}

/// Keep the station connected. Reconnects after any disconnect.
#[embassy_executor::task]
pub async fn wifi_task(mut controller: WifiController<'static>) -> ! {
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

    loop {
        if let Err(e) = controller.set_config(&cfg) {
            error!("wifi: set_config: {e:?}");
            Timer::after(RECONNECT_DELAY).await;
            continue;
        }
        info!("wifi: connecting to {}", config::WIFI_SSID);
        match controller.connect_async().await {
            Ok(info) => info!("wifi: connected, channel {}", info.channel),
            Err(e) => {
                warn!("wifi: connect failed: {e:?}");
                Timer::after(RECONNECT_DELAY).await;
                continue;
            }
        }
        match controller.wait_for_disconnect_async().await {
            Ok(info) => warn!("wifi: disconnected: {:?}", info.reason),
            Err(e) => warn!("wifi: disconnect wait failed: {e:?}"),
        }
        Timer::after(RECONNECT_DELAY).await;
    }
}

/// Drive the network stack.
#[embassy_executor::task]
pub async fn net_task(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
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
        if let Some(cfg) = stack.config_v4() {
            info!("net: ip {}", cfg.address);
        }
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

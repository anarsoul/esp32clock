// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! Home Assistant over MQTT: the task that holds the connection to the
//! broker in the settings, announces the clock with a discovery payload,
//! publishes the `/api` JSON as the state and takes commands on
//! `set/<key>` and `cmd`. The topics, the payload and the command rules
//! are the pure half in `esp32clock_core::mqtt`, re-exported here.
//!
//! `rust-mqtt` does the protocol and nothing else; this task is the
//! connection management: resolve, connect, keep alive, reconnect with
//! a growing back-off, and start over when the broker setting changes.
//! Everything is QoS 0 with a clean session, so there is nothing to
//! recover across a connection. `poll_header` is cancel-safe and is the
//! one client future the loop selects on; a header is followed by
//! `poll_body` to completion, and every publish happens between polls,
//! so the client never has two futures at once.
//!
//! Before a deliberate reset, `goodbye` lets the task publish `offline`
//! and disconnect, so Home Assistant shows the clock unavailable at
//! once rather than after the broker's keepalive timeout.

use core::cell::Cell;
use core::net::Ipv4Addr;
use core::num::NonZero;
use core::sync::atomic::{AtomicU32, Ordering};

use critical_section::Mutex as CsMutex;
use embassy_futures::select::{Either, Either4, select, select4};
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use log::{info, warn};
use rust_mqtt::Bytes;
use rust_mqtt::buffer::BumpBuffer;
use rust_mqtt::client::event::Event;
use rust_mqtt::client::options::{
    ConnectOptions, DisconnectOptions, PublicationOptions, RetainHandling, SubscriptionOptions, TopicReference, WillOptions,
};
use rust_mqtt::client::{Client, MqttError};
use rust_mqtt::config::KeepAlive;
use rust_mqtt::types::{MqttBinary, MqttString, PacketIdentifier, ReasonCode, TopicFilter, TopicName};

pub use esp32clock_core::mqtt::{Command, DeviceId, Link, Route};
use esp32clock_core::mqtt::{self as core_mqtt, DISCOVERY_MAX, Device, HA_ONLINE, OFFLINE, ONLINE};

use crate::form::{self, FormError};
use crate::settings::{self, Broker};
use crate::{buzzer, display, heal, http, ota};

/// How long a DNS query or a TCP or MQTT connect may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The keepalive the clock asks for, and when it pings on its own: a
/// state publish every `STATE_PERIOD` usually makes the ping moot.
const KEEP_ALIVE_SECS: u16 = 60;
const PING_AFTER: Duration = Duration::from_secs(45);
const PING_TIMEOUT: Duration = Duration::from_secs(10);
/// The subscriptions must be acknowledged within this after the connect,
/// or the session fails; `subscribe` only sends, and a refusal comes
/// back as a SUBACK with a reason code, not as an error.
const SUBACK_TIMEOUT: Duration = Duration::from_secs(10);

/// The socket's own inactivity timeout, a safety net behind the pings.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(2 * KEEP_ALIVE_SECS as u64);

/// The state goes out this often whatever changed, for the sensors, the
/// signal and the uptime.
const STATE_PERIOD: Duration = Duration::from_secs(60);

/// A run of settings changes is one state message.
const STATE_SETTLE: Duration = Duration::from_millis(300);

/// Home Assistant asks every device to wait a random while before
/// answering its birth message with discovery.
const BIRTH_DELAY_MAX_MS: u64 = 2000;

/// Reconnect back-off: from the first to the last, doubling.
const BACKOFF_MIN: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// How long a deliberate reset waits for the goodbye.
const GOODBYE_WAIT: Duration = Duration::from_millis(500);

/// Socket buffers: commands are short, the discovery payload goes out.
const RX_BUF: usize = 1024;
const TX_BUF: usize = 2048;

/// Where received topics and payloads land: a command is a few dozen
/// bytes, a retained state message the broker may replay is under 1 KB.
const BUMP_BUF: usize = 1024;

/// The state JSON; `/api` is about 600 bytes.
const STATE_BUF: usize = 1024;

/// What the task is doing, for the page and the JSON.
static LINK: CsMutex<Cell<Link>> = CsMutex::new(Cell::new(Link::Off));

pub fn link() -> Link {
    critical_section::with(|cs| LINK.borrow(cs).get())
}

fn set_link(link: Link) {
    critical_section::with(|cs| LINK.borrow(cs).set(link));
}

// Counters for the heartbeat, one writer each.
static CONNECTS: AtomicU32 = AtomicU32::new(0);
static PUBLISHES: AtomicU32 = AtomicU32::new(0);
static COMMANDS: AtomicU32 = AtomicU32::new(0);

fn bump(counter: &AtomicU32) {
    counter.store(counter.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
}

/// The heartbeat's `mqtt:` line.
pub struct Report;

impl core::fmt::Display for Report {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{} ({} connects, {} published, {} commands)",
            link().name(),
            CONNECTS.load(Ordering::Relaxed),
            PUBLISHES.load(Ordering::Relaxed),
            COMMANDS.load(Ordering::Relaxed)
        )
    }
}

/// A deliberate reset asks the task to say goodbye; the task answers
/// once the DISCONNECT is out.
static GOODBYE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static GOODBYE_DONE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Publish `offline` and disconnect before a reset, within
/// `GOODBYE_WAIT`. Returns at once when there is no connection.
pub async fn goodbye() {
    if link() != Link::Connected {
        return;
    }
    GOODBYE.signal(());
    if with_timeout(GOODBYE_WAIT, GOODBYE_DONE.wait()).await.is_err() {
        warn!("mqtt: no goodbye within {} ms, resetting anyway", GOODBYE_WAIT.as_millis());
    }
}

/// Why a session ended.
enum Outcome {
    /// The broker setting changed or was cleared: start over at once.
    Settings,
    /// A failure, with its reason and whether the connection had been
    /// up, which resets the back-off.
    Failed { reason: &'static str, was_up: bool },
    /// The goodbye was said; the chip is about to reset.
    Goodbye,
}

/// Hold a connection to the broker in the settings, for as long as
/// there is one. Spawned on the station only.
#[embassy_executor::task]
pub async fn mqtt_task(stack: Stack<'static>, id: DeviceId) -> ! {
    let mut rx = settings::receiver();
    let mut backoff = BACKOFF_MIN;
    let mut last_error = None;
    loop {
        let broker = settings::get().mqtt;
        if broker.is_none() {
            set_link(Link::Off);
            info!("mqtt: off, no broker stored");
            while rx.changed().await.mqtt.is_none() {}
            last_error = None;
            backoff = BACKOFF_MIN;
            continue;
        }
        set_link(Link::Connecting { last_error });
        stack.wait_config_up().await;
        if settings::get().mqtt != broker {
            continue;
        }
        match session(stack, &id, &broker, &mut rx).await {
            Outcome::Settings => {
                backoff = BACKOFF_MIN;
                last_error = None;
            }
            Outcome::Failed { reason, was_up } => {
                warn!("mqtt: {reason}; retrying in {} s", backoff.as_secs());
                last_error = Some(reason);
                set_link(Link::Connecting { last_error });
                if was_up {
                    backoff = BACKOFF_MIN;
                }
                // A new broker setting cuts the wait short; anything
                // else changing does not.
                let mut wait = Timer::after(backoff);
                let changed = loop {
                    match select(&mut wait, rx.changed()).await {
                        Either::First(()) => break false,
                        Either::Second(s) if s.mqtt != broker => break true,
                        Either::Second(_) => {}
                    }
                };
                if changed {
                    backoff = BACKOFF_MIN;
                } else {
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                }
            }
            Outcome::Goodbye => {
                set_link(Link::Off);
                GOODBYE_DONE.signal(());
                // The reset follows; nothing more to do.
                loop {
                    Timer::after(Duration::from_secs(3600)).await;
                }
            }
        }
    }
}

/// The broker's address: an IPv4 literal as it is, a name through DNS.
async fn resolve(stack: Stack<'static>, host: &str) -> Result<IpAddress, &'static str> {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Ok(IpAddress::Ipv4(ip));
    }
    let addrs = with_timeout(CONNECT_TIMEOUT, stack.dns_query(host, DnsQueryType::A))
        .await
        .map_err(|_| "dns timed out")?
        .map_err(|_| "dns failed")?;
    addrs.first().copied().ok_or("no A record for the broker")
}

/// The client's error as a few words for the log and the page.
fn describe<const N: usize>(e: &MqttError<'_, N>) -> &'static str {
    match e {
        MqttError::Network(_) => "connection lost",
        MqttError::Server => "protocol error from the broker",
        MqttError::Alloc => "packet too large for the buffer",
        MqttError::Disconnect { reason, .. } => match reason {
            ReasonCode::BadUserNameOrPassword => "bad user name or password",
            ReasonCode::NotAuthorized => "not authorized",
            ReasonCode::ServerUnavailable => "broker unavailable",
            ReasonCode::ServerBusy => "broker busy",
            ReasonCode::Banned => "banned by the broker",
            ReasonCode::KeepAliveTimeout => "keepalive timeout at the broker",
            ReasonCode::SessionTakenOver => "session taken over: another clock with this id?",
            _ => "refused by the broker",
        },
        _ => "client error",
    }
}

/// Three subscriptions in flight at most (`set/+`, `cmd`, Home
/// Assistant's status), one QoS 1 publication, no aliases or properties.
type MqttClient<'a, 'c> = Client<'a, 'c, TcpSocket<'c>, BumpBuffer<'c>, 3, 1, 1, 0, 0, 0, 0>;

/// Publish `payload` on `topic` at QoS 0.
async fn publish(client: &mut MqttClient<'_, '_>, topic: &str, payload: &[u8], retain: bool) -> Result<(), &'static str> {
    let name = TopicName::new(MqttString::from_str(topic).map_err(|_| "bad topic")?).ok_or("bad topic")?;
    let mut options = PublicationOptions::new(TopicReference::Name(name));
    if retain {
        options = options.retain();
    }
    client.publish(&options, Bytes::Borrowed(payload)).await.map_err(|e| describe(&e))?;
    bump(&PUBLISHES);
    Ok(())
}

/// Subscribe at QoS 0. `retained` says whether a retained message on
/// the filter is wanted at once: Home Assistant's birth message may be
/// retained, and a replay would make every connect resend discovery
/// twice. Returns the packet identifier the SUBACK will carry; this only
/// sends the SUBSCRIBE.
async fn subscribe(client: &mut MqttClient<'_, '_>, filter: &str, retained: bool) -> Result<PacketIdentifier, &'static str> {
    let filter = TopicFilter::new(MqttString::from_str(filter).map_err(|_| "bad filter")?).ok_or("bad filter")?;
    let mut options = SubscriptionOptions::new();
    if !retained {
        options = options.retain_handling(RetainHandling::NeverSend);
    }
    client.subscribe(filter, &options).await.map_err(|e| describe(&e))
}

/// The `/api` JSON, as the state message.
fn state_json(out: &mut heapless::String<STATE_BUF>) -> Result<(), &'static str> {
    out.clear();
    form::write_json(out, &settings::get(), &http::status()).map_err(|_| "state JSON does not fit")
}

/// The discovery payload for the clock as it is now.
fn discovery_json(out: &mut heapless::String<DISCOVERY_MAX>, stack: Stack<'static>, id: &DeviceId) -> Result<(), &'static str> {
    out.clear();
    let hostname = settings::get().hostname;
    let device = Device {
        id,
        name: hostname.as_str(),
        ip: stack.config_v4().map(|c| c.address.address().octets()),
        version: ota::VERSION,
    };
    core_mqtt::write_discovery(out, &device).map_err(|_| "discovery payload does not fit")
}

/// Random milliseconds up to `BIRTH_DELAY_MAX_MS`.
fn birth_delay() -> Duration {
    Duration::from_millis(esp_hal::rng::Rng::new().random() as u64 % BIRTH_DELAY_MAX_MS)
}

/// Act on a publication the clock is subscribed to. `true` when the
/// discovery should be resent (Home Assistant's birth).
async fn incoming(id: &DeviceId, topic: &str, payload: &[u8]) -> bool {
    let Ok(payload) = core::str::from_utf8(payload) else {
        warn!("mqtt: {topic}: payload is not UTF-8");
        return false;
    };
    match id.route(topic) {
        Route::Set(key) => {
            bump(&COMMANDS);
            let mut trial = settings::get();
            match core_mqtt::apply_set(&mut trial, key, payload) {
                Ok(()) => {
                    info!("mqtt: set {key}={payload}");
                    settings::update(|s| *s = trial);
                }
                Err(FormError::Forbidden) => warn!("mqtt: set/{key} is not a key the bus takes"),
                Err(e) => warn!("mqtt: set {key}={payload} refused: {e:?}"),
            }
            false
        }
        Route::Command => {
            bump(&COMMANDS);
            match Command::parse(payload) {
                Some(Command::Play) => {
                    info!("mqtt: play");
                    buzzer::preview(settings::get().melody);
                }
                Some(Command::Silence) => {
                    info!("mqtt: silence");
                    display::silence();
                }
                Some(Command::Reload) => {
                    // The reset says its own goodbye; the caller does it.
                    warn!("mqtt: reload asked for");
                    RELOAD.signal(());
                }
                None => warn!("mqtt: cmd {payload:?} unknown"),
            }
            false
        }
        Route::HaStatus => {
            if payload == HA_ONLINE {
                info!("mqtt: Home Assistant is up, resending discovery");
                true
            } else {
                false
            }
        }
        Route::Other => {
            warn!("mqtt: unexpected topic {topic}");
            false
        }
    }
}

/// A `reload` command, acted on by the session loop after the event is
/// handled, so the goodbye goes out on this very connection.
static RELOAD: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// One connection, from resolve to the first failure.
async fn session(stack: Stack<'static>, id: &DeviceId, broker: &Broker, rx: &mut settings::SettingsReceiver) -> Outcome {
    let failed = |reason| Outcome::Failed { reason, was_up: false };
    let addr = match resolve(stack, broker.host.as_str()).await {
        Ok(a) => a,
        Err(e) => return failed(e),
    };

    let mut rx_buf = [0u8; RX_BUF];
    let mut tx_buf = [0u8; TX_BUF];
    let mut socket = TcpSocket::new(stack, &mut rx_buf, &mut tx_buf);
    socket.set_timeout(Some(SOCKET_TIMEOUT));
    match with_timeout(CONNECT_TIMEOUT, socket.connect(IpEndpoint::new(addr, broker.port))).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => return failed("tcp connect refused"),
        Err(_) => return failed("tcp connect timed out"),
    }

    let mut bump_mem = [0u8; BUMP_BUF];
    let mut buffer = BumpBuffer::new(&mut bump_mem);
    let mut client: MqttClient<'_, '_> = Client::new(&mut buffer);

    let status_topic = id.status_topic();
    let Ok(will_topic) = MqttString::from_str(status_topic.as_str()) else { return failed("bad topic") };
    let Some(will_topic) = TopicName::new(will_topic) else { return failed("bad topic") };
    let Ok(will_payload) = MqttBinary::from_slice(OFFLINE.as_bytes()) else { return failed("bad will") };
    let keep_alive = KeepAlive::Seconds(NonZero::new(KEEP_ALIVE_SECS).expect("keepalive is not 0"));
    let mut options =
        ConnectOptions::new().clean_start().keep_alive(keep_alive).will(WillOptions::new(will_topic, will_payload).retain());
    if !broker.user.is_empty() {
        let (Ok(user), Ok(password)) =
            (MqttString::from_str(broker.user.as_str()), MqttBinary::from_slice(broker.password.0.as_str().as_bytes()))
        else {
            return failed("bad credentials");
        };
        options = options.user_name(user).password(password);
    }
    let Ok(client_id) = MqttString::from_str(id.as_str()) else { return failed("bad client id") };
    match with_timeout(CONNECT_TIMEOUT, client.connect(socket, &options, Some(client_id))).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            let reason = describe(&e);
            let _ = client.abort().await;
            return failed(reason);
        }
        Err(_) => {
            let _ = client.abort().await;
            return failed("mqtt connect timed out");
        }
    }
    // Safety: the `Connected` value was dropped above; nothing borrows
    // the buffer.
    unsafe { client.buffer_mut().reset() };

    let mut state = heapless::String::<STATE_BUF>::new();
    let mut discovery = heapless::String::<DISCOVERY_MAX>::new();
    // The subscriptions are answered later, by SUBACKs the loop below
    // matches against these identifiers; the reason to fail with goes
    // along with each.
    let mut awaiting: heapless::Vec<(PacketIdentifier, &'static str), 3> = heapless::Vec::new();
    let set_filter = id.set_filter();
    let cmd_topic = id.cmd_topic();
    let subscriptions = [
        (set_filter.as_str(), true, "subscribe to set/+ refused"),
        (cmd_topic.as_str(), false, "subscribe to cmd refused"),
        (core_mqtt::HA_STATUS_TOPIC, false, "subscribe to homeassistant/status refused"),
    ];
    // The hostname the discovery is built for; a change during the start
    // is caught below.
    let mut hostname = settings::get().hostname;
    let start = async {
        publish(&mut client, status_topic.as_str(), ONLINE.as_bytes(), true).await?;
        for (filter, retained, refused) in subscriptions {
            let pid = subscribe(&mut client, filter, retained).await?;
            let _ = awaiting.push((pid, refused));
        }
        discovery_json(&mut discovery, stack, id)?;
        publish(&mut client, id.discovery_topic().as_str(), discovery.as_bytes(), false).await?;
        // Whatever changed up to here goes into the state built now, so
        // the notification is taken; its value is compared below, since
        // a broker or hostname change is more than a state to publish.
        let pending = rx.try_changed();
        state_json(&mut state)?;
        publish(&mut client, id.state_topic().as_str(), state.as_bytes(), true).await?;
        Ok::<_, &'static str>(pending)
    };
    let pending = match start.await {
        Ok(pending) => pending,
        Err(reason) => {
            let _ = client.abort().await;
            return failed(reason);
        }
    };
    bump(&CONNECTS);
    set_link(Link::Connected);
    info!("mqtt: connected to {}:{} as {}", broker.host.as_str(), broker.port, id.as_str());

    let now = Instant::now();
    let mut last_send = now;
    let mut ping_sent: Option<Instant> = None;
    let mut state_due = now + STATE_PERIOD;
    let mut discovery_due: Option<Instant> = None;
    let suback_deadline = now + SUBACK_TIMEOUT;
    let failed = |reason| Outcome::Failed { reason, was_up: true };
    if let Some(s) = pending {
        if s.mqtt != *broker {
            info!("mqtt: broker setting changed during the start, disconnecting");
            farewell(&mut client, status_topic.as_str()).await;
            return Outcome::Settings;
        }
        if s.hostname != hostname {
            hostname = s.hostname;
            discovery_due = Some(now + birth_delay());
        }
    }

    /// Say goodbye on this connection: `offline` retained and a
    /// DISCONNECT, then the socket closed and flushed.
    async fn farewell(client: &mut MqttClient<'_, '_>, status_topic: &str) {
        let _ = publish(client, status_topic, OFFLINE.as_bytes(), true).await;
        match client.disconnect(&DisconnectOptions::new()).await {
            Ok(mut socket) => {
                socket.close();
                let _ = with_timeout(GOODBYE_WAIT, embedded_io_async::Write::flush(&mut socket)).await;
            }
            Err(_) => {
                let _ = client.abort().await;
            }
        }
    }

    loop {
        let ping_deadline = ping_sent.map_or(last_send + PING_AFTER, |at| at + PING_TIMEOUT);
        let mut deadline = state_due.min(ping_deadline);
        if let Some(at) = discovery_due {
            deadline = deadline.min(at);
        }
        if !awaiting.is_empty() {
            deadline = deadline.min(suback_deadline);
        }
        let resend_discovery = match select4(
            client.poll_header(),
            rx.changed(),
            display::ringing_changed(),
            select(Timer::at(deadline), select(GOODBYE.wait(), RELOAD.wait())),
        )
        .await
        {
            Either4::First(Ok(header)) => {
                let event = match client.poll_body(header).await {
                    Ok(e) => e,
                    Err(e) => {
                        let reason = describe(&e);
                        let _ = client.abort().await;
                        return failed(reason);
                    }
                };
                let mut refused = None;
                let resend = match event {
                    Event::Publish(p) => match p.topic.name() {
                        Some(name) => incoming(id, name.as_ref().as_str(), p.message.as_bytes()).await,
                        None => false,
                    },
                    Event::Pingresp => {
                        ping_sent = None;
                        false
                    }
                    Event::Suback(ack) => {
                        match awaiting.iter().position(|(pid, _)| *pid == ack.packet_identifier) {
                            Some(i) => {
                                let (_, reason) = awaiting.swap_remove(i);
                                if !ack.reason_code.is_success() {
                                    warn!("mqtt: {reason}: reason code {:#04x}", ack.reason_code.value());
                                    refused = Some(reason);
                                }
                            }
                            None => warn!("mqtt: SUBACK for a packet not waited for"),
                        }
                        false
                    }
                    _ => false,
                };
                // Safety: the event, the only borrower of the buffer, has
                // been dropped.
                unsafe { client.buffer_mut().reset() };
                if let Some(reason) = refused {
                    let _ = client.abort().await;
                    return failed(reason);
                }
                resend
            }
            Either4::First(Err(e)) => {
                let reason = describe(&e);
                let _ = client.abort().await;
                return failed(reason);
            }
            Either4::Second(s) => {
                if s.mqtt != *broker {
                    info!("mqtt: broker setting changed, disconnecting");
                    farewell(&mut client, status_topic.as_str()).await;
                    return Outcome::Settings;
                }
                // Debounce: more changes within the settle window fold in.
                state_due = state_due.min(Instant::now() + STATE_SETTLE);
                if s.hostname != hostname {
                    hostname = s.hostname;
                    true
                } else {
                    false
                }
            }
            Either4::Third(_) => {
                state_due = Instant::now();
                false
            }
            Either4::Fourth(Either::First(())) => false,
            Either4::Fourth(Either::Second(Either::First(()))) => {
                farewell(&mut client, status_topic.as_str()).await;
                return Outcome::Goodbye;
            }
            Either4::Fourth(Either::Second(Either::Second(()))) => {
                farewell(&mut client, status_topic.as_str()).await;
                set_link(Link::Off);
                heal::reboot().await
            }
        };
        if resend_discovery {
            discovery_due = Some(Instant::now() + birth_delay());
        }

        let now = Instant::now();
        if !awaiting.is_empty() && now >= suback_deadline {
            let _ = client.abort().await;
            return failed("no answer to a subscribe");
        }
        if discovery_due.is_some_and(|at| now >= at) {
            discovery_due = None;
            let sent = async {
                discovery_json(&mut discovery, stack, id)?;
                publish(&mut client, id.discovery_topic().as_str(), discovery.as_bytes(), false).await
            };
            if let Err(reason) = sent.await {
                let _ = client.abort().await;
                return failed(reason);
            }
            last_send = now;
        }
        if now >= state_due {
            state_due = now + STATE_PERIOD;
            let sent = async {
                state_json(&mut state)?;
                publish(&mut client, id.state_topic().as_str(), state.as_bytes(), true).await
            };
            if let Err(reason) = sent.await {
                let _ = client.abort().await;
                return failed(reason);
            }
            last_send = now;
        }
        match ping_sent {
            Some(at) if now >= at + PING_TIMEOUT => {
                let _ = client.abort().await;
                return failed("no answer to a ping");
            }
            None if now >= last_send + PING_AFTER => {
                if let Err(e) = client.ping().await {
                    let reason = describe(&e);
                    let _ = client.abort().await;
                    return failed(reason);
                }
                ping_sent = Some(now);
                last_send = now;
            }
            _ => {}
        }
    }
}

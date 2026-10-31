// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! A DHCP server for the clock's own access point, enough for a phone
//! to get an address and open the page: discover and request answered
//! with offer, ack or nak, eight leases, one option set. embassy-net
//! has only the client side.
//!
//! The packet layout and the lease table are pure (`parse`, `build`,
//! `Leases`), with the clock passed in as seconds, so a captured
//! exchange checks them on the host; `dhcpd_task` is the socket.
//! Every reply goes to the broadcast address: the client has no
//! address yet, a broadcast needs no ARP, and every client accepts it.

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_time::Instant;
use log::{info, warn};

use crate::config::{AP_ADDRESS, AP_LEASE_SECS};

pub const SERVER_PORT: u16 = 67;
pub const CLIENT_PORT: u16 = 68;

/// Addresses handed out: `AP_ADDRESS` with the last octet 2 to `2 + POOL`.
pub const POOL: usize = 8;
const FIRST_HOST: u8 = 2;

/// An offer that is not followed by a request frees its slot after this.
const OFFER_HOLD_SECS: u64 = 60;

/// Fixed BOOTP fields (RFC 951) and where the options start (RFC 2131).
const OP: usize = 0;
const HTYPE: usize = 1;
const HLEN: usize = 2;
const XID: usize = 4;
const FLAGS: usize = 10;
const CIADDR: usize = 12;
const YIADDR: usize = 16;
const SIADDR: usize = 20;
const CHADDR: usize = 28;
const COOKIE: usize = 236;
const OPTIONS: usize = 240;
const MAGIC: [u8; 4] = [99, 130, 83, 99];

/// A BOOTP request is at least this long; replies are padded to it.
const MIN_LEN: usize = 300;

const OPT_MASK: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_REQUESTED: u8 = 50;
const OPT_LEASE: u8 = 51;
const OPT_TYPE: u8 = 53;
const OPT_SERVER: u8 = 54;
const OPT_END: u8 = 255;
const OPT_PAD: u8 = 0;

const TYPE_DISCOVER: u8 = 1;
const TYPE_OFFER: u8 = 2;
const TYPE_REQUEST: u8 = 3;
const TYPE_ACK: u8 = 5;
const TYPE_NAK: u8 = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Discover,
    Request,
}

/// What a client asked, as far as the server cares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Request {
    pub kind: Kind,
    pub xid: [u8; 4],
    pub flags: [u8; 2],
    pub ciaddr: [u8; 4],
    pub chaddr: [u8; 6],
    /// Option 50, the address the client wants.
    pub requested: Option<[u8; 4]>,
    /// Option 54, the server whose offer the client took.
    pub server: Option<[u8; 4]>,
}

/// Parse a client packet: a BOOTP request from an Ethernet client with
/// the DHCP cookie and a discover or request type. Anything else is
/// `None` and ignored.
pub fn parse(pkt: &[u8]) -> Option<Request> {
    if pkt.len() < OPTIONS || pkt[OP] != 1 || pkt[HTYPE] != 1 || pkt[HLEN] != 6 || pkt[COOKIE..OPTIONS] != MAGIC {
        return None;
    }
    let mut kind = None;
    let mut requested = None;
    let mut server = None;
    let mut i = OPTIONS;
    while i < pkt.len() {
        let code = pkt[i];
        if code == OPT_END {
            break;
        }
        if code == OPT_PAD {
            i += 1;
            continue;
        }
        let len = *pkt.get(i + 1)? as usize;
        let value = pkt.get(i + 2..i + 2 + len)?;
        match (code, len) {
            (OPT_TYPE, 1) => {
                kind = match value[0] {
                    TYPE_DISCOVER => Some(Kind::Discover),
                    TYPE_REQUEST => Some(Kind::Request),
                    _ => return None,
                }
            }
            (OPT_REQUESTED, 4) => requested = Some(value.try_into().ok()?),
            (OPT_SERVER, 4) => server = Some(value.try_into().ok()?),
            _ => {}
        }
        i += 2 + len;
    }
    Some(Request {
        kind: kind?,
        xid: pkt[XID..XID + 4].try_into().ok()?,
        flags: pkt[FLAGS..FLAGS + 2].try_into().ok()?,
        ciaddr: pkt[CIADDR..CIADDR + 4].try_into().ok()?,
        chaddr: pkt[CHADDR..CHADDR + 6].try_into().ok()?,
        requested,
        server,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reply {
    Offer([u8; 4]),
    Ack([u8; 4]),
    Nak,
}

/// Write the reply to `req` into `out`: the fixed fields echoed, the
/// address, the server as `siaddr`, then the options every client
/// wants (type, server, lease, mask, router and DNS, both the server).
/// Returns the length, or `None` if `out` is too small.
pub fn build(out: &mut [u8], req: &Request, reply: Reply) -> Option<usize> {
    if out.len() < MIN_LEN {
        return None;
    }
    out[..OPTIONS].fill(0);
    out[OP] = 2;
    out[HTYPE] = 1;
    out[HLEN] = 6;
    out[XID..XID + 4].copy_from_slice(&req.xid);
    out[FLAGS..FLAGS + 2].copy_from_slice(&req.flags);
    out[SIADDR..SIADDR + 4].copy_from_slice(&AP_ADDRESS);
    out[CHADDR..CHADDR + 6].copy_from_slice(&req.chaddr);
    out[COOKIE..OPTIONS].copy_from_slice(&MAGIC);
    let mut w = Writer { out, at: OPTIONS };
    match reply {
        Reply::Offer(addr) | Reply::Ack(addr) => {
            if let Reply::Ack(_) = reply {
                w.out[CIADDR..CIADDR + 4].copy_from_slice(&req.ciaddr);
            }
            w.out[YIADDR..YIADDR + 4].copy_from_slice(&addr);
            w.option(OPT_TYPE, &[if let Reply::Offer(_) = reply { TYPE_OFFER } else { TYPE_ACK }])?;
            w.option(OPT_SERVER, &AP_ADDRESS)?;
            w.option(OPT_LEASE, &AP_LEASE_SECS.to_be_bytes())?;
            w.option(OPT_MASK, &[255, 255, 255, 0])?;
            w.option(OPT_ROUTER, &AP_ADDRESS)?;
            w.option(OPT_DNS, &AP_ADDRESS)?;
        }
        Reply::Nak => {
            w.option(OPT_TYPE, &[TYPE_NAK])?;
            w.option(OPT_SERVER, &AP_ADDRESS)?;
        }
    }
    w.byte(OPT_END)?;
    let at = w.at;
    let end = at.max(MIN_LEN);
    out[at..end].fill(0);
    Some(end)
}

struct Writer<'a> {
    out: &'a mut [u8],
    at: usize,
}

impl Writer<'_> {
    fn byte(&mut self, b: u8) -> Option<()> {
        *self.out.get_mut(self.at)? = b;
        self.at += 1;
        Some(())
    }

    fn option(&mut self, code: u8, value: &[u8]) -> Option<()> {
        self.byte(code)?;
        self.byte(value.len() as u8)?;
        self.out.get_mut(self.at..self.at + value.len())?.copy_from_slice(value);
        self.at += value.len();
        Some(())
    }
}

/// The address of pool slot `i`.
fn address(i: usize) -> [u8; 4] {
    [AP_ADDRESS[0], AP_ADDRESS[1], AP_ADDRESS[2], FIRST_HOST + i as u8]
}

/// The pool slot of an address, if it is one of ours.
fn slot_of(addr: [u8; 4]) -> Option<usize> {
    (addr[..3] == AP_ADDRESS[..3] && (FIRST_HOST..FIRST_HOST + POOL as u8).contains(&addr[3]))
        .then(|| (addr[3] - FIRST_HOST) as usize)
}

/// Who holds which address until when, in seconds of the caller's
/// clock. A client keeps its slot across discovers; a slot whose time
/// has passed is free.
#[derive(Clone, Copy, Debug, Default)]
pub struct Leases {
    slots: [Option<([u8; 6], u64)>; POOL],
}

impl Leases {
    pub const fn new() -> Self {
        Leases { slots: [None; POOL] }
    }

    fn find(&self, mac: [u8; 6], now_s: u64) -> Option<usize> {
        self.slots.iter().position(|s| matches!(s, Some((m, until)) if *m == mac && *until > now_s))
    }

    fn free(&self, now_s: u64) -> Option<usize> {
        self.slots.iter().position(|s| !matches!(s, Some((_, until)) if *until > now_s))
    }

    /// Answer a request, or `None` to stay silent: a request meant for
    /// another server, a pool with no room, a type the server ignores.
    pub fn handle(&mut self, req: &Request, now_s: u64) -> Option<Reply> {
        match req.kind {
            Kind::Discover => {
                let i = self.find(req.chaddr, now_s).or_else(|| self.free(now_s))?;
                self.slots[i] = Some((req.chaddr, now_s + OFFER_HOLD_SECS));
                Some(Reply::Offer(address(i)))
            }
            Kind::Request => {
                if req.server.is_some_and(|s| s != AP_ADDRESS) {
                    return None;
                }
                // Selecting or rebooting clients name the address in option
                // 50; a renewing client has it in `ciaddr`.
                let wanted = req.requested.unwrap_or(req.ciaddr);
                let Some(i) = slot_of(wanted) else {
                    return Some(Reply::Nak);
                };
                let mine = self.find(req.chaddr, now_s);
                let taken = matches!(self.slots[i], Some((m, until)) if m != req.chaddr && until > now_s);
                if mine.is_some_and(|m| m != i) || taken {
                    return Some(Reply::Nak);
                }
                self.slots[i] = Some((req.chaddr, now_s + AP_LEASE_SECS as u64));
                Some(Reply::Ack(wanted))
            }
        }
    }
}

/// Room for a client packet; phones send 300 to 600 bytes.
const PACKET: usize = 1024;

/// A MAC address for the log.
struct Mac([u8; 6]);

impl core::fmt::Display for Mac {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let b = self.0;
        write!(f, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", b[0], b[1], b[2], b[3], b[4], b[5])
    }
}

/// Serve DHCP on the access point's network.
#[embassy_executor::task]
pub async fn dhcpd_task(stack: Stack<'static>) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 2];
    let mut rx_buf = [0u8; PACKET];
    let mut tx_meta = [PacketMetadata::EMPTY; 2];
    let mut tx_buf = [0u8; MIN_LEN * 2];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    stack.wait_link_up().await;
    if let Err(e) = socket.bind(SERVER_PORT) {
        // Nothing to do without the socket; the page is still reachable
        // by a phone with a static address.
        warn!("dhcpd: bind failed: {e:?}");
        loop {
            embassy_time::Timer::after_secs(3600).await;
        }
    }
    info!("dhcpd: serving {}.{}.{}.{}-{} for {} s", AP_ADDRESS[0], AP_ADDRESS[1], AP_ADDRESS[2], FIRST_HOST, FIRST_HOST + POOL as u8 - 1, AP_LEASE_SECS);
    let broadcast = IpEndpoint::new(IpAddress::v4(255, 255, 255, 255), CLIENT_PORT);
    let mut leases = Leases::new();
    let mut pkt = [0u8; PACKET];
    let mut out = [0u8; MIN_LEN * 2];
    loop {
        let Ok((n, _)) = socket.recv_from(&mut pkt).await else { continue };
        let Some(req) = parse(&pkt[..n]) else { continue };
        let now_s = Instant::now().as_secs();
        let Some(reply) = leases.handle(&req, now_s) else {
            info!("dhcpd: {:?} from {} ignored", req.kind, Mac(req.chaddr));
            continue;
        };
        let Some(len) = build(&mut out, &req, reply) else { continue };
        match reply {
            Reply::Offer(a) | Reply::Ack(a) => info!(
                "dhcpd: {:?} from {}: {} {}.{}.{}.{}",
                req.kind,
                Mac(req.chaddr),
                if let Reply::Offer(_) = reply { "offer" } else { "ack" },
                a[0],
                a[1],
                a[2],
                a[3]
            ),
            Reply::Nak => warn!("dhcpd: {:?} from {} for {:?}: nak", req.kind, Mac(req.chaddr), req.requested),
        }
        if let Err(e) = socket.send_to(&out[..len], broadcast).await {
            warn!("dhcpd: send failed: {e:?}");
        }
    }
}

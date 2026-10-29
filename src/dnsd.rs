//! A captive DNS for the clock's own access point: every name is the
//! clock. A phone that has joined checks for internet by looking up a
//! known name and fetching a page from it; with the answer pointing
//! here and the HTTP server redirecting the probe to the setup page,
//! the phone opens that page in its sign-in sheet on its own.
//!
//! `answer` is pure, so a query checks on the host; `dnsd_task` is the
//! socket.

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Stack};
use log::{info, warn};

use crate::config::AP_ADDRESS;

pub const PORT: u16 = 53;

/// How long the phone may keep the answer. Short, so a phone that
/// leaves for a real network does not keep sending it to the clock.
const TTL_SECS: u32 = 60;

/// A DNS message over UDP is at most this long without extensions.
const PACKET: usize = 512;

const HEADER: usize = 12;
const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;

/// Answer a query, or `None` for anything that is not a plain query
/// with one question: a response, another opcode, a truncated name.
/// An A question gets the clock's address; any other type gets an
/// empty answer with no error, which is "no such record" to the asker.
pub fn answer(query: &[u8], out: &mut [u8]) -> Option<usize> {
    if query.len() < HEADER || out.len() < PACKET {
        return None;
    }
    let flags = u16::from_be_bytes([query[2], query[3]]);
    let qdcount = u16::from_be_bytes([query[4], query[5]]);
    // QR must be 0 (a query) and the opcode 0 (standard).
    if flags & 0xF800 != 0 || qdcount != 1 {
        return None;
    }
    // Walk the labels of the one question to its end.
    let mut i = HEADER;
    loop {
        let len = *query.get(i)? as usize;
        if len == 0 {
            i += 1;
            break;
        }
        if len >= 0xC0 {
            // A pointer in a question is not something a client sends.
            return None;
        }
        i += 1 + len;
    }
    let qtype = u16::from_be_bytes([*query.get(i)?, *query.get(i + 1)?]);
    let qclass = u16::from_be_bytes([*query.get(i + 2)?, *query.get(i + 3)?]);
    let question_end = i + 4;
    let answers: u16 = if qtype == TYPE_A && qclass == CLASS_IN { 1 } else { 0 };

    // Header: the ID, QR set, RD copied, RA set, no error; one question,
    // the answers, nothing else.
    out[..2].copy_from_slice(&query[..2]);
    let rd = flags & 0x0100;
    out[2..4].copy_from_slice(&(0x8080 | rd).to_be_bytes());
    out[4..6].copy_from_slice(&1u16.to_be_bytes());
    out[6..8].copy_from_slice(&answers.to_be_bytes());
    out[8..12].fill(0);
    let question = &query[HEADER..question_end];
    let mut n = HEADER + question.len();
    out[HEADER..n].copy_from_slice(question);
    if answers == 1 {
        // The name as a pointer to the question, then the record.
        let record = [
            0xC0,
            HEADER as u8,
            0,
            TYPE_A as u8,
            0,
            CLASS_IN as u8,
            (TTL_SECS >> 24) as u8,
            (TTL_SECS >> 16) as u8,
            (TTL_SECS >> 8) as u8,
            TTL_SECS as u8,
            0,
            4,
            AP_ADDRESS[0],
            AP_ADDRESS[1],
            AP_ADDRESS[2],
            AP_ADDRESS[3],
        ];
        out.get_mut(n..n + record.len())?.copy_from_slice(&record);
        n += record.len();
    }
    Some(n)
}

/// The name in a query, for the log: labels joined with dots, cut to
/// the buffer.
fn name_of(query: &[u8]) -> heapless::String<64> {
    let mut s = heapless::String::new();
    let mut i = HEADER;
    while let Some(&len) = query.get(i) {
        if len == 0 || len >= 0xC0 {
            break;
        }
        if !s.is_empty() {
            let _ = s.push('.');
        }
        for &c in query.get(i + 1..i + 1 + len as usize).unwrap_or(&[]) {
            let _ = s.push(if c.is_ascii_graphic() { c as char } else { '?' });
        }
        i += 1 + len as usize;
    }
    s
}

/// Answer every name with the clock's address while it is the access
/// point.
#[embassy_executor::task]
pub async fn dnsd_task(stack: Stack<'static>) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; PACKET * 2];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; PACKET * 2];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    stack.wait_link_up().await;
    if let Err(e) = socket.bind(PORT) {
        warn!("dnsd: bind failed: {e:?}");
        loop {
            embassy_time::Timer::after_secs(3600).await;
        }
    }
    let [a, b, c, d] = AP_ADDRESS;
    info!("dnsd: every name is {a}.{b}.{c}.{d}");
    let mut query = [0u8; PACKET];
    let mut out = [0u8; PACKET];
    loop {
        let Ok((n, meta)) = socket.recv_from(&mut query).await else { continue };
        let Some(len) = answer(&query[..n], &mut out) else { continue };
        info!("dnsd: {} from {}", name_of(&query[..n]), meta.endpoint);
        let to: IpEndpoint = meta.endpoint;
        if let IpAddress::Ipv4(_) = to.addr
            && let Err(e) = socket.send_to(&out[..len], to).await
        {
            warn!("dnsd: send failed: {e:?}");
        }
    }
}

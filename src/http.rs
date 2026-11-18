// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The settings page and API: a few TCP sockets, one request per
//! connection, responses built whole and written once. Runs in thread
//! mode and never touches the scan path, except that a firmware upload
//! (`POST /ota`, streamed into `ota::install`) stops it for the write.
//!
//! Four pages, each with a line of links to the others: `/` (the
//! alarm, the display, the status), `/settings`, `/ha` (the MQTT
//! broker) and `/system`. The first three are forms posted to their
//! own path, each a partial update of the settings. `POST /api` takes
//! every setting; the network (`ssid`, `password`) only with the token.
//! `GET /system` serves the page that asks for a PIN (its script
//! posts `/pin`, which issues it),
//! with the four things the PIN or the token allows: the upload (`POST
//! /ota`), a new token (`POST /token`), a reset (`POST /reload`) and a
//! reset into the access point (`POST /ap`); `GET /update` and `GET
//! /token`, the pages it replaced, redirect to it. `POST /pin` issues
//! a PIN, for that page and for a script. Every request is held to
//! `http::admit` first: a `Host` that is not the clock's address or
//! name is sent to the address (`GET`) or refused (`POST`), and a
//! `POST` whose `Origin` is another site's is refused, so no page on
//! the internet can post a form to the clock through a browser on the
//! LAN, by a form or by a name made to resolve here. On the access point `GET /` is the setup form, `POST /scan`
//! refreshes its network list, and any other path is a redirect to the
//! clock's address, so a phone's connectivity probe (sent to the clock
//! by the captive DNS) opens the form in the sign-in sheet.
//!
//! Several sockets listen at once because a browser opens more
//! connections than it uses: a speculative one alongside the page, one
//! for the favicon, a fresh one for the redirect after a form post. With
//! a single socket each of those was a refused connection for the next
//! real request.

use core::fmt::Write as _;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use embassy_net::Stack;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, with_timeout};
use embedded_io_async::{Read, Write};
use log::{info, warn};

use onebitdawn_core::http::{BODY_MAX, HEADERS_MAX, Method, Own, PAGE_MAX, RESPONSE_BUF, Refused, admit, parse_head};

use crate::buzzer;
use crate::config::HTTP_PORT;
use crate::form::{self, Allow, FormError, HealStamp, LocalNow, Note, Page, Status};
use crate::net::{self, Mode};
use crate::{auth, clock, display, heal, mqtt, ota, pack, sensors, settings, watchdog};

/// The tab icon: the panel as pixel art, a 32 px `12:34` and a 16 px
/// `1:23` as PNG entries in one ICO, served at the path browsers ask
/// for by themselves.
const FAVICON: &[u8] = include_bytes!("favicon.ico");

/// `/tz.js`, the browser-zone script of the timezone field; it must fit
/// the response buffer after its header.
const TZ_JS: &[u8] = include_bytes!("tz.js");
const _: () = assert!(TZ_JS.len() + 128 <= RESPONSE_BUF, "tz.js does not fit the response buffer");

/// Connections served at once, each with its own buffers and task.
pub const LISTENERS: usize = 3;

/// The receive buffer is the TCP window, and a firmware upload is 800 KB
/// of body; 1 KB throttled it to one segment per round trip.
const RX_BUF: usize = 4096;
const TX_BUF: usize = 1024;
const REQUEST_BUF: usize = 1536;

/// A client that stalls this long mid-transfer is dropped.
const TIMEOUT: Duration = Duration::from_secs(5);

/// A rescan asked for by the setup page takes about three seconds; past
/// this the page comes back with whatever the list holds.
const SCAN_WAIT: Duration = Duration::from_secs(10);

/// A client that has connected but not sent its request within this long
/// is dropped, so an idle speculative connection frees its socket.
const HEAD_TIMEOUT: Duration = Duration::from_secs(2);

/// A refused upload reads and discards the rest of the body, up to this
/// much and for this long, before answering. Answering at once and
/// closing would reset the connection under the client's unsent body,
/// and a browser then reports a lost connection instead of the `403`.
const DRAIN_MAX: usize = 2 * 1024 * 1024;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(40);

/// Where a listener is in its loop, for the heartbeat. Three listeners
/// stuck in the same place is the shape of a wedged server.
#[derive(Clone, Copy)]
#[repr(u8)]
enum State {
    Waiting = 0,
    Reading = 1,
    Writing = 2,
    Closing = 3,
}

impl State {
    fn name(raw: u8) -> &'static str {
        match raw {
            0 => "waiting",
            1 => "reading",
            2 => "writing",
            3 => "closing",
            _ => "?",
        }
    }
}

/// One listener's state and counts; written by its task, read by the
/// heartbeat.
struct Listener {
    state: AtomicU8,
    accepted: AtomicU32,
    served: AtomicU32,
    head_timeouts: AtomicU32,
}

impl Listener {
    const fn new() -> Self {
        Listener {
            state: AtomicU8::new(State::Waiting as u8),
            accepted: AtomicU32::new(0),
            served: AtomicU32::new(0),
            head_timeouts: AtomicU32::new(0),
        }
    }

    fn set(&self, state: State) {
        self.state.store(state as u8, Ordering::Relaxed);
    }
}

/// The target has no atomic add; each counter has one writer, so a plain
/// read-modify-write is safe.
fn bump(counter: &AtomicU32) {
    counter.store(counter.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
}

static LISTENER: [Listener; LISTENERS] = [const { Listener::new() }; LISTENERS];

/// One line per heartbeat: each listener's state and its counts of
/// connections accepted, requests served and heads that never arrived.
pub fn log_status() {
    let mut line = heapless::String::<160>::new();
    for (i, l) in LISTENER.iter().enumerate() {
        let _ = write!(
            line,
            "{}L{i} {} {}/{}/{}",
            if i == 0 { "" } else { ", " },
            State::name(l.state.load(Ordering::Relaxed)),
            l.accepted.load(Ordering::Relaxed),
            l.served.load(Ordering::Relaxed),
            l.head_timeouts.load(Ordering::Relaxed),
        );
    }
    info!("http: {line} (accepted/served/head timeouts)");
}

/// A fixed buffer with `core::fmt::Write`; overflow is an error, not a
/// truncation, so a page that outgrows it is noticed.
struct Response {
    buf: [u8; RESPONSE_BUF],
    len: usize,
}

impl Response {
    fn new() -> Self {
        Response { buf: [0; RESPONSE_BUF], len: 0 }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl Response {
    fn write_bytes(&mut self, bytes: &[u8]) -> core::fmt::Result {
        let end = self.len + bytes.len();
        if end > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }
}

impl core::fmt::Write for Response {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write_bytes(s.as_bytes())
    }
}

/// The body of a request: read whole into the request buffer for the
/// small ones, or left on the socket for a firmware upload, with what
/// followed the head in the buffer and the total to expect.
enum Body<'a> {
    Inline(&'a [u8]),
    Stream { first: &'a [u8], total: usize },
}

/// What was asked. `query` is what followed `?` in the target, without
/// the `?`; `authorization`, `host` and `origin` are those headers'
/// values, if sent.
struct Request<'a> {
    method: Method,
    path: &'a str,
    query: &'a str,
    authorization: Option<&'a str>,
    host: Option<&'a str>,
    origin: Option<&'a str>,
    body: Body<'a>,
}

/// Why `read_request` gave no request: the connection is just closed,
/// or a status goes out first, for a request the server understood
/// enough to refuse: a `POST` without `Content-Length` to a route that
/// reads a body (not an empty form, whose length is 0, but a body the
/// server cannot read: `411`) or a `Transfer-Encoding`, which it does
/// not read (`400`).
enum NoRequest {
    Close,
    Refuse(&'static str),
}

/// Whether a `POST` to `path` carries a body the server reads, so that a
/// missing `Content-Length` is a request it cannot serve. The other
/// posts (`/reload`, `/ap`, `/pin`, `/scan`, `/stop`) are commands with nothing
/// to read, sent as a bare `curl -X POST` by the scripts, and a missing
/// length there means none (found on the bench, 2026-11-08, when the
/// first `411` rule refused `scripts/reload.sh`).
fn reads_body(path: &str) -> bool {
    matches!(path, "/" | "/settings" | "/ha" | "/api" | "/play" | "/token" | "/ota" | "/clips")
}

/// Read the head and the body. `Err` for anything that is not a request
/// this server answers; see `NoRequest`.
///
/// The head is parsed twice: once for the two facts that decide how the
/// body is read (a firmware upload's body stays on the socket), so that
/// its borrow of the buffer ends before the body is read into it, and
/// once more afterwards, on the same bytes, for the request itself.
async fn read_request<'a>(socket: &mut TcpSocket<'_>, buf: &'a mut [u8; REQUEST_BUF]) -> Result<Request<'a>, NoRequest> {
    fn close<E>(_: E) -> NoRequest {
        NoRequest::Close
    }
    const BAD: NoRequest = NoRequest::Refuse("400 Bad Request");
    let mut n = 0;
    let head_end = loop {
        if n == buf.len() {
            // A head with no end in the buffer: how much more is coming
            // is unknown, so what arrives is discarded until the client
            // pauses, and then it is told.
            discard_until_quiet(socket, buf).await;
            return Err(NoRequest::Refuse("431 Request Header Fields Too Large"));
        }
        let got = socket.read(&mut buf[n..]).await.map_err(close)?;
        if got == 0 {
            return Err(NoRequest::Close);
        }
        n += got;
        if let Some(i) = buf[..n].windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };

    let (upload, content_length) = {
        let head = parse_head(core::str::from_utf8(&buf[..head_end]).map_err(|_| BAD)?).ok_or(BAD)?;
        if head.transfer_encoding {
            return Err(BAD);
        }
        let content_length = match (head.method, head.content_length) {
            (_, Some(n)) => n,
            (Method::Get, None) => 0,
            (Method::Post, None) if reads_body(head.path) => return Err(NoRequest::Refuse("411 Length Required")),
            (Method::Post, None) => 0,
        };
        (head.method == Method::Post && matches!(head.path, "/ota" | "/clips"), content_length)
    };
    if !upload {
        if content_length > BODY_MAX || head_end + content_length > buf.len() {
            // The rest of the body is read before the answer, or the
            // close resets the connection and the client never sees it.
            discard(socket, buf, (head_end + content_length).saturating_sub(n)).await;
            return Err(NoRequest::Refuse("413 Content Too Large"));
        }
        while n < head_end + content_length {
            let got = socket.read(&mut buf[n..]).await.map_err(close)?;
            if got == 0 {
                return Err(NoRequest::Close);
            }
            n += got;
        }
    }

    let (head_bytes, rest) = buf.split_at(head_end);
    let head = parse_head(core::str::from_utf8(head_bytes).map_err(close)?).ok_or(NoRequest::Close)?;
    let body = if upload {
        Body::Stream { first: &rest[..n - head_end], total: content_length }
    } else {
        Body::Inline(&rest[..content_length])
    };
    Ok(Request {
        method: head.method,
        path: head.path,
        query: head.query,
        authorization: head.authorization,
        host: head.host,
        origin: head.origin,
        body,
    })
}

/// Read and throw away `remaining` bytes of a refused request's body,
/// the request buffer as scratch. Nothing over `DRAIN_MAX` is waited
/// for, and the listener's `HEAD_TIMEOUT` is over the whole of it.
async fn discard(socket: &mut TcpSocket<'_>, buf: &mut [u8; REQUEST_BUF], remaining: usize) {
    if remaining > DRAIN_MAX {
        return;
    }
    let mut left = remaining;
    while left > 0 {
        match socket.read(&mut buf[..left.min(REQUEST_BUF)]).await {
            Ok(0) | Err(_) => break,
            Ok(n) => left -= n,
        }
    }
}

/// Read and throw away what a client is still sending, until it has
/// been quiet for `QUIET` or has sent `REQUEST_BUF` eight times over:
/// for a head too long to find the end of.
async fn discard_until_quiet(socket: &mut TcpSocket<'_>, buf: &mut [u8; REQUEST_BUF]) {
    const QUIET: Duration = Duration::from_millis(100);
    let mut left = 8 * REQUEST_BUF;
    while left > 0 {
        match with_timeout(QUIET, socket.read(&mut buf[..])).await {
            Ok(Ok(n)) if n > 0 => left = left.saturating_sub(n),
            _ => break,
        }
    }
}

/// The head of every response with a body of the clock's own making.
/// No page may be shown in a frame: a click there would be a post from
/// the clock's own origin, which `admit` lets through.
fn status_line(out: &mut Response, status: &str, content_type: &str) -> core::fmt::Result {
    let before = out.len;
    write!(
        out,
        "HTTP/1.0 {status}\r\nContent-Type: {content_type}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Frame-Options: DENY\r\n\r\n"
    )?;
    debug_assert!(status != "200 OK" || out.len - before <= HEADERS_MAX);
    Ok(())
}

const _: () = assert!(PAGE_MAX + HEADERS_MAX <= RESPONSE_BUF);

/// Back to the clock page, or the setup form on the access point.
fn redirect_home(out: &mut Response, note: Note) -> core::fmt::Result {
    redirect_to(out, Page::Clock, note)
}

/// Back to the page a form was posted from, with what became of it in
/// the query so the page can say so: `?saved=1`, or the error of a
/// rejected form and the key of the field at fault, if it has one.
fn redirect_to(out: &mut Response, page: Page, note: Note) -> core::fmt::Result {
    write!(out, "HTTP/1.0 303 See Other\r\nLocation: {}", page.path())?;
    match note {
        Note::None => {}
        Note::Saved => out.write_str("?saved=1")?,
        Note::Refused(e, key) => {
            out.write_str(match e {
                FormError::BadValue => "?error=value",
                FormError::UnknownKey => "?error=key",
                FormError::Forbidden => "?error=token",
                FormError::BrokerPassword => "?error=broker",
            })?;
            if let Some(key) = key {
                write!(out, "&key={key}")?;
            }
        }
    }
    out.write_str("\r\nConnection: close\r\n\r\n")
}

/// The notice the page was redirected back with, from `?saved=1` or
/// `?error=...&key=...`. The key is taken only if it is one of the
/// pages' own (`form::field`), so the page never writes the query's
/// text.
fn note_from_query(query: &str) -> Note {
    let error = match query_value(query, "error") {
        Some("value") => FormError::BadValue,
        Some("key") => FormError::UnknownKey,
        Some("token") => FormError::Forbidden,
        Some("broker") => FormError::BrokerPassword,
        _ if query_value(query, "saved").is_some() => return Note::Saved,
        _ => return Note::None,
    };
    Note::Refused(error, query_value(query, "key").and_then(form::field))
}

/// The value of `key` in a query string, undecoded.
fn query_value<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| match pair.split_once('=') {
        Some((k, v)) if k == key => Some(v),
        _ => None,
    })
}

/// A random number for a PIN.
fn random() -> u32 {
    esp_hal::rng::Rng::new().random()
}

/// The read-only status for the page and the JSON.
pub fn status() -> Status {
    let now = clock::unix_secs();
    let last_heal = heal::last_unix().map(|t| {
        let local = clock::local_time(t);
        HealStamp {
            unix_secs: t,
            weekday: local.weekday,
            hour: local.hour,
            minute: local.minute,
            ago_secs: now.map(|n| n - t),
        }
    });
    let (temp_dc, light) = sensors::room();
    Status {
        version: ota::VERSION,
        slot: ota::booted().name(),
        mode: if net::mode() == Mode::AccessPoint { "ap" } else { "sta" },
        uptime_secs: embassy_time::Instant::now().as_secs(),
        heals: heal::count(),
        watchdog: watchdog::count(),
        clips_paused: watchdog::clips_suspect(),
        last_heal,
        temp_dc,
        light,
        rssi: net::rssi(),
        mqtt: mqtt::link(),
        ringing: display::ringing(),
        clips_room: pack::room(),
        local: now.map(|n| {
            let t = clock::local_time(n);
            LocalNow { weekday: t.weekday, hour: t.hour, minute: t.minute, offset_secs: t.offset_secs, dst: clock::period_at(n).1 }
        }),
    }
}

/// What the listener does once the response is out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Then {
    Close,
    /// Reboot into the image just written.
    NewImage,
    /// Reboot as asked, with the carry record.
    Reload,
    /// Leave the access point for the network in the record, which the
    /// provisioning form posted unchanged, so no save will do it.
    Join,
    /// Reset into the access point, as asked with the token.
    AccessPoint,
}

/// The page a settings form is served at and posted to, on the station.
fn form_page(path: &str) -> Option<Page> {
    [Page::Clock, Page::Settings, Page::HomeAssistant].into_iter().find(|page| page.path() == path)
}

/// `POST /reload` and `POST /ap`: a reset of the kind `then`, for the
/// token or the PIN the system page's visitor read off the panel.
fn restart(out: &mut Response, req: &Request<'_>, what: &str, then: Then) -> Result<Then, core::fmt::Error> {
    match auth::token_or_pin(req.authorization) {
        Ok(()) => {
            warn!("http: {what} requested");
            status_line(out, "200 OK", "text/plain")?;
            writeln!(out, "{what}")?;
            Ok(then)
        }
        Err(e) => {
            warn!("http: {what} refused: {}", e.text);
            status_line(out, e.status, "text/plain")?;
            writeln!(out, "{}", e.text)?;
            Ok(Then::Close)
        }
    }
}

/// Build the response for one request with its body in hand.
fn respond(out: &mut Response, req: &Request<'_>, body: &[u8]) -> Result<Then, core::fmt::Error> {
    match (req.method, req.path) {
        (Method::Get, "/") if net::mode() == Mode::AccessPoint => {
            status_line(out, "200 OK", "text/html; charset=utf-8")?;
            form::write_provision_page(out, &settings::get(), &status(), &net::networks(), note_from_query(req.query))?;
        }
        // The settings pages are the station's: on the access point
        // their paths go the captive portal's way, to the setup form.
        (Method::Get, path) if net::mode() != Mode::AccessPoint && form_page(path).is_some() => {
            status_line(out, "200 OK", "text/html; charset=utf-8")?;
            let (s, st, note) = (settings::get(), status(), note_from_query(req.query));
            match form_page(path) {
                Some(Page::Settings) => form::write_settings_page(out, &s, &st, note)?,
                Some(Page::HomeAssistant) => form::write_ha_page(out, &s, &st, note)?,
                _ => form::write_page(out, &s, &st, note)?,
            }
        }
        (Method::Post, "/") if net::mode() == Mode::AccessPoint => {
            // The provisioning form: the network keys are allowed, since
            // the access point's own password is the gate here. A change
            // to anything is saved, and on the access point the persist
            // task resets into the network after every save; the same
            // settings again (an access point raised by the hold, nothing
            // touched) are nothing to save, so the listener resets after
            // answering. An empty password keeps the stored one
            // (`form::settle_password`).
            let current = settings::get();
            let mut trial = current;
            let result = form::apply_naming(&mut trial, body, Allow::ALL)
                .and_then(|()| form::settle_password(&mut trial, &current.wifi, &net::networks()).map_err(|e| (e, None)));
            match result {
                Ok(()) if trial.wifi.is_none() => {
                    warn!("http: setup form named no network");
                    redirect_home(out, Note::Refused(FormError::BadValue, None))?;
                }
                Ok(()) => {
                    status_line(out, "200 OK", "text/html; charset=utf-8")?;
                    form::write_saved_page(out, &trial)?;
                    if trial == current {
                        warn!("http: network {} posted unchanged, leaving the access point", trial.wifi.ssid.as_str());
                        return Ok(Then::Join);
                    }
                    warn!("http: network {} saved, leaving the access point once it is written", trial.wifi.ssid.as_str());
                    settings::update(|s| *s = trial);
                }
                Err((e, key)) => {
                    warn!("http: setup form rejected: {e:?} {key:?}");
                    redirect_home(out, Note::Refused(e, key))?;
                }
            }
        }
        (Method::Post, path) if net::mode() != Mode::AccessPoint && form_page(path).is_some() => {
            // A page's form, a partial update: only the keys it sends
            // change, and a checkbox is sent either way. All or
            // nothing: the form is applied to a copy and committed only
            // if every pair parsed. The broker keys are a page's to
            // set; the network keys are not. The answer is the page it
            // came from, which says that it was saved, or why not and
            // which field.
            let mut trial = settings::get();
            let note = match form::apply_naming(&mut trial, body, Allow::PAGE) {
                Ok(()) => {
                    settings::update(|s| *s = trial);
                    Note::Saved
                }
                Err((e, key)) => {
                    warn!("http: form rejected: {e:?} {key:?}");
                    Note::Refused(e, key)
                }
            };
            redirect_to(out, form_page(path).unwrap_or(Page::Clock), note)?;
        }
        (Method::Post, "/play") => {
            // Preview the sound chosen in the form without saving anything:
            // the form is applied to a scratch copy and only its melody used,
            // so the page's broker rule need not hold (`Allow::ALL`).
            // Nothing sounds while the alarm rings (`buzzer::preview`).
            let mut trial = settings::get();
            if let Err(e) = form::apply(&mut trial, body, Allow::ALL) {
                warn!("http: play form rejected: {e:?}");
            }
            buzzer::preview(trial.melody);
            redirect_home(out, Note::None)?;
        }
        (Method::Post, "/stop") => {
            // The page's Stop button: an end to a preview. A ringing
            // alarm is not its to end, nor `/play`'s to interrupt; the
            // buzzer ignores both. Whatever body came with it is ignored.
            buzzer::stop_preview();
            redirect_home(out, Note::None)?;
        }
        (Method::Get, "/favicon.ico") => {
            // The two responses a browser may cache, this and the script
            // below: they only change with the firmware.
            out.write_str("HTTP/1.0 200 OK\r\nContent-Type: image/x-icon\r\nConnection: close\r\nCache-Control: max-age=86400\r\n\r\n")?;
            out.write_bytes(FAVICON)?;
        }
        (Method::Get, "/tz.js") => {
            // The browser-zone script of both forms' timezone field.
            out.write_str("HTTP/1.0 200 OK\r\nContent-Type: text/javascript\r\nConnection: close\r\nCache-Control: max-age=86400\r\n\r\n")?;
            out.write_bytes(TZ_JS)?;
        }
        (Method::Get, "/system") => {
            // The page asks for a PIN, once, for whichever of its four
            // actions; its script has the clock issue one, below. Not
            // this `GET`, which any site's `<img>` can send.
            status_line(out, "200 OK", "text/html; charset=utf-8")?;
            form::write_system_page(out, settings::get().hostname.as_str(), &status(), auth::token_enabled())?;
        }
        (Method::Get, "/clips") => {
            // The clip pack and its upload, with the PIN as the system
            // page's: this `GET` issues none either.
            status_line(out, "200 OK", "text/html; charset=utf-8")?;
            form::write_clips_page(out, settings::get().hostname.as_str(), &status())?;
        }
        (Method::Get, "/update" | "/token") => {
            // The pages the system page replaced: an older README, the
            // web installer's text and bookmarks name them.
            write!(out, "HTTP/1.0 302 Found\r\nLocation: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n", Page::System.path())?;
        }
        (Method::Post, "/pin") => {
            // A PIN on the panel: for the system page, whose script
            // posts this as it loads, and for a script that reads the
            // serial log. The digits are never in the response.
            match auth::issue_pin(random()) {
                Ok(_) => {
                    status_line(out, "200 OK", "text/plain")?;
                    writeln!(out, "PIN issued, on the panel and the serial log for {} min", auth::PIN_TTL_MS / 60_000)?;
                }
                Err(secs) => {
                    status_line(out, "403 Forbidden", "text/plain")?;
                    writeln!(out, "locked out for {secs} s after a wrong PIN")?;
                }
            }
        }
        (Method::Post, "/token") => {
            // A new API token, with the PIN from the system page or the
            // current token. The value is never logged or echoed. The
            // body is parsed before the PIN is spent, so a bad one costs
            // a 400 and not the PIN.
            let token = match form::parse_token(body) {
                Ok(token) => token,
                Err(e) => {
                    warn!("http: token change rejected: {e:?}");
                    status_line(out, "400 Bad Request", "text/plain")?;
                    out.write_str("bad token: 8 to 32 printable characters without spaces, or empty to clear\n")?;
                    return Ok(Then::Close);
                }
            };
            if let Err(e) = auth::token_or_pin(req.authorization) {
                warn!("http: token change refused: {}", e.text);
                status_line(out, e.status, "text/plain")?;
                writeln!(out, "{}", e.text)?;
                return Ok(Then::Close);
            }
            let set = !token.0.is_empty();
            // Cleared on purpose: the build's token must not come back at
            // the next boot.
            settings::update(|s| {
                s.token = token;
                s.token_cleared = !set;
            });
            warn!("http: API token {}", if set { "set" } else { "cleared" });
            status_line(out, "200 OK", "text/plain")?;
            out.write_str(if set { "token set\n" } else { "token cleared; the script paths are closed\n" })?;
        }
        (Method::Get, "/api") => {
            status_line(out, "200 OK", "application/json")?;
            form::write_json(out, &settings::get(), &status())?;
        }
        (Method::Post, "/api") => {
            // Validate on a copy first so a bad pair changes nothing. The
            // network and broker keys need the token, except on the access
            // point, whose own password is the gate; the rest do not.
            let trusted = net::mode() == Mode::AccessPoint || auth::token(req.authorization).is_ok();
            let current = settings::get();
            let mut trial = current;
            match form::apply(&mut trial, body, if trusted { Allow::ALL } else { Allow::NONE }) {
                // A password and no network to use it on: saved, it
                // would be a record that joins nothing. Only a request
                // that made it so is refused, so a clock already there
                // still takes its other keys.
                Ok(()) if trial.wifi != current.wifi && trial.wifi.password_alone() => {
                    warn!("http: a password with no network refused");
                    status_line(out, "400 Bad Request", "text/plain")?;
                    out.write_str("a password needs a network: send ssid with it\n")?;
                }
                Ok(()) => {
                    settings::update(|s| *s = trial);
                    status_line(out, "200 OK", "application/json")?;
                    form::write_json(out, &trial, &status())?;
                }
                Err(FormError::Forbidden) => {
                    warn!("http: network or broker change without the token refused");
                    status_line(out, "403 Forbidden", "text/plain")?;
                    out.write_str("token required for ssid, password and mqtt_*: send Authorization: Bearer <API_TOKEN>\n")?;
                }
                Err(e) => {
                    status_line(out, "400 Bad Request", "text/plain")?;
                    out.write_str(match e {
                        FormError::UnknownKey => "unknown key\n",
                        _ => "bad value\n",
                    })?;
                }
            }
        }
        // Into the access point: from a script with the token, the way
        // onto a bench without hands on the buttons, or from the system
        // page with the PIN.
        (Method::Post, "/ap") => return restart(out, req, "resetting into access point mode", Then::AccessPoint),
        // A deliberate reset, which is also the way to test the carry:
        // no hold for a ringing alarm.
        (Method::Post, "/reload") => return restart(out, req, "rebooting with the time carried over", Then::Reload),
        // The bench's hangs, for trying the watchdog: watchdog.rs.
        #[cfg(feature = "bench-hang")]
        (Method::Post, path) if path.starts_with("/hang/") => {
            let hang = watchdog::bench::Hang::parse(&path["/hang/".len()..]);
            match (hang, auth::token_or_pin(req.authorization)) {
                (Some(hang), Ok(())) => {
                    watchdog::bench::request(hang);
                    status_line(out, "200 OK", "text/plain")?;
                    writeln!(out, "hanging: {hang:?}")?;
                }
                (None, _) => {
                    status_line(out, "404 Not Found", "text/plain")?;
                    out.write_str("no such hang\n")?;
                }
                (_, Err(e)) => {
                    status_line(out, e.status, "text/plain")?;
                    writeln!(out, "{}", e.text)?;
                }
            }
        }
        _ if net::mode() == Mode::AccessPoint => {
            // The captive portal: a phone's probe lands here through the
            // DNS and is sent to the page, which the phone then shows.
            let [a, b, c, d] = crate::config::AP_ADDRESS;
            write!(out, "HTTP/1.0 302 Found\r\nLocation: http://{a}.{b}.{c}.{d}/\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n")?;
        }
        _ => {
            status_line(out, "404 Not Found", "text/plain")?;
            out.write_str("not found\n")?;
        }
    }
    Ok(Then::Close)
}

/// `POST /scan` on the access point: ask the AP task for a scan, wait
/// for it, and send the page back to `/` with the new list. The form
/// body is ignored. Not a route anywhere else.
async fn rescan(out: &mut Response) -> Result<Then, core::fmt::Error> {
    if net::mode() != Mode::AccessPoint {
        status_line(out, "404 Not Found", "text/plain")?;
        out.write_str("not found\n")?;
        return Ok(Then::Close);
    }
    info!("http: rescan asked for");
    let scan = net::request_scan();
    if with_timeout(SCAN_WAIT, net::scan_done(scan)).await.is_err() {
        warn!("http: scan not done within {} s", SCAN_WAIT.as_secs());
    }
    redirect_home(out, Note::None)?;
    Ok(Then::Close)
}

/// A request `admit` refused. Another `Host` on a `GET` is sent to the
/// clock's address: a LAN that calls the clock something else gets
/// there, a page that rebound its name to the clock is sent off its
/// origin, and on the access point it is the captive portal's
/// redirect. Anything else is `403`. An upload's body is taken off the
/// socket first, as for a wrong token.
async fn turn_away(
    out: &mut Response,
    req: &Request<'_>,
    refused: Refused,
    ip: Option<[u8; 4]>,
    socket: &mut TcpSocket<'_>,
) -> Result<Then, core::fmt::Error> {
    warn!("http: {:?} {} refused by its {refused:?}: host {:?}, origin {:?}", req.method, req.path, req.host, req.origin);
    if let Body::Stream { first, total } = req.body {
        drain(socket, out, total.saturating_sub(first.len())).await;
    }
    match (refused, req.method, ip) {
        (Refused::Host, Method::Get, Some([a, b, c, d])) => {
            write!(out, "HTTP/1.0 302 Found\r\nLocation: http://{a}.{b}.{c}.{d}/\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n")?;
        }
        (Refused::Host, ..) => {
            status_line(out, "403 Forbidden", "text/plain")?;
            out.write_str("not the clock's name: use its address, or its hostname alone or with .lan or .local\n")?;
        }
        (Refused::Origin, ..) => {
            status_line(out, "403 Forbidden", "text/plain")?;
            out.write_str("a request from another site's page\n")?;
        }
    }
    Ok(Then::Close)
}

/// The socket as a body reader that counts what was taken from it, so a
/// refusal knows how much is left to drain.
struct Counting<'a, 'b> {
    socket: &'a mut TcpSocket<'b>,
    taken: usize,
}

impl embedded_io_async::ErrorType for Counting<'_, '_> {
    type Error = embassy_net::tcp::Error;
}

impl Read for Counting<'_, '_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let n = self.socket.read(buf).await?;
        self.taken += n;
        Ok(n)
    }
}

/// Read and discard `remaining` body bytes, within the caps, using the
/// response buffer as scratch before it is written. Stops early on a
/// client that goes quiet or away.
async fn drain(socket: &mut TcpSocket<'_>, out: &mut Response, remaining: usize) {
    if remaining == 0 || remaining > DRAIN_MAX {
        return;
    }
    let mut left = remaining;
    let drained = with_timeout(DRAIN_TIMEOUT, async {
        while left > 0 {
            let want = left.min(out.buf.len());
            match socket.read(&mut out.buf[..want]).await {
                Ok(0) | Err(_) => break,
                Ok(n) => left -= n,
            }
        }
    })
    .await;
    if drained.is_err() || left > 0 {
        warn!("http: drained {} of {remaining} refused body bytes", remaining - left);
    }
}

/// A firmware upload: the credentials first, then the body streamed from
/// the socket into the other slot. The response says what happened; on
/// success the caller reboots once it is sent. A refusal first takes the
/// rest of the body off the socket, except when another upload is
/// running and needs the air.
async fn upload(out: &mut Response, req: &Request<'_>, socket: &mut TcpSocket<'_>) -> Result<Then, core::fmt::Error> {
    let Body::Stream { first, total } = req.body else {
        return Ok(Then::Close);
    };
    let mut body = Counting { socket, taken: 0 };
    // What was written, for the answer: a firmware image, or a clip
    // pack (`POST /clips`), after either of which the clock restarts.
    enum Written {
        Image(ota::Done),
        Clips(pack::Done),
    }
    let result = match auth::token_or_pin(req.authorization) {
        Ok(()) if req.path == "/clips" => pack::install(first, total, &mut body).await.map(Written::Clips),
        Ok(()) => ota::install(first, total, &mut body).await.map(Written::Image),
        Err(e) => {
            warn!("http: upload refused: {}", e.text);
            Err(e)
        }
    };
    let consumed = first.len() + body.taken;
    if let Err(e) = &result
        && !e.status.starts_with("503")
    {
        drain(socket, out, total.saturating_sub(consumed)).await;
    }
    match result {
        Ok(Written::Image(done)) => {
            status_line(out, "200 OK", "text/plain")?;
            writeln!(out, "updated: {} written to {}, {} bytes; rebooting", done.version, done.slot.name(), done.bytes)?;
            Ok(Then::NewImage)
        }
        Ok(Written::Clips(done)) => {
            status_line(out, "200 OK", "text/plain")?;
            writeln!(out, "clips: a pack of {} clips written, {} bytes; rebooting", done.clips, done.bytes)?;
            // A new pack: if the clip task hung in the old one, try these.
            watchdog::trust_clips();
            Ok(Then::Reload)
        }
        Err(e) => {
            status_line(out, e.status, "text/plain")?;
            writeln!(out, "{}", e.text)?;
            Ok(Then::Close)
        }
    }
}

/// Serve the page and the API on `HTTP_PORT` once the network is up.
/// Spawn it `LISTENERS` times.
#[embassy_executor::task(pool_size = LISTENERS)]
pub async fn http_task(stack: Stack<'static>, index: usize) -> ! {
    let mut rx = [0u8; RX_BUF];
    let mut tx = [0u8; TX_BUF];
    let mut request = [0u8; REQUEST_BUF];
    stack.wait_config_up().await;
    if index == 0 {
        info!("http: serving on port {HTTP_PORT}");
    }
    let me = &LISTENER[index];
    loop {
        me.set(State::Waiting);
        let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
        socket.set_timeout(Some(TIMEOUT));
        if let Err(e) = socket.accept(HTTP_PORT).await {
            warn!("http: L{index} accept: {e:?}");
            continue;
        }
        bump(&me.accepted);
        me.set(State::Reading);
        let req = match with_timeout(HEAD_TIMEOUT, read_request(&mut socket, &mut request)).await {
            Ok(req) => req,
            Err(_) => {
                bump(&me.head_timeouts);
                info!("http: L{index} no request within {} s", HEAD_TIMEOUT.as_secs());
                Err(NoRequest::Close)
            }
        };
        let mut then = Then::Close;
        // The one response buffer of the loop: a second one for the
        // refusal cost 6 KB of static RAM per listener (2026-11-07).
        let mut out = Response::new();
        let served = match req {
            Ok(req) => {
                info!("http: L{index} {:?} {}", req.method, req.path);
                let hostname = settings::get().hostname;
                let own = Own {
                    ip: stack.config_v4().map(|c| c.address.address().octets()),
                    hostname: hostname.as_str(),
                    ap: net::mode() == Mode::AccessPoint,
                };
                let admitted = admit(req.method, req.host, req.origin, &own);
                let built = match (admitted, &req.body) {
                    (Err(refused), _) => turn_away(&mut out, &req, refused, own.ip, &mut socket).await,
                    (Ok(()), Body::Inline(_)) if req.method == Method::Post && req.path == "/scan" => rescan(&mut out).await,
                    (Ok(()), Body::Inline(body)) => respond(&mut out, &req, body),
                    (Ok(()), Body::Stream { .. }) => upload(&mut out, &req, &mut socket).await,
                };
                match built {
                    Ok(t) => then = t,
                    Err(_) => {
                        warn!("http: response did not fit");
                        out = Response::new();
                        let _ = status_line(&mut out, "500 Internal Server Error", "text/plain");
                    }
                }
                // A request turned away is not one served: it must not
                // confirm a new image.
                admitted.is_ok()
            }
            Err(NoRequest::Refuse(status)) => {
                warn!("http: L{index} refused: {status}");
                let _ = status_line(&mut out, status, "text/plain");
                let _ = out.write_str(status);
                let _ = out.write_str("\n");
                false
            }
            Err(NoRequest::Close) => false,
        };
        if !out.as_bytes().is_empty() {
            me.set(State::Writing);
            if let Err(e) = socket.write_all(out.as_bytes()).await {
                warn!("http: L{index} write: {e:?}");
            }
        }
        if served {
            bump(&me.served);
            // Serving a request is proof enough that a new image works.
            ota::confirm();
        }
        me.set(State::Closing);
        socket.close();
        let _ = socket.flush().await;
        match then {
            Then::Close => {}
            Then::NewImage => ota::reboot().await,
            Then::Reload => heal::reboot().await,
            Then::Join => heal::switch_mode(false).await,
            Then::AccessPoint => heal::switch_mode(true).await,
        }
    }
}

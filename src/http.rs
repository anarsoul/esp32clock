//! The settings page and API: a few TCP sockets, one request per
//! connection, responses built whole and written once. Runs in thread
//! mode and never touches the scan path, except that a firmware upload
//! (`POST /ota`, streamed into `ota::install`) stops it for the write.
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

use crate::buzzer;
use crate::config::HTTP_PORT;
use crate::form::{self, FormError, HealStamp, Status};
use crate::{clock, heal, ota, settings};

/// Connections served at once, each with its own buffers and task.
pub const LISTENERS: usize = 3;

/// The receive buffer is the TCP window, and a firmware upload is 800 KB
/// of body; 1 KB throttled it to one segment per round trip.
const RX_BUF: usize = 4096;
const TX_BUF: usize = 1024;
const REQUEST_BUF: usize = 1536;
const BODY_MAX: usize = 512;
const RESPONSE_BUF: usize = 4096;

/// A client that stalls this long mid-transfer is dropped.
const TIMEOUT: Duration = Duration::from_secs(5);

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

impl core::fmt::Write for Response {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.len + s.len();
        if end > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Method {
    Get,
    Post,
}

/// The body of a request: read whole into the request buffer for the
/// small ones, or left on the socket for a firmware upload, with what
/// followed the head in the buffer and the total to expect.
enum Body<'a> {
    Inline(&'a [u8]),
    Stream { first: &'a [u8], total: usize },
}

/// What was asked. `query` is what followed `?` in the target, without
/// the `?`; `authorization` is that header's value, if sent.
struct Request<'a> {
    method: Method,
    path: &'a str,
    query: &'a str,
    authorization: Option<&'a str>,
    body: Body<'a>,
}

/// The request line, `Content-Length` and `Authorization`, borrowed
/// from the head.
struct Head<'a> {
    method: Method,
    path: &'a str,
    query: &'a str,
    authorization: Option<&'a str>,
    content_length: usize,
}

fn parse_head(head: &str) -> Option<Head<'_>> {
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut words = request_line.split(' ');
    let method = match words.next()? {
        "GET" => Method::Get,
        "POST" => Method::Post,
        _ => return None,
    };
    let target = words.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut content_length = 0;
    let mut authorization = None;
    for (key, value) in lines.filter_map(|l| l.split_once(':')) {
        if key.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse().ok()?;
        } else if key.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.trim());
        }
    }
    Some(Head { method, path, query, authorization, content_length })
}

/// Read the head and the body. `None` for anything that is not a request
/// this server answers, in which case the connection is just closed.
///
/// The head is parsed twice: once for the two facts that decide how the
/// body is read (a firmware upload's body stays on the socket), so that
/// its borrow of the buffer ends before the body is read into it, and
/// once more afterwards, on the same bytes, for the request itself.
async fn read_request<'a>(socket: &mut TcpSocket<'_>, buf: &'a mut [u8; REQUEST_BUF]) -> Option<Request<'a>> {
    let mut n = 0;
    let head_end = loop {
        if n == buf.len() {
            return None;
        }
        let got = socket.read(&mut buf[n..]).await.ok()?;
        if got == 0 {
            return None;
        }
        n += got;
        if let Some(i) = buf[..n].windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };

    let (upload, content_length) = {
        let head = parse_head(core::str::from_utf8(&buf[..head_end]).ok()?)?;
        (head.method == Method::Post && head.path == "/ota", head.content_length)
    };
    if !upload {
        if content_length > BODY_MAX || head_end + content_length > buf.len() {
            return None;
        }
        while n < head_end + content_length {
            let got = socket.read(&mut buf[n..]).await.ok()?;
            if got == 0 {
                return None;
            }
            n += got;
        }
    }

    let (head_bytes, rest) = buf.split_at(head_end);
    let head = parse_head(core::str::from_utf8(head_bytes).ok()?)?;
    let body = if upload {
        Body::Stream { first: &rest[..n - head_end], total: content_length }
    } else {
        Body::Inline(&rest[..content_length])
    };
    Some(Request { method: head.method, path: head.path, query: head.query, authorization: head.authorization, body })
}

fn status_line(out: &mut Response, status: &str, content_type: &str) -> core::fmt::Result {
    write!(out, "HTTP/1.0 {status}\r\nContent-Type: {content_type}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n")
}

/// Back to the page, with the error of a rejected form in the query so
/// the page can say that nothing was saved.
fn redirect_home(out: &mut Response, error: Option<FormError>) -> core::fmt::Result {
    let suffix = match error {
        None => "",
        Some(FormError::BadValue) => "?error=value",
        Some(FormError::UnknownKey) => "?error=key",
    };
    write!(out, "HTTP/1.0 303 See Other\r\nLocation: /{suffix}\r\nConnection: close\r\n\r\n")
}

/// The error the page was redirected back with, from `?error=...`.
fn error_from_query(query: &str) -> Option<FormError> {
    query.split('&').find_map(|pair| match pair.split_once('=') {
        Some(("error", "value")) => Some(FormError::BadValue),
        Some(("error", "key")) => Some(FormError::UnknownKey),
        _ => None,
    })
}

/// The read-only status for the page and the JSON.
fn status() -> Status {
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
    Status {
        version: ota::VERSION,
        slot: ota::booted().name(),
        uptime_secs: embassy_time::Instant::now().as_secs(),
        heals: heal::count(),
        last_heal,
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
}

/// Build the response for one request with its body in hand.
fn respond(out: &mut Response, req: &Request<'_>, body: &[u8]) -> Result<Then, core::fmt::Error> {
    match (req.method, req.path) {
        (Method::Get, "/") => {
            status_line(out, "200 OK", "text/html; charset=utf-8")?;
            form::write_page(out, &settings::get(), &status(), error_from_query(req.query))?;
        }
        (Method::Post, "/") => {
            // All or nothing: the form is applied to a copy and committed
            // only if every pair parsed, so a bad field (an alarm time the
            // browser sent empty, say) cannot leave the checkboxes, which
            // `apply` resets first, half applied.
            let mut trial = settings::get();
            let result = form::apply(&mut trial, body, true);
            match result {
                Ok(()) => {
                    settings::update(|s| *s = trial);
                }
                Err(e) => warn!("http: form rejected: {e:?}"),
            }
            redirect_home(out, result.err())?;
        }
        (Method::Post, "/play") => {
            // Preview the sound chosen in the form without saving anything:
            // the form is applied to a scratch copy and only its melody used.
            let mut trial = settings::get();
            if let Err(e) = form::apply(&mut trial, body, true) {
                warn!("http: play form rejected: {e:?}");
            }
            buzzer::preview(trial.melody);
            redirect_home(out, None)?;
        }
        (Method::Get, "/update") => {
            // Opening the page puts a PIN on the panel; the page asks for it.
            let lockout_secs = ota::issue_pin().err();
            status_line(out, "200 OK", "text/html; charset=utf-8")?;
            form::write_update_page(out, &status(), lockout_secs)?;
        }
        (Method::Get, "/api") => {
            status_line(out, "200 OK", "application/json")?;
            form::write_json(out, &settings::get(), &status())?;
        }
        (Method::Post, "/api") => {
            // Validate on a copy first so a bad pair changes nothing.
            let mut trial = settings::get();
            match form::apply(&mut trial, body, false) {
                Ok(()) => {
                    settings::update(|s| *s = trial);
                    status_line(out, "200 OK", "application/json")?;
                    form::write_json(out, &trial, &status())?;
                }
                Err(e) => {
                    status_line(out, "400 Bad Request", "text/plain")?;
                    out.write_str(match e {
                        FormError::UnknownKey => "unknown key\n",
                        FormError::BadValue => "bad value\n",
                    })?;
                }
            }
        }
        (Method::Post, "/reload") => {
            // A deliberate reset for testing the carry: the token, not the
            // PIN, and no hold for a ringing alarm.
            return match ota::authorize_token(req.authorization) {
                Ok(()) => {
                    warn!("http: reload requested, rebooting with the time carried over");
                    status_line(out, "200 OK", "text/plain")?;
                    out.write_str("rebooting with the time carried over\n")?;
                    Ok(Then::Reload)
                }
                Err(e) => {
                    warn!("http: reload refused: {}", e.text);
                    status_line(out, e.status, "text/plain")?;
                    writeln!(out, "{}", e.text)?;
                    Ok(Then::Close)
                }
            };
        }
        _ => {
            status_line(out, "404 Not Found", "text/plain")?;
            out.write_str("not found\n")?;
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
    let result = match ota::authorize(req.authorization) {
        Ok(()) => ota::install(first, total, &mut body).await,
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
        Ok(done) => {
            status_line(out, "200 OK", "text/plain")?;
            writeln!(out, "updated: {} written to {}, {} bytes; rebooting", done.version, done.slot.name(), done.bytes)?;
            Ok(Then::NewImage)
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
                None
            }
        };
        let mut then = Then::Close;
        if let Some(req) = req {
            info!("http: L{index} {:?} {}", req.method, req.path);
            let mut out = Response::new();
            let built = match req.body {
                Body::Inline(body) => respond(&mut out, &req, body),
                Body::Stream { .. } => upload(&mut out, &req, &mut socket).await,
            };
            match built {
                Ok(t) => then = t,
                Err(_) => {
                    warn!("http: response did not fit");
                    out = Response::new();
                    let _ = status_line(&mut out, "500 Internal Server Error", "text/plain");
                }
            }
            me.set(State::Writing);
            if let Err(e) = socket.write_all(out.as_bytes()).await {
                warn!("http: L{index} write: {e:?}");
            }
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
        }
    }
}

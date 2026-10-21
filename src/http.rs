//! The settings page and API: a few TCP sockets, one request per
//! connection, responses built whole and written once. Runs in thread
//! mode and never touches the scan path.
//!
//! Several sockets listen at once because a browser opens more
//! connections than it uses: a speculative one alongside the page, one
//! for the favicon, a fresh one for the redirect after a form post. With
//! a single socket each of those was a refused connection for the next
//! real request.

use core::fmt::Write as _;

use embassy_net::Stack;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, with_timeout};
use embedded_io_async::Write;
use log::{info, warn};

use crate::buzzer;
use crate::config::HTTP_PORT;
use crate::form::{self, FormError};
use crate::settings;

/// Connections served at once, each with its own buffers and task.
pub const LISTENERS: usize = 3;

const RX_BUF: usize = 1024;
const TX_BUF: usize = 1024;
const REQUEST_BUF: usize = 1536;
const BODY_MAX: usize = 512;
const RESPONSE_BUF: usize = 2560;

/// A client that stalls this long mid-transfer is dropped.
const TIMEOUT: Duration = Duration::from_secs(5);

/// A client that has connected but not sent its request within this long
/// is dropped, so an idle speculative connection frees its socket.
const HEAD_TIMEOUT: Duration = Duration::from_secs(2);

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

/// What was asked. The body is a slice of the request buffer; `query` is
/// what followed `?` in the target, without the `?`.
struct Request<'a> {
    method: Method,
    path: &'a str,
    query: &'a str,
    body: &'a [u8],
}

/// The request line and `Content-Length`, as positions in the buffer so
/// that the borrow of the head ends before the body is read into it.
struct Head {
    method: Method,
    target_start: usize,
    path_len: usize,
    target_len: usize,
    content_length: usize,
}

fn parse_head(head: &str) -> Option<Head> {
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut words = request_line.split(' ');
    let method = match words.next()? {
        "GET" => Method::Get,
        "POST" => Method::Post,
        _ => return None,
    };
    let target = words.next()?;
    let target_start = target.as_ptr() as usize - head.as_ptr() as usize;
    let path_len = target.find('?').unwrap_or(target.len());
    let content_length = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0);
    Some(Head { method, target_start, path_len, target_len: target.len(), content_length })
}

/// Read the head and the body. `None` for anything that is not a request
/// this server answers, in which case the connection is just closed.
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

    let head = parse_head(core::str::from_utf8(&buf[..head_end]).ok()?)?;
    if head.content_length > BODY_MAX || head_end + head.content_length > buf.len() {
        return None;
    }
    while n < head_end + head.content_length {
        let got = socket.read(&mut buf[n..]).await.ok()?;
        if got == 0 {
            return None;
        }
        n += got;
    }

    let (head_bytes, rest) = buf.split_at(head_end);
    let target = core::str::from_utf8(&head_bytes[head.target_start..head.target_start + head.target_len]).ok()?;
    let (path, query) = target.split_at(head.path_len);
    let query = query.strip_prefix('?').unwrap_or(query);
    Some(Request { method: head.method, path, query, body: &rest[..head.content_length] })
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

/// Build the response for one request.
fn respond(out: &mut Response, req: &Request<'_>) -> core::fmt::Result {
    match (req.method, req.path) {
        (Method::Get, "/") => {
            status_line(out, "200 OK", "text/html; charset=utf-8")?;
            form::write_page(out, &settings::get(), error_from_query(req.query))
        }
        (Method::Post, "/") => {
            // All or nothing: the form is applied to a copy and committed
            // only if every pair parsed, so a bad field (an alarm time the
            // browser sent empty, say) cannot leave the checkboxes, which
            // `apply` resets first, half applied.
            let mut trial = settings::get();
            let result = form::apply(&mut trial, req.body, true);
            match result {
                Ok(()) => {
                    settings::update(|s| *s = trial);
                }
                Err(e) => warn!("http: form rejected: {e:?}"),
            }
            redirect_home(out, result.err())
        }
        (Method::Post, "/play") => {
            // Preview the sound chosen in the form without saving anything:
            // the form is applied to a scratch copy and only its melody used.
            let mut trial = settings::get();
            if let Err(e) = form::apply(&mut trial, req.body, true) {
                warn!("http: play form rejected: {e:?}");
            }
            buzzer::preview(trial.melody);
            redirect_home(out, None)
        }
        (Method::Get, "/api") => {
            status_line(out, "200 OK", "application/json")?;
            form::write_json(out, &settings::get())
        }
        (Method::Post, "/api") => {
            // Validate on a copy first so a bad pair changes nothing.
            let mut trial = settings::get();
            match form::apply(&mut trial, req.body, false) {
                Ok(()) => {
                    settings::update(|s| *s = trial);
                    status_line(out, "200 OK", "application/json")?;
                    form::write_json(out, &trial)
                }
                Err(e) => {
                    status_line(out, "400 Bad Request", "text/plain")?;
                    out.write_str(match e {
                        FormError::UnknownKey => "unknown key\n",
                        FormError::BadValue => "bad value\n",
                    })
                }
            }
        }
        _ => {
            status_line(out, "404 Not Found", "text/plain")?;
            out.write_str("not found\n")
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
    loop {
        let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
        socket.set_timeout(Some(TIMEOUT));
        if let Err(e) = socket.accept(HTTP_PORT).await {
            warn!("http: accept: {e:?}");
            continue;
        }
        let req = with_timeout(HEAD_TIMEOUT, read_request(&mut socket, &mut request)).await.ok().flatten();
        if let Some(req) = req {
            info!("http: {:?} {}", req.method, req.path);
            let mut out = Response::new();
            if respond(&mut out, &req).is_err() {
                warn!("http: response did not fit");
                out = Response::new();
                let _ = status_line(&mut out, "500 Internal Server Error", "text/plain");
            }
            if let Err(e) = socket.write_all(out.as_bytes()).await {
                warn!("http: write: {e:?}");
            }
        }
        socket.close();
        let _ = socket.flush().await;
    }
}

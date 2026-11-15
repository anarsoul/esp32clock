// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The part of the HTTP server that needs no socket: the request head
//! parser and the response buffer's size, which the pages are checked
//! against on the host.

/// The response buffer: the whole page plus headers must fit, and
/// overflow is a 500, not a truncation. The worst-case pages are
/// measured by the tests in `form`. One per listener task; raised
/// from 4096 to 6 KB for the settings page's unsaved-changes script,
/// and to 8 KB on 2026-11-15 (the user's figure) for the clock page's
/// day checkboxes and the script that posts a form and keeps what was
/// typed, with room for what comes after.
pub const RESPONSE_BUF: usize = 8 * 1024;

/// What a page body may take of it: the status line and headers the
/// firmware writes first are 126 bytes for a page (`HEADERS_MAX`),
/// rounded up with room to grow.
pub const PAGE_MAX: usize = RESPONSE_BUF - 160;

/// The longest head `status_line` writes: `200 OK` with the pages'
/// content type.
pub const HEADERS_MAX: usize = 126;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    Get,
    Post,
}

/// The request line, `Content-Length`, `Authorization`, `Host`,
/// `Origin` and whether a `Transfer-Encoding` was given, borrowed from
/// the head.
pub struct Head<'a> {
    pub method: Method,
    pub path: &'a str,
    pub query: &'a str,
    pub authorization: Option<&'a str>,
    /// `None` when the header is absent: a `POST` without it is not an
    /// empty form but a request whose body the server cannot read.
    pub content_length: Option<usize>,
    /// A `Transfer-Encoding` header was present. The server reads no
    /// chunked body and refuses the request.
    pub transfer_encoding: bool,
    /// The name the client asked for, as sent; see `admit`.
    pub host: Option<&'a str>,
    /// The origin of the page that made the request, as a browser says
    /// it on every cross-origin `POST`; see `admit`.
    pub origin: Option<&'a str>,
}

/// Parse the head of a request: the request line and the headers the
/// server reads. `None` for anything but `GET` or `POST`, a
/// `Content-Length` that is not a number, or a second `Host` or
/// `Origin`, which would leave it open which of them was checked.
pub fn parse_head(head: &str) -> Option<Head<'_>> {
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
    let mut content_length = None;
    let mut authorization = None;
    let mut transfer_encoding = false;
    let mut host = None;
    let mut origin = None;
    for (key, value) in lines.filter_map(|l| l.split_once(':')) {
        if key.eq_ignore_ascii_case("content-length") {
            content_length = Some(value.trim().parse().ok()?);
        } else if key.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.trim());
        } else if key.eq_ignore_ascii_case("transfer-encoding") {
            transfer_encoding = true;
        } else if key.eq_ignore_ascii_case("host") {
            if host.replace(value.trim()).is_some() {
                return None;
            }
        } else if key.eq_ignore_ascii_case("origin") && origin.replace(value.trim()).is_some() {
            return None;
        }
    }
    Some(Head { method, path, query, authorization, content_length, transfer_encoding, host, origin })
}

/// What the clock answers to: its address and its name, for `admit`.
#[derive(Clone, Copy, Debug)]
pub struct Own<'a> {
    /// The clock's IPv4 address, if it has one.
    pub ip: Option<[u8; 4]>,
    /// The hostname setting.
    pub hostname: &'a str,
    /// On the access point the captive DNS answers every name with the
    /// clock, so no name is the clock's own there, only the address.
    pub ap: bool,
}

/// The suffixes the hostname is accepted with, besides bare: the ones
/// a home router gives its DHCP clients. Neither can be registered, so
/// no public name ends in them. The user's list of 2026-11-14.
pub const HOST_SUFFIXES: [&str; 2] = [".lan", ".local"];

/// Why `admit` refused a request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// `Host` is not the clock: a name that came to resolve to the
    /// clock's address (DNS rebinding), or a LAN's own domain. A `GET`
    /// is sent to the address; a `POST` is `403`.
    Host,
    /// A `POST` from another site's page: `403`.
    Origin,
}

/// `authority` (a `Host` value, or what follows `http://` in an
/// `Origin`) without its port and trailing dot.
fn name_of(authority: &str) -> &str {
    let name = match authority.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => authority,
    };
    name.strip_suffix('.').unwrap_or(name)
}

/// Whether `name` is `ip` in the one form a browser sends: four decimal
/// octets without leading zeros.
fn is_ip(name: &str, ip: [u8; 4]) -> bool {
    let mut parts = name.split('.');
    for octet in ip {
        match parts.next() {
            Some(p) if !p.is_empty() && p.len() <= 3 && (p == "0" || !p.starts_with('0')) && p.bytes().all(|b| b.is_ascii_digit()) => {
                if p.parse() != Ok(octet as u16) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    parts.next().is_none()
}

/// Whether `authority` names the clock: its address, or on the station
/// its hostname, bare or with one of `HOST_SUFFIXES`, in any case.
fn is_own(authority: &str, own: &Own<'_>) -> bool {
    let name = name_of(authority);
    if own.ip.is_some_and(|ip| is_ip(name, ip)) {
        return true;
    }
    if own.ap || own.hostname.is_empty() || name.len() < own.hostname.len() || !name.is_char_boundary(own.hostname.len()) {
        return false;
    }
    let (head, suffix) = name.split_at(own.hostname.len());
    head.eq_ignore_ascii_case(own.hostname) && (suffix.is_empty() || HOST_SUFFIXES.iter().any(|s| suffix.eq_ignore_ascii_case(s)))
}

/// Whether a request is one the clock's own pages or a script made,
/// from the two headers a browser sets and a page cannot.
///
/// `Host` must name the clock (`is_own`) or be absent, as from a client
/// that is no browser: a page served under any other name that
/// resolves to the clock is another origin's page with the clock's
/// address, and would otherwise be let read `/api` and guess the PIN.
/// On a `POST`, `Origin` must be absent (`curl`, the scripts; a browser
/// sends it on every cross-origin post, `null` when it hides it) or
/// `http://` and a name of the clock: a form on another site is
/// otherwise a request the browser sends without asking.
pub fn admit(method: Method, host: Option<&str>, origin: Option<&str>, own: &Own<'_>) -> Result<(), Refused> {
    if host.is_some_and(|h| !is_own(h, own)) {
        return Err(Refused::Host);
    }
    match (method, origin) {
        (Method::Post, Some(o)) if !o.strip_prefix("http://").is_some_and(|a| is_own(a, own)) => Err(Refused::Origin),
        _ => Ok(()),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_head() {
        let h = parse_head("POST /ota HTTP/1.0\r\nHost: clock\r\ncontent-length: 831488\r\nAUTHORIZATION:  Bearer abc \r\n\r\n").unwrap();
        assert_eq!(h.method, Method::Post);
        assert_eq!(h.path, "/ota");
        assert_eq!(h.query, "");
        assert_eq!(h.content_length, Some(831488));
        assert_eq!(h.authorization, Some("Bearer abc"));
        assert!(!h.transfer_encoding);
        assert_eq!((h.host, h.origin), (Some("clock"), None));
    }

    #[test]
    fn host_and_origin_are_kept_and_never_twice() {
        let h = parse_head("POST / HTTP/1.1\r\nhost: 10.1.1.30:80 \r\nORIGIN: http://evil.example\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert_eq!((h.host, h.origin), (Some("10.1.1.30:80"), Some("http://evil.example")));
        assert!(parse_head("GET / HTTP/1.1\r\nHost: clock\r\nHost: evil.example\r\n\r\n").is_none());
        assert!(parse_head("POST / HTTP/1.1\r\nOrigin: http://clock\r\nOrigin: null\r\n\r\n").is_none());
    }

    const STATION: Own<'static> = Own { ip: Some([10, 1, 1, 30]), hostname: "white-clock", ap: false };
    const AP: Own<'static> = Own { ip: Some([192, 168, 4, 1]), hostname: "white-clock", ap: true };

    #[test]
    fn the_clocks_own_names_are_admitted() {
        for host in [
            "10.1.1.30",
            "10.1.1.30:80",
            "10.1.1.30.",
            "white-clock",
            "WHITE-Clock",
            "white-clock.lan",
            "white-clock.LAN:80",
            "white-clock.local",
            "white-clock.local.",
        ] {
            assert_eq!(admit(Method::Get, Some(host), None, &STATION), Ok(()), "{host}");
            assert_eq!(admit(Method::Post, Some(host), None, &STATION), Ok(()), "{host}, a script");
            let origin = format!("http://{host}");
            assert_eq!(admit(Method::Post, Some(host), Some(&origin), &STATION), Ok(()), "{host}, its own page");
        }
        // No Host at all is no browser.
        assert_eq!(admit(Method::Post, None, None, &STATION), Ok(()));
        // A page opened by name may post to the address: both are the clock.
        assert_eq!(admit(Method::Post, Some("10.1.1.30"), Some("http://white-clock.lan"), &STATION), Ok(()));
        assert_eq!(admit(Method::Get, Some("192.168.4.1"), None, &AP), Ok(()));
        assert_eq!(admit(Method::Post, Some("192.168.4.1"), Some("http://192.168.4.1"), &AP), Ok(()));
    }

    #[test]
    fn other_names_are_refused() {
        for host in [
            "",
            "evil.example",
            "white-clock.evil.example",
            "white-clock.lan.evil.example",
            "white-clock.lan.local",
            "white-clockx",
            "white-clock.lanx",
            "xwhite-clock",
            "white-cloc",
            "10.1.1.31",
            "10.1.1.30.5",
            "10.1.1",
            "010.1.1.30",
            "10.1.1.+30",
            "10.1.1.30:x",
            "[::1]",
            "white-cloc\u{e9}",
            "white-clock\u{e9}",
        ] {
            assert_eq!(admit(Method::Get, Some(host), None, &STATION), Err(Refused::Host), "{host:?}");
            assert_eq!(admit(Method::Post, Some(host), None, &STATION), Err(Refused::Host), "{host:?}");
        }
        // Rebinding: the page and the request agree, and neither is the clock.
        assert_eq!(admit(Method::Post, Some("evil.example"), Some("http://evil.example"), &STATION), Err(Refused::Host));
        // On the access point every name resolves to the clock.
        for host in ["white-clock", "white-clock.lan", "captive.apple.com", "10.1.1.30"] {
            assert_eq!(admit(Method::Get, Some(host), None, &AP), Err(Refused::Host), "{host}");
        }
        // A clock without an address answers to no address.
        let none = Own { ip: None, ..STATION };
        assert_eq!(admit(Method::Get, Some("10.1.1.30"), None, &none), Err(Refused::Host));
        assert_eq!(admit(Method::Get, Some("white-clock.lan"), None, &none), Ok(()));
    }

    #[test]
    fn a_post_from_another_page_is_refused() {
        for origin in [
            "http://evil.example",
            "https://evil.example",
            "null",
            "",
            "https://10.1.1.30",
            "10.1.1.30",
            "http://",
            "http://10.1.1.30/",
            "http://white-clock.evil.example",
            "http://10.1.1.31",
        ] {
            assert_eq!(admit(Method::Post, Some("10.1.1.30"), Some(origin), &STATION), Err(Refused::Origin), "{origin:?}");
            assert_eq!(admit(Method::Post, None, Some(origin), &STATION), Err(Refused::Origin), "{origin:?}, no Host");
            // A GET changes nothing, and a browser sends `Origin` on some.
            assert_eq!(admit(Method::Get, Some("10.1.1.30"), Some(origin), &STATION), Ok(()), "{origin:?}");
        }
        assert_eq!(admit(Method::Post, Some("192.168.4.1"), Some("http://white-clock.lan"), &AP), Err(Refused::Origin));
    }

    #[test]
    fn query_is_split_off_and_headers_are_optional() {
        let h = parse_head("GET /?error=value HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(h.method, Method::Get);
        assert_eq!(h.path, "/");
        assert_eq!(h.query, "error=value");
        assert_eq!(h.content_length, None, "absent, not zero");
        assert_eq!(h.authorization, None);
        let chunked = parse_head("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
        assert!(chunked.transfer_encoding && chunked.content_length.is_none());
        let empty = parse_head("POST / HTTP/1.0\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert_eq!(empty.content_length, Some(0), "an explicit empty body is a body");
        let bare = parse_head("GET /api").unwrap();
        assert_eq!((bare.path, bare.query), ("/api", ""));
    }

    #[test]
    fn refused_heads() {
        assert!(parse_head("").is_none());
        assert!(parse_head("PUT / HTTP/1.0\r\n\r\n").is_none());
        assert!(parse_head("GET\r\n\r\n").is_none(), "no target");
        assert!(parse_head("POST /ota HTTP/1.0\r\nContent-Length: lots\r\n\r\n").is_none());
        assert!(parse_head("POST /ota HTTP/1.0\r\nContent-Length: -1\r\n\r\n").is_none());
    }

    #[test]
    fn upload_is_decided_by_method_and_exact_path() {
        let is_upload = |head: &str| parse_head(head).map(|h| h.method == Method::Post && h.path == "/ota");
        assert_eq!(is_upload("POST /ota HTTP/1.0\r\n\r\n"), Some(true));
        assert_eq!(is_upload("GET /ota HTTP/1.0\r\n\r\n"), Some(false));
        assert_eq!(is_upload("POST /otax HTTP/1.0\r\n\r\n"), Some(false));
        assert_eq!(is_upload("POST /ota?x=1 HTTP/1.0\r\n\r\n"), Some(true));
    }

    const _: () = assert!(PAGE_MAX + HEADERS_MAX <= RESPONSE_BUF);
}

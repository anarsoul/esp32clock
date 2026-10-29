//! The part of the HTTP server that needs no socket: the request head
//! parser and the response buffer's size, which the pages are checked
//! against on the host.

/// The response buffer: the whole page plus headers must fit, and
/// overflow is a 500, not a truncation. The worst-case pages are
/// measured by the tests in `form`.
pub const RESPONSE_BUF: usize = 4096;

/// What a page body may take of it: the status line and headers the
/// firmware writes first are 88 bytes, rounded up with room to grow.
pub const PAGE_MAX: usize = RESPONSE_BUF - 128;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    Get,
    Post,
}

/// The request line, `Content-Length` and `Authorization`, borrowed
/// from the head.
pub struct Head<'a> {
    pub method: Method,
    pub path: &'a str,
    pub query: &'a str,
    pub authorization: Option<&'a str>,
    pub content_length: usize,
}

/// Parse the head of a request: the request line and the two headers
/// the server reads. `None` for anything but `GET` or `POST`, or a
/// `Content-Length` that is not a number.
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_head() {
        let h = parse_head("POST /ota HTTP/1.0\r\nHost: clock\r\ncontent-length: 831488\r\nAUTHORIZATION:  Bearer abc \r\n\r\n").unwrap();
        assert_eq!(h.method, Method::Post);
        assert_eq!(h.path, "/ota");
        assert_eq!(h.query, "");
        assert_eq!(h.content_length, 831488);
        assert_eq!(h.authorization, Some("Bearer abc"));
    }

    #[test]
    fn query_is_split_off_and_headers_are_optional() {
        let h = parse_head("GET /?error=value HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(h.method, Method::Get);
        assert_eq!(h.path, "/");
        assert_eq!(h.query, "error=value");
        assert_eq!(h.content_length, 0);
        assert_eq!(h.authorization, None);
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

    /// The status line and headers the firmware writes are 88 bytes.
    const _: () = assert!(PAGE_MAX + 88 <= RESPONSE_BUF);
}

//! Who may do what over HTTP: the API token and the panel PIN.
//!
//! The token is a setting (`settings::Settings::token`), seeded from
//! `API_TOKEN` at build time and settable later with the PIN, never
//! shown. A script sends it as `Authorization: Bearer <token>` for a
//! firmware upload, `/reload`, the Wi-Fi credentials on `/api` and a
//! new token on `/token`. The PIN is four digits the clock shows on its
//! panel when the update page is opened (or `POST /pin` is sent), good
//! for one attempt, so whoever uses it can see the clock; it stands in
//! for the token on `/ota` and `/token`, the two things a clock with no
//! token built in needs it for. The rules are pure, with the clock and
//! the random number passed in, so they run on the host.

use core::cell::RefCell;

use critical_section::Mutex as CsMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::Instant;
use log::{info, warn};

use crate::settings;

/// Whether a token is set at all. The page's PIN works regardless.
pub fn token_enabled() -> bool {
    !settings::get().token.0.is_empty()
}

/// Compare a presented token with the set one in time that depends only
/// on the lengths, not on where they first differ. An empty token never
/// matches.
pub fn token_ok(presented: &[u8]) -> bool {
    let token = settings::get().token;
    bytes_eq(token.0.as_str().as_bytes(), presented) && !token.0.is_empty()
}

/// Constant-time equality for a short secret: the whole of `secret` is
/// visited whatever `presented` holds.
pub fn bytes_eq(secret: &[u8], presented: &[u8]) -> bool {
    let mut diff = 0u8;
    for (i, &t) in secret.iter().enumerate() {
        diff |= t ^ presented.get(i).copied().unwrap_or(0);
    }
    presented.len() == secret.len() && diff == 0
}

/// A PIN is good for this long after it was issued.
pub const PIN_TTL_MS: u64 = 5 * 60 * 1000;

/// After a wrong PIN no new one is issued for this long.
pub const PIN_LOCKOUT_MS: u64 = 10 * 1000;

/// The PIN the update page must send: four digits the clock shows on
/// its panel when the page is opened. One attempt uses it up, right or
/// wrong, and a wrong one starts the lockout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PinState {
    /// The PIN and when it was issued.
    pending: Option<(u16, u64)>,
    /// No PIN is issued before this time.
    locked_until: Option<u64>,
}

/// Why a PIN was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinError {
    /// Nothing issued, or the last one was used up.
    None,
    Expired,
    Wrong,
}

impl PinState {
    /// Issue a PIN from a random number, or say how many milliseconds
    /// of lockout remain. A new PIN replaces an unused one.
    pub fn issue(&mut self, now_ms: u64, random: u32) -> Result<u16, u64> {
        if let Some(until) = self.locked_until {
            if now_ms < until {
                return Err(until - now_ms);
            }
            self.locked_until = None;
        }
        let pin = (random % 10_000) as u16;
        self.pending = Some((pin, now_ms));
        Ok(pin)
    }

    /// Check the presented digits against the pending PIN, using it up.
    /// One attempt at four digits, then a lockout: nothing to time.
    pub fn check(&mut self, now_ms: u64, presented: &str) -> Result<(), PinError> {
        let Some((pin, issued)) = self.pending.take() else {
            return Err(PinError::None);
        };
        if now_ms.saturating_sub(issued) > PIN_TTL_MS {
            return Err(PinError::Expired);
        }
        if presented.len() == 4 && presented.parse::<u16>() == Ok(pin) {
            Ok(())
        } else {
            self.locked_until = Some(now_ms + PIN_LOCKOUT_MS);
            Err(PinError::Wrong)
        }
    }
}

static PIN: CsMutex<RefCell<PinState>> = CsMutex::new(RefCell::new(PinState { pending: None, locked_until: None }));

/// A PIN for the display task to show, raised by `issue_pin`.
static SHOW_PIN: Signal<CriticalSectionRawMutex, u16> = Signal::new();

/// Issue a PIN from `random`: shown on the panel, logged (the serial
/// port is physical access too, and a headless board has no panel), and
/// remembered for `PIN_TTL_MS`. `Err` is the lockout left, in seconds
/// rounded up, after a wrong attempt.
pub fn issue_pin(random: u32) -> Result<u16, u64> {
    let now_ms = Instant::now().as_millis();
    let result = critical_section::with(|cs| PIN.borrow_ref_mut(cs).issue(now_ms, random));
    match result {
        Ok(pin) => {
            info!("auth: PIN {pin:04} shown on the panel, good for {} min", PIN_TTL_MS / 60_000);
            SHOW_PIN.signal(pin);
            Ok(pin)
        }
        Err(left_ms) => {
            warn!("auth: PIN asked for during the lockout, {} s left", left_ms.div_ceil(1000));
            Err(left_ms.div_ceil(1000))
        }
    }
}

/// The next PIN to show on the panel.
pub async fn pin_to_show() -> u16 {
    SHOW_PIN.wait().await
}

/// Why a request was refused, with the HTTP status it earns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Refusal {
    pub status: &'static str,
    pub text: &'static str,
}

pub const fn refuse(status: &'static str, text: &'static str) -> Refusal {
    Refusal { status, text }
}

/// Check the `Authorization` header, if any: `Bearer <token>` for a
/// script, `PIN <digits>` for the update page.
pub fn token_or_pin(header: Option<&str>) -> Result<(), Refusal> {
    let now_ms = Instant::now().as_millis();
    with_pin(header, |digits| critical_section::with(|cs| PIN.borrow_ref_mut(cs).check(now_ms, digits)))
}

/// Check the header for the token alone; the PIN does not do. For
/// `/reload` and the Wi-Fi credentials, a script's tools.
pub fn token(header: Option<&str>) -> Result<(), Refusal> {
    let Some(token) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
        return Err(refuse("403 Forbidden", "no token: send Authorization: Bearer <API_TOKEN>"));
    };
    if !token_enabled() {
        return Err(refuse("403 Forbidden", "no token set: build one in or set one from the update page"));
    }
    if token_ok(token.trim().as_bytes()) { Ok(()) } else { Err(refuse("403 Forbidden", "bad token")) }
}

/// `token_or_pin` with the PIN check passed in, so the header rules run
/// on the host.
pub fn with_pin(header: Option<&str>, check_pin: impl FnOnce(&str) -> Result<(), PinError>) -> Result<(), Refusal> {
    let Some(header) = header else {
        return Err(refuse("403 Forbidden", "no credentials: a PIN from the update page or the token"));
    };
    if header.starts_with("Bearer ") {
        return token(Some(header));
    }
    if let Some(digits) = header.strip_prefix("PIN ") {
        return match check_pin(digits.trim()) {
            Ok(()) => Ok(()),
            Err(PinError::None) => Err(refuse("403 Forbidden", "no PIN issued: open the update page first")),
            Err(PinError::Expired) => Err(refuse("403 Forbidden", "PIN expired: reload the update page")),
            Err(PinError::Wrong) => Err(refuse("403 Forbidden", "wrong PIN")),
        };
    }
    Err(refuse("403 Forbidden", "bad credentials"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Secret, Text};

    #[test]
    fn constant_time_compare_is_exact() {
        assert!(bytes_eq(b"abcdefgh", b"abcdefgh"));
        assert!(!bytes_eq(b"abcdefgh", b"abcdefg"));
        assert!(!bytes_eq(b"abcdefgh", b"abcdefghi"));
        assert!(!bytes_eq(b"abcdefgh", b"Abcdefgh"));
        assert!(!bytes_eq(b"abcdefgh", b"abcdefgH"));
        assert!(!bytes_eq(b"abcdefgh", b""));
        assert!(bytes_eq(b"", b""));
    }

    #[test]
    fn pin_is_one_attempt_with_a_ttl_and_a_lockout() {
        let mut p = PinState::default();
        assert_eq!(p.check(0, "1234"), Err(PinError::None));
        let pin = p.issue(1000, 10_000 + 123).unwrap();
        assert_eq!(pin, 123, "modulo ten thousand");
        assert_eq!(p.check(2000, "0123"), Ok(()));
        assert_eq!(p.check(2000, "0123"), Err(PinError::None), "used up");
        // Wrong: lockout, no new PIN until it ends.
        let pin = p.issue(3000, 4242).unwrap();
        assert_eq!(p.check(3001, "4243"), Err(PinError::Wrong));
        assert_eq!(p.issue(3002, 1), Err(PIN_LOCKOUT_MS - 1));
        assert_eq!(p.check(3003, "4242"), Err(PinError::None), "a wrong try also used it up");
        assert_eq!(p.issue(3001 + PIN_LOCKOUT_MS, 5), Ok(5));
        let _ = pin;
        // Expired.
        p.issue(10_000, 7777).unwrap();
        assert_eq!(p.check(10_000 + PIN_TTL_MS, "7777"), Ok(()));
        p.issue(20_000, 7777).unwrap();
        assert_eq!(p.check(20_001 + PIN_TTL_MS, "7777"), Err(PinError::Expired));
        // The rule is four characters that parse to the number.
        p.issue(0, 123).unwrap();
        assert_eq!(p.check(1, "123"), Err(PinError::Wrong));
        p.issue(PIN_LOCKOUT_MS + 1, 123).unwrap();
        assert_eq!(p.check(PIN_LOCKOUT_MS + 2, "+123"), Ok(()), "another spelling of the same secret");
        p.issue(PIN_LOCKOUT_MS + 3, 123).unwrap();
        assert_eq!(p.check(PIN_LOCKOUT_MS + 4, "01234"), Err(PinError::Wrong));
        p.issue(2 * PIN_LOCKOUT_MS + 5, 123).unwrap();
        assert_eq!(p.check(2 * PIN_LOCKOUT_MS + 6, "012a"), Err(PinError::Wrong));
    }

    #[test]
    fn header_rules() {
        let pin_ok = |digits: &str| if digits == "1234" { Ok(()) } else { Err(PinError::Wrong) };
        assert_eq!(with_pin(None, pin_ok).map_err(|r| r.status), Err("403 Forbidden"));
        assert_eq!(with_pin(Some("PIN 1234"), pin_ok), Ok(()));
        assert_eq!(with_pin(Some("PIN  1234 "), pin_ok), Ok(()), "trimmed");
        assert_eq!(with_pin(Some("PIN 0000"), pin_ok).map_err(|r| r.text), Err("wrong PIN"));
        assert_eq!(with_pin(Some("PIN 0000"), |_| Err(PinError::Expired)).map_err(|r| r.text), Err("PIN expired: reload the update page"));
        assert_eq!(with_pin(Some("PIN 0000"), |_| Err(PinError::None)).map_err(|r| r.text), Err("no PIN issued: open the update page first"));
        assert_eq!(with_pin(Some("Basic abc"), pin_ok).map_err(|r| r.text), Err("bad credentials"));
        assert_eq!(with_pin(Some("pin 1234"), pin_ok).map_err(|r| r.text), Err("bad credentials"), "the scheme is case-sensitive");
    }

    #[test]
    fn token_header_against_the_setting() {
        // No token set: nothing matches, and the text says so.
        settings::update(|s| s.token = Secret(Text::EMPTY));
        assert!(!token_enabled());
        assert!(!token_ok(b""), "an empty token never matches");
        assert_eq!(token(Some("Bearer ")).map_err(|r| r.text), Err("no token set: build one in or set one from the update page"));
        assert_eq!(token(None).map_err(|r| r.text), Err("no token: send Authorization: Bearer <API_TOKEN>"));
        assert_eq!(token(Some("PIN 1234")).map_err(|r| r.text), Err("no token: send Authorization: Bearer <API_TOKEN>"));
        settings::update(|s| s.token = Secret(Text::literal("secret-token-1")));
        assert!(token_enabled());
        assert_eq!(token(Some("Bearer secret-token-1")), Ok(()));
        assert_eq!(token(Some("Bearer secret-token-1 ")), Ok(()), "trailing space trimmed");
        assert_eq!(token(Some("Bearer secret-token-")).map_err(|r| r.text), Err("bad token"));
        assert_eq!(token(Some("Bearer secret-token-12")).map_err(|r| r.text), Err("bad token"));
        assert_eq!(token(Some("Bearer Secret-token-1")).map_err(|r| r.text), Err("bad token"));
        assert_eq!(token(Some("bearer secret-token-1")).map_err(|r| r.text), Err("no token: send Authorization: Bearer <API_TOKEN>"));
        assert_eq!(with_pin(Some("Bearer secret-token-1"), |_| Err(PinError::None)), Ok(()), "the token does for a PIN route");
        assert_eq!(with_pin(Some("Bearer wrong-token-xx"), |_| Err(PinError::None)).map_err(|r| r.text), Err("bad token"));
        settings::update(|s| s.token = Secret(Text::EMPTY));
    }
}

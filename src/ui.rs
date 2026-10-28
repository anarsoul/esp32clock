//! The mode state machine behind the two buttons.
//!
//! Pure logic: events, the settings and the time go in, a `View` of what
//! to draw comes out. The settings themselves live in `settings`; the UI
//! edits whatever copy it is handed. Nothing here touches hardware.

use embassy_time::{Duration, Instant};

use crate::buttons::{Button, Event, Kind};
use crate::clock::LocalTime;
use crate::settings::{BRIGHTNESS_STEPS, PALETTE, Settings};

/// Leave alarm-set mode after this long without a press.
const SET_TIMEOUT: Duration = Duration::from_secs(15);

/// Stop ringing after this long if nobody presses anything. Public so
/// `heal::reset` can bound how long it holds a reset off for a ring.
pub const RING_TIMEOUT: Duration = Duration::from_secs(300);

/// Each octet of the IP stays this long.
pub const IP_STEP: Duration = Duration::from_millis(2000);

/// Show the four octets this many times.
const IP_ROUNDS: u64 = 2;

/// The update page's PIN stays on the panel this long, unless a press
/// ends it. The PIN itself is good for longer; see `ota::PIN_TTL_MS`.
pub const PIN_SHOW: Duration = Duration::from_secs(60);

/// After a power cut the clock comes back and gets a valid time some
/// minutes late. On the first tick with a time, an alarm that came round
/// within this many minutes before is rung anyway, so an outage across
/// the alarm minute does not swallow it.
pub const MISSED_ALARM_GRACE: u16 = 10;

const MINUTES_PER_DAY: u16 = 24 * 60;

/// Minutes since the alarm time last came round, and the weekday that
/// was. Before the alarm time that day is the day before.
fn minutes_since_alarm(settings: &Settings, t: LocalTime) -> (u16, u8) {
    let minute = t.hour as u16 * 60 + t.minute as u16;
    let alarm = settings.alarm_hour as u16 * 60 + settings.alarm_minute as u16;
    if minute >= alarm {
        (minute - alarm, t.weekday)
    } else {
        (minute + MINUTES_PER_DAY - alarm, (t.weekday + 6) % 7)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Clock,
    SetHour,
    SetMinute,
    Ringing,
    /// Showing the IP address, octet by octet.
    ShowIp,
    /// Showing the update page's PIN.
    ShowPin,
}

/// Which part of an alarm-set view is being edited.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Hour,
    Minute,
}

/// What the display should show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    /// Waiting for the first sync.
    Sweep,
    /// The time, colon on or off.
    Time { time: LocalTime, colon: bool },
    /// The alarm time; `blank` hides the edited field for the blink.
    AlarmSet { hour: u8, minute: u8, field: Field, blank: bool },
    /// Alarm ringing: the time, all on, flashing.
    Ringing { time: LocalTime, flash: bool },
    /// Octet `index` (0 to 3) of the IP address.
    Ip { index: usize },
    /// Four digits for the update page.
    Pin { pin: u16 },
}

pub struct Ui {
    pub mode: Mode,
    /// When the current mode began, or the last press in alarm-set: what
    /// every timeout and the IP octet index count from.
    since: Instant,
    /// The PIN being shown.
    pin: u16,
    /// Unix time the alarm last rang or was used up by the skip flag.
    /// An alarm that came round after that is still due; one that came
    /// round before it has been dealt with, so it fires once and the
    /// boot-time grace does not fire it again. Carried across a software
    /// reset by the display task.
    pub last_alarm: Option<i64>,
    /// A valid time has been seen since boot; the missed-alarm check is
    /// only made before that.
    had_time: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

impl Ui {
    pub fn new() -> Self {
        Ui { mode: Mode::Clock, since: Instant::now(), pin: 0, last_alarm: None, had_time: false }
    }

    /// Switch mode, timing it from `now`.
    fn enter(&mut self, mode: Mode, now: Instant) {
        self.mode = mode;
        self.since = now;
    }

    /// Feed a button event, editing `settings` as the mode dictates.
    pub fn handle(&mut self, settings: &mut Settings, ev: Event, now: Instant) {
        // A press in alarm-set restarts its timeout.
        self.since = now;
        match self.mode {
            Mode::Ringing | Mode::ShowIp | Mode::ShowPin => self.enter(Mode::Clock, now),
            Mode::Clock => match (ev.button, ev.kind) {
                (Button::Top, Kind::Short) => {
                    settings.color = (settings.color + 1) % PALETTE.len();
                }
                (Button::Bottom, Kind::Short) => {
                    settings.brightness = (settings.brightness + 1) % BRIGHTNESS_STEPS.len();
                }
                (Button::Top, Kind::Long) => self.enter(Mode::SetHour, now),
                (Button::Bottom, Kind::Long) => {
                    settings.alarm_armed = !settings.alarm_armed;
                }
                (Button::Both, Kind::Long) => self.enter(Mode::ShowIp, now),
                (Button::Both, Kind::Short) => {}
            },
            Mode::SetHour => match (ev.button, ev.kind) {
                (Button::Top, Kind::Short) => {
                    settings.alarm_hour = (settings.alarm_hour + 1) % 24;
                }
                (Button::Bottom, Kind::Short) => self.enter(Mode::SetMinute, now),
                _ => {}
            },
            Mode::SetMinute => match (ev.button, ev.kind) {
                (Button::Top, Kind::Short) => {
                    settings.alarm_minute = (settings.alarm_minute + 5) % 60;
                }
                (Button::Bottom, Kind::Short) => self.enter(Mode::Clock, now),
                _ => {}
            },
        }
    }

    /// Show a PIN from the update page. Ringing and alarm-set keep the
    /// panel; the PIN replaces the clock, the IP or an earlier PIN.
    pub fn show_pin(&mut self, pin: u16, now: Instant) {
        if matches!(self.mode, Mode::Clock | Mode::ShowIp | Mode::ShowPin) {
            self.pin = pin;
            self.enter(Mode::ShowPin, now);
        }
    }

    /// Advance timeouts and the alarm trigger. `time` is `None` before the
    /// first sync. `settings` is only written to consume `skip_next`.
    pub fn tick(&mut self, settings: &mut Settings, time: Option<LocalTime>, now: Instant) {
        let timeout = match self.mode {
            Mode::SetHour | Mode::SetMinute => Some(SET_TIMEOUT),
            Mode::Ringing => Some(RING_TIMEOUT),
            Mode::ShowIp => Some(IP_STEP * (4 * IP_ROUNDS as u32)),
            Mode::ShowPin => Some(PIN_SHOW),
            Mode::Clock => None,
        };
        match timeout {
            Some(limit) => {
                if now - self.since >= limit {
                    self.enter(Mode::Clock, now);
                }
            }
            None => {
                if let Some(t) = time {
                    let (behind, day) = minutes_since_alarm(settings, t);
                    // When the alarm last came round, as Unix time.
                    let came_round = t.unix - behind as i64 * 60 - t.second as i64;
                    let armed = settings.alarm_armed && settings.alarm_on(day);
                    let handled = self.last_alarm.is_some_and(|at| at >= came_round);
                    // On the alarm minute, once. On the first valid time
                    // after boot, also shortly after it: a power cut may
                    // have eaten the alarm.
                    let due = armed && !handled && (behind == 0 || (!self.had_time && behind <= MISSED_ALARM_GRACE));
                    if due {
                        self.last_alarm = Some(t.unix);
                        if settings.skip_next {
                            // A skipped alarm is used up, not postponed.
                            settings.skip_next = false;
                        } else {
                            self.enter(Mode::Ringing, now);
                        }
                    }
                }
            }
        }
        self.had_time |= time.is_some();
    }

    pub fn ringing(&self) -> bool {
        self.mode == Mode::Ringing
    }

    /// What to draw right now. `phase` is the uptime in milliseconds, used
    /// for blinking.
    pub fn view(&self, settings: &Settings, time: Option<LocalTime>, phase_ms: u64) -> View {
        let blink = (phase_ms / 250) % 2 == 1;
        match self.mode {
            Mode::ShowIp => {
                let elapsed = phase_ms.saturating_sub(self.since.as_millis());
                View::Ip { index: (elapsed / IP_STEP.as_millis()) as usize % 4 }
            }
            Mode::ShowPin => View::Pin { pin: self.pin },
            Mode::SetHour | Mode::SetMinute => View::AlarmSet {
                hour: settings.alarm_hour,
                minute: settings.alarm_minute,
                field: if self.mode == Mode::SetHour { Field::Hour } else { Field::Minute },
                blank: blink,
            },
            Mode::Ringing => match time {
                Some(t) => View::Ringing { time: t, flash: blink },
                None => View::Sweep,
            },
            Mode::Clock => match time {
                Some(t) => View::Time { time: t, colon: t.second % 2 == 0 },
                None => View::Sweep,
            },
        }
    }
}

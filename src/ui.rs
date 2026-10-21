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

/// Stop ringing after this long if nobody presses anything.
const RING_TIMEOUT: Duration = Duration::from_secs(120);

/// Each octet of the IP stays this long.
pub const IP_STEP: Duration = Duration::from_millis(2000);

/// Show the four octets this many times.
const IP_ROUNDS: u64 = 2;

/// After a power cut the clock comes back and gets a valid time some
/// minutes late. On the first tick with a time, an alarm that came round
/// within this many minutes before is rung anyway, so an outage across
/// the alarm minute does not swallow it.
pub const MISSED_ALARM_GRACE: u16 = 10;

const MINUTES_PER_DAY: u16 = 24 * 60;

/// Minutes since the alarm time last came round, if the alarm is armed
/// on the day it came round on. Before the alarm time that day is the
/// day before.
fn alarm_age(settings: &Settings, t: LocalTime) -> Option<u16> {
    let minute = t.hour as u16 * 60 + t.minute as u16;
    let alarm = settings.alarm_hour as u16 * 60 + settings.alarm_minute as u16;
    let (age, day) = if minute >= alarm {
        (minute - alarm, t.weekday)
    } else {
        (minute + MINUTES_PER_DAY - alarm, (t.weekday + 6) % 7)
    };
    (settings.alarm_armed && settings.alarm_on(day)).then_some(age)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Clock,
    SetHour,
    SetMinute,
    Ringing,
    /// Showing the IP address, octet by octet.
    ShowIp,
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
}

pub struct Ui {
    pub mode: Mode,
    /// Last press, for the set-mode timeout.
    last_press: Instant,
    /// When ringing started.
    ring_started: Instant,
    /// When the IP display started.
    ip_started: Instant,
    /// Minute of day the alarm last fired on, so it fires once.
    fired_minute: Option<u16>,
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
        let now = Instant::now();
        Ui {
            mode: Mode::Clock,
            last_press: now,
            ring_started: now,
            ip_started: now,
            fired_minute: None,
            had_time: false,
        }
    }

    /// Feed a button event, editing `settings` as the mode dictates.
    pub fn handle(&mut self, settings: &mut Settings, ev: Event, now: Instant) {
        self.last_press = now;
        match self.mode {
            Mode::Ringing | Mode::ShowIp => self.mode = Mode::Clock,
            Mode::Clock => match (ev.button, ev.kind) {
                (Button::Top, Kind::Short) => {
                    settings.color = (settings.color + 1) % PALETTE.len();
                }
                (Button::Bottom, Kind::Short) => {
                    settings.brightness = (settings.brightness + 1) % BRIGHTNESS_STEPS.len();
                }
                (Button::Top, Kind::Long) => self.mode = Mode::SetHour,
                (Button::Bottom, Kind::Long) => {
                    settings.alarm_armed = !settings.alarm_armed;
                }
                (Button::Both, Kind::Long) => {
                    self.ip_started = now;
                    self.mode = Mode::ShowIp;
                }
                (Button::Both, Kind::Short) => {}
            },
            Mode::SetHour => match (ev.button, ev.kind) {
                (Button::Top, Kind::Short) => {
                    settings.alarm_hour = (settings.alarm_hour + 1) % 24;
                }
                (Button::Bottom, Kind::Short) => self.mode = Mode::SetMinute,
                _ => {}
            },
            Mode::SetMinute => match (ev.button, ev.kind) {
                (Button::Top, Kind::Short) => {
                    settings.alarm_minute = (settings.alarm_minute + 5) % 60;
                }
                (Button::Bottom, Kind::Short) => self.mode = Mode::Clock,
                _ => {}
            },
        }
    }

    /// Advance timeouts and the alarm trigger. `time` is `None` before the
    /// first sync.
    pub fn tick(&mut self, settings: &Settings, time: Option<LocalTime>, now: Instant) {
        match self.mode {
            Mode::SetHour | Mode::SetMinute => {
                if now - self.last_press >= SET_TIMEOUT {
                    self.mode = Mode::Clock;
                }
            }
            Mode::Ringing => {
                if now - self.ring_started >= RING_TIMEOUT {
                    self.mode = Mode::Clock;
                }
            }
            Mode::ShowIp => {
                if now - self.ip_started >= IP_STEP * (4 * IP_ROUNDS as u32) {
                    self.mode = Mode::Clock;
                }
            }
            Mode::Clock => {
                if let Some(t) = time {
                    let minute = t.hour as u16 * 60 + t.minute as u16;
                    let alarm = settings.alarm_hour as u16 * 60 + settings.alarm_minute as u16;
                    // On the alarm minute, once. On the first valid time
                    // after boot, also shortly after it: a power cut may
                    // have eaten the alarm.
                    let due = match alarm_age(settings, t) {
                        Some(0) => self.fired_minute != Some(minute),
                        Some(age) => !self.had_time && age <= MISSED_ALARM_GRACE,
                        None => false,
                    };
                    if due {
                        self.fired_minute = Some(minute);
                        self.ring_started = now;
                        self.mode = Mode::Ringing;
                    }
                    if minute != alarm {
                        self.fired_minute = None;
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
                let elapsed = phase_ms.saturating_sub(self.ip_started.as_millis());
                View::Ip { index: (elapsed / IP_STEP.as_millis()) as usize % 4 }
            }
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

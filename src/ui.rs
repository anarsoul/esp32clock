//! Settings and the mode state machine behind the two buttons.
//!
//! Pure logic: events and the time go in, a `View` of what to draw comes
//! out. Nothing here touches hardware.

use embassy_time::{Duration, Instant};

use crate::buttons::{Button, Event, Kind};
use crate::clock::LocalTime;
use crate::panel::Rgb;

/// Colours the top button cycles through.
pub const PALETTE: [Rgb; 8] = [
    Rgb::RED,
    Rgb::ORANGE,
    Rgb::YELLOW,
    Rgb::GREEN,
    Rgb::CYAN,
    Rgb::BLUE,
    Rgb::MAGENTA,
    Rgb::WHITE,
];

/// Brightness steps the bottom button cycles through, in percent.
pub const BRIGHTNESS_STEPS: [u8; 5] = [10, 25, 50, 75, 100];

/// Leave alarm-set mode after this long without a press.
const SET_TIMEOUT: Duration = Duration::from_secs(15);

/// Stop ringing after this long if nobody presses anything.
const RING_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    pub color: usize,
    pub brightness: usize,
    pub alarm_hour: u8,
    pub alarm_minute: u8,
    pub alarm_armed: bool,
}

impl Settings {
    pub fn color(&self) -> Rgb {
        PALETTE[self.color % PALETTE.len()]
    }

    pub fn brightness_pct(&self) -> u8 {
        BRIGHTNESS_STEPS[self.brightness % BRIGHTNESS_STEPS.len()]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Clock,
    SetHour,
    SetMinute,
    Ringing,
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
}

pub struct Ui {
    pub settings: Settings,
    pub mode: Mode,
    /// Last press, for the set-mode timeout.
    last_press: Instant,
    /// When ringing started.
    ring_started: Instant,
    /// Minute of day the alarm last fired on, so it fires once.
    fired_minute: Option<u16>,
}

impl Ui {
    pub fn new(settings: Settings) -> Self {
        let now = Instant::now();
        Ui { settings, mode: Mode::Clock, last_press: now, ring_started: now, fired_minute: None }
    }

    /// Feed a button event.
    pub fn handle(&mut self, ev: Event, now: Instant) {
        self.last_press = now;
        match self.mode {
            Mode::Ringing => self.mode = Mode::Clock,
            Mode::Clock => match (ev.button, ev.kind) {
                (Button::Top, Kind::Short) => {
                    self.settings.color = (self.settings.color + 1) % PALETTE.len();
                }
                (Button::Bottom, Kind::Short) => {
                    self.settings.brightness = (self.settings.brightness + 1) % BRIGHTNESS_STEPS.len();
                }
                (Button::Top, Kind::Long) => self.mode = Mode::SetHour,
                (Button::Bottom, Kind::Long) => {
                    self.settings.alarm_armed = !self.settings.alarm_armed;
                }
            },
            Mode::SetHour => match (ev.button, ev.kind) {
                (Button::Top, Kind::Short) => {
                    self.settings.alarm_hour = (self.settings.alarm_hour + 1) % 24;
                }
                (Button::Bottom, Kind::Short) => self.mode = Mode::SetMinute,
                _ => {}
            },
            Mode::SetMinute => match (ev.button, ev.kind) {
                (Button::Top, Kind::Short) => {
                    self.settings.alarm_minute = (self.settings.alarm_minute + 5) % 60;
                }
                (Button::Bottom, Kind::Short) => self.mode = Mode::Clock,
                _ => {}
            },
        }
    }

    /// Advance timeouts and the alarm trigger. `time` is `None` before the
    /// first sync.
    pub fn tick(&mut self, time: Option<LocalTime>, now: Instant) {
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
            Mode::Clock => {
                if let Some(t) = time {
                    let minute = t.hour as u16 * 60 + t.minute as u16;
                    let alarm = self.settings.alarm_hour as u16 * 60 + self.settings.alarm_minute as u16;
                    if self.settings.alarm_armed && minute == alarm && self.fired_minute != Some(minute) {
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
    }

    pub fn ringing(&self) -> bool {
        self.mode == Mode::Ringing
    }

    /// What to draw right now. `phase` is the uptime in milliseconds, used
    /// for blinking.
    pub fn view(&self, time: Option<LocalTime>, phase_ms: u64) -> View {
        let blink = (phase_ms / 250) % 2 == 1;
        match self.mode {
            Mode::SetHour | Mode::SetMinute => View::AlarmSet {
                hour: self.settings.alarm_hour,
                minute: self.settings.alarm_minute,
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

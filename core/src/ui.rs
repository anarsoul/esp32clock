// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

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
/// ends it. The PIN itself is good for longer; see `auth::PIN_TTL_MS`.
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
                // The mode switch; the display task acts on it first.
                (_, Kind::Hold) => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::local_time;
    use crate::settings::{DAYS_ALL, DAYS_WEEKDAYS};

    /// Monday 1970-01-05 00:00 UTC.
    const MONDAY: i64 = 4 * 86_400;

    fn at(day: i64, hour: i64, minute: i64, second: i64) -> LocalTime {
        local_time(MONDAY + day * 86_400 + hour * 3600 + minute * 60 + second, 0)
    }

    fn press(button: Button, kind: Kind) -> Event {
        Event { button, kind }
    }

    fn ms(n: u64) -> Instant {
        Instant::from_millis(n)
    }

    fn armed() -> Settings {
        Settings { alarm_hour: 7, alarm_minute: 0, alarm_armed: true, alarm_days: DAYS_ALL, ..Settings::DEFAULT }
    }

    #[test]
    fn short_presses_cycle_colour_and_brightness() {
        let mut ui = Ui::new();
        let mut s = Settings::DEFAULT;
        let (c0, b0) = (s.color, s.brightness);
        ui.handle(&mut s, press(Button::Top, Kind::Short), ms(0));
        assert_eq!(s.color, (c0 + 1) % PALETTE.len());
        ui.handle(&mut s, press(Button::Bottom, Kind::Short), ms(0));
        assert_eq!(s.brightness, (b0 + 1) % BRIGHTNESS_STEPS.len());
        for _ in 0..PALETTE.len() - 1 {
            ui.handle(&mut s, press(Button::Top, Kind::Short), ms(0));
        }
        assert_eq!(s.color, c0, "wraps");
        assert_eq!(ui.mode, Mode::Clock);
        ui.handle(&mut s, press(Button::Bottom, Kind::Long), ms(0));
        assert!(s.alarm_armed);
        ui.handle(&mut s, press(Button::Both, Kind::Hold), ms(0));
        assert_eq!(ui.mode, Mode::Clock, "the hold is for the display task");
    }

    #[test]
    fn alarm_set_flow_and_timeout() {
        let mut ui = Ui::new();
        let mut s = Settings { alarm_hour: 23, alarm_minute: 55, ..Settings::DEFAULT };
        ui.handle(&mut s, press(Button::Top, Kind::Long), ms(1000));
        assert_eq!(ui.mode, Mode::SetHour);
        ui.handle(&mut s, press(Button::Top, Kind::Short), ms(1000));
        assert_eq!(s.alarm_hour, 0, "hours wrap at 24");
        assert!(matches!(ui.view(&s, None, 1000), View::AlarmSet { field: Field::Hour, .. }));
        ui.handle(&mut s, press(Button::Bottom, Kind::Short), ms(2000));
        assert_eq!(ui.mode, Mode::SetMinute);
        ui.handle(&mut s, press(Button::Top, Kind::Short), ms(2000));
        assert_eq!(s.alarm_minute, 0, "minutes step by five and wrap");
        assert!(matches!(ui.view(&s, None, 2000), View::AlarmSet { field: Field::Minute, .. }));
        // A press restarts the timeout; without one it lapses.
        ui.tick(&mut s, None, ms(2000 + SET_TIMEOUT.as_millis() - 1));
        assert_eq!(ui.mode, Mode::SetMinute);
        ui.tick(&mut s, None, ms(2000 + SET_TIMEOUT.as_millis()));
        assert_eq!(ui.mode, Mode::Clock);
        // Bottom short leaves set mode.
        ui.handle(&mut s, press(Button::Top, Kind::Long), ms(5000));
        ui.handle(&mut s, press(Button::Bottom, Kind::Short), ms(5000));
        ui.handle(&mut s, press(Button::Bottom, Kind::Short), ms(5000));
        assert_eq!(ui.mode, Mode::Clock);
    }

    #[test]
    fn ip_display_steps_through_the_octets_twice() {
        let mut ui = Ui::new();
        let mut s = Settings::DEFAULT;
        ui.handle(&mut s, press(Button::Both, Kind::Long), ms(10_000));
        assert_eq!(ui.mode, Mode::ShowIp);
        let step = IP_STEP.as_millis();
        for i in 0..8u64 {
            assert_eq!(ui.view(&s, None, 10_000 + i * step + 1), View::Ip { index: (i % 4) as usize });
        }
        ui.tick(&mut s, None, ms(10_000 + 8 * step - 1));
        assert_eq!(ui.mode, Mode::ShowIp);
        ui.tick(&mut s, None, ms(10_000 + 8 * step));
        assert_eq!(ui.mode, Mode::Clock);
        // Any press ends it early.
        ui.handle(&mut s, press(Button::Both, Kind::Long), ms(20_000));
        ui.handle(&mut s, press(Button::Top, Kind::Short), ms(20_001));
        assert_eq!(ui.mode, Mode::Clock);
        assert_eq!(s.color, Settings::DEFAULT.color, "the ending press does nothing else");
    }

    #[test]
    fn pin_shows_only_over_the_clock_and_times_out() {
        let mut ui = Ui::new();
        let mut s = Settings::DEFAULT;
        ui.show_pin(1234, ms(0));
        assert_eq!(ui.view(&s, None, 0), View::Pin { pin: 1234 });
        ui.show_pin(42, ms(1));
        assert_eq!(ui.view(&s, None, 1), View::Pin { pin: 42 }, "a newer PIN replaces it");
        ui.tick(&mut s, None, ms(1 + PIN_SHOW.as_millis()));
        assert_eq!(ui.mode, Mode::Clock);
        ui.handle(&mut s, press(Button::Top, Kind::Long), ms(2));
        ui.show_pin(7, ms(3));
        assert_eq!(ui.mode, Mode::SetHour, "alarm-set keeps the panel");
    }

    #[test]
    fn alarm_rings_once_on_its_minute() {
        let mut ui = Ui::new();
        let mut s = armed();
        ui.tick(&mut s, Some(at(0, 6, 59, 59)), ms(0));
        assert_eq!(ui.mode, Mode::Clock);
        ui.tick(&mut s, Some(at(0, 7, 0, 0)), ms(1000));
        assert_eq!(ui.mode, Mode::Ringing);
        assert!(ui.ringing());
        assert_eq!(ui.last_alarm, Some(at(0, 7, 0, 0).unix));
        assert!(matches!(ui.view(&s, Some(at(0, 7, 0, 0)), 1000), View::Ringing { .. }));
        ui.tick(&mut s, Some(at(0, 7, 0, 30)), ms(31_000));
        assert_eq!(ui.mode, Mode::Ringing);
        // A press ends it and the same minute does not ring again.
        ui.handle(&mut s, press(Button::Top, Kind::Short), ms(32_000));
        assert_eq!(ui.mode, Mode::Clock);
        assert_eq!(s.color, Settings::DEFAULT.color);
        ui.tick(&mut s, Some(at(0, 7, 0, 40)), ms(41_000));
        assert_eq!(ui.mode, Mode::Clock);
        // Tomorrow it rings again.
        ui.tick(&mut s, Some(at(1, 7, 0, 0)), ms(100_000));
        assert_eq!(ui.mode, Mode::Ringing);
        // And stops by itself after the ring timeout.
        ui.tick(&mut s, Some(at(1, 7, 4, 59)), ms(100_000 + RING_TIMEOUT.as_millis() - 1));
        assert_eq!(ui.mode, Mode::Ringing);
        ui.tick(&mut s, Some(at(1, 7, 5, 0)), ms(100_000 + RING_TIMEOUT.as_millis()));
        assert_eq!(ui.mode, Mode::Clock);
    }

    #[test]
    fn alarm_days_and_disarmed() {
        let mut ui = Ui::new();
        let mut s = Settings { alarm_days: DAYS_WEEKDAYS, ..armed() };
        ui.tick(&mut s, Some(at(5, 7, 0, 0)), ms(0)); // Saturday
        assert_eq!(ui.mode, Mode::Clock);
        ui.tick(&mut s, Some(at(7, 7, 0, 0)), ms(1000)); // next Monday
        assert_eq!(ui.mode, Mode::Ringing);
        let mut quiet = Ui::new();
        let mut off = Settings { alarm_armed: false, ..armed() };
        quiet.tick(&mut off, Some(at(0, 7, 0, 0)), ms(0));
        assert_eq!(quiet.mode, Mode::Clock);
        assert_eq!(quiet.last_alarm, None);
    }

    #[test]
    fn missed_alarm_grace_on_the_first_time_after_boot() {
        // Boot at 07:05: the alarm five minutes ago rings.
        let mut ui = Ui::new();
        let mut s = armed();
        ui.tick(&mut s, Some(at(0, 7, 5, 0)), ms(0));
        assert_eq!(ui.mode, Mode::Ringing);
        // Boot at 07:11: too late.
        let mut late = Ui::new();
        late.tick(&mut s, Some(at(0, 7, 11, 0)), ms(0));
        assert_eq!(late.mode, Mode::Clock);
        assert_eq!(late.last_alarm, None);
        // Already had a time: 07:05 is just a minute like any other.
        let mut running = Ui::new();
        running.tick(&mut s, Some(at(0, 6, 0, 0)), ms(0));
        running.tick(&mut s, Some(at(0, 7, 5, 0)), ms(1000));
        assert_eq!(running.mode, Mode::Clock);
        // Carried: the alarm was handled at 07:00:30, so the grace stays quiet.
        let mut carried = Ui::new();
        carried.last_alarm = Some(at(0, 7, 0, 30).unix);
        carried.tick(&mut s, Some(at(0, 7, 5, 0)), ms(0));
        assert_eq!(carried.mode, Mode::Clock);
        // Carried from yesterday: today's alarm is new.
        let mut old = Ui::new();
        old.last_alarm = Some(at(-1, 7, 0, 30).unix);
        old.tick(&mut s, Some(at(0, 7, 5, 0)), ms(0));
        assert_eq!(old.mode, Mode::Ringing);
        // Across midnight: the alarm at 23:58 came round "yesterday" for a 00:03 boot.
        let mut mid = Ui::new();
        let mut night = Settings { alarm_hour: 23, alarm_minute: 58, alarm_days: DAYS_WEEKDAYS, ..armed() };
        mid.tick(&mut night, Some(at(5, 0, 3, 0)), ms(0)); // Saturday 00:03, alarm was Friday 23:58
        assert_eq!(mid.mode, Mode::Ringing);
        let mut sun = Ui::new();
        sun.tick(&mut night, Some(at(6, 0, 3, 0)), ms(0)); // Sunday 00:03, alarm would have been Saturday
        assert_eq!(sun.mode, Mode::Clock);
    }

    #[test]
    fn skip_next_uses_up_the_alarm() {
        let mut ui = Ui::new();
        let mut s = Settings { skip_next: true, ..armed() };
        ui.tick(&mut s, Some(at(0, 6, 0, 0)), ms(0));
        assert!(s.skip_next);
        ui.tick(&mut s, Some(at(0, 7, 0, 0)), ms(1000));
        assert_eq!(ui.mode, Mode::Clock);
        assert!(!s.skip_next, "the flag is consumed");
        assert_eq!(ui.last_alarm, Some(at(0, 7, 0, 0).unix), "and the alarm counts as handled");
        ui.tick(&mut s, Some(at(0, 7, 0, 30)), ms(31_000));
        assert_eq!(ui.mode, Mode::Clock);
        ui.tick(&mut s, Some(at(1, 7, 0, 0)), ms(100_000));
        assert_eq!(ui.mode, Mode::Ringing, "only the next alarm was skipped");
        // A carried skip flag is used up by the boot-time grace too.
        let mut boot = Ui::new();
        let mut t = Settings { skip_next: true, ..armed() };
        boot.tick(&mut t, Some(at(0, 7, 5, 0)), ms(0));
        assert_eq!(boot.mode, Mode::Clock);
        assert!(!t.skip_next);
    }

    #[test]
    fn views_without_a_time() {
        let ui = Ui::new();
        let s = Settings::DEFAULT;
        assert_eq!(ui.view(&s, None, 0), View::Sweep);
        let t = at(0, 12, 34, 56);
        assert_eq!(ui.view(&s, Some(t), 0), View::Time { time: t, colon: true });
        let odd = at(0, 12, 34, 57);
        assert_eq!(ui.view(&s, Some(odd), 0), View::Time { time: odd, colon: false });
    }
}

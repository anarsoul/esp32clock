//! Turns the clock and the UI state into panel images, and rings the alarm.

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};

use crate::buttons;
use crate::buzzer::{self, Beep};
use crate::config::{DEFAULT_ALARM, DEFAULT_BRIGHTNESS, DEFAULT_COLOR, MAX_CURRENT_STEP, TWELVE_HOUR};
use crate::panel::{Panel, Rgb, ScanImage, current_step_for};
use crate::ui::{Field, Settings, Ui, View};
use crate::{clock, scan};

/// Step time of the waiting animation and of the blink.
const BLINK_STEP: Duration = Duration::from_millis(250);

/// How often the alarm pattern repeats while ringing.
const RING_STEP: Duration = Duration::from_millis(1500);

/// Component scale for the trailing dash, out of 256.
const TRAIL_SCALE: u16 = 64;

/// The alarm sound.
const ALARM_BEEP: Beep = Beep { freq_hz: 2500, on_ms: 100, off_ms: 100, count: 4 };

fn dimmed(c: Rgb) -> Rgb {
    let f = |v: u8| ((v as u16 * TRAIL_SCALE) >> 8) as u8;
    Rgb::new(f(c.r), f(c.g), f(c.b))
}

/// Hours as shown: 24 hour, or 12 hour with the AM mark.
fn draw_hhmm(panel: &mut Panel, hour: u8, minute: u8, show_hour: bool, show_minute: bool) {
    let mut h = hour;
    if TWELVE_HOUR {
        h %= 12;
        if h == 0 {
            h = 12;
        }
        panel.set_am(hour < 12);
    }
    if show_hour {
        panel.set_char(0, if h >= 10 { (b'0' + h / 10) as char } else { ' ' });
        panel.set_char(1, (b'0' + h % 10) as char);
    }
    if show_minute {
        panel.set_char(2, (b'0' + minute / 10) as char);
        panel.set_char(3, (b'0' + minute % 10) as char);
    }
}

/// One dash sweeps across the four digits with a dimmer dash one step
/// behind it, so a glance says "working on it".
fn draw_sweep(panel: &mut Panel, color: Rgb, phase_ms: u64) {
    let step = (phase_ms / BLINK_STEP.as_millis()) as usize;
    let head = step % 4;
    let tail = (step + 3) % 4;
    panel.set_digit_color(tail, dimmed(color));
    panel.set_char(head, '-');
    panel.set_char(tail, '-');
}

fn draw(panel: &mut Panel, view: View, settings: &Settings, phase_ms: u64) {
    panel.clear();
    panel.set_color(settings.color());
    panel.set_current(current_step_for(settings.brightness_pct()).min(MAX_CURRENT_STEP));
    panel.set_degree(settings.alarm_armed);
    match view {
        View::Sweep => draw_sweep(panel, settings.color(), phase_ms),
        View::Time { time, colon } => {
            draw_hhmm(panel, time.hour, time.minute, true, true);
            panel.set_colon(colon);
        }
        View::AlarmSet { hour, minute, field, blank } => {
            draw_hhmm(
                panel,
                hour,
                minute,
                !(blank && field == Field::Hour),
                !(blank && field == Field::Minute),
            );
            panel.set_colon(true);
        }
        View::Ringing { time, flash } => {
            if flash {
                draw_hhmm(panel, time.hour, time.minute, true, true);
                panel.set_colon(true);
            }
        }
    }
}

/// Run the UI: wait for a tick or a button, update, draw, ring.
#[embassy_executor::task]
pub async fn display_task() -> ! {
    let mut panel = Panel::new();
    let mut ui = Ui::new(Settings {
        color: DEFAULT_COLOR,
        brightness: DEFAULT_BRIGHTNESS,
        alarm_hour: DEFAULT_ALARM.0,
        alarm_minute: DEFAULT_ALARM.1,
        alarm_armed: false,
    });
    let mut last: Option<ScanImage> = None;
    let mut last_ring: Option<Instant> = None;

    loop {
        let now = Instant::now();
        let time = clock::now();
        ui.tick(time, now);

        if ui.ringing() && last_ring.is_none_or(|t| now - t >= RING_STEP) {
            buzzer::beep(ALARM_BEEP);
            last_ring = Some(now);
        }
        if !ui.ringing() {
            last_ring = None;
        }

        let view = ui.view(time, now.as_millis());
        draw(&mut panel, view, &ui.settings, now.as_millis());
        let img = panel.render();
        if last != Some(img) {
            scan::publish(&img).await;
            last = Some(img);
        }

        // Next redraw: the next second in clock mode, the blink step while
        // anything blinks or sweeps, plus a little so the boundary has passed.
        let steady = matches!(view, View::Time { .. });
        let wait = if steady { clock::until_next_second() } else { BLINK_STEP / 4 };
        match select(Timer::after(wait + Duration::from_millis(2)), buttons::next_event()).await {
            Either::First(()) => {}
            Either::Second(ev) => ui.handle(ev, Instant::now()),
        }
    }
}

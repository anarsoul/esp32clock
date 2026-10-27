//! Turns the clock and the UI state into panel images, and rings the alarm.

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_futures::select::{Either4, select4};
use embassy_net::Stack;
use embassy_time::{Duration, Instant, Timer};
use log::info;

use crate::buttons;
use crate::buzzer;
use crate::config::MAX_CURRENT_STEP;
use crate::panel::{Panel, Rgb, ScanImage, current_step_for};
use crate::settings::{self, Settings};
use crate::ui::{Field, IP_STEP, Ui, View};
use crate::{carry, clock, ota, scan};

/// Step time of the waiting animation and of the blink.
const BLINK_STEP: Duration = Duration::from_millis(250);

/// Component scale for the trailing dash, out of 256.
const TRAIL_SCALE: u16 = 64;

fn dimmed(c: Rgb) -> Rgb {
    let f = |v: u8| ((v as u16 * TRAIL_SCALE) >> 8) as u8;
    Rgb::new(f(c.r), f(c.g), f(c.b))
}

/// Hours as shown: 24 hour, or 12 hour with the AM mark.
fn draw_hhmm(panel: &mut Panel, twelve_hour: bool, hour: u8, minute: u8, show_hour: bool, show_minute: bool) {
    let mut h = hour;
    if twelve_hour {
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

/// One octet of the IP: its number (1 to 4) in the first digit, its
/// value right-aligned in the other three. `no1P` without an address.
fn draw_ip(panel: &mut Panel, ip: Option<[u8; 4]>, index: usize) {
    let Some(ip) = ip else {
        for (i, c) in "no1P".chars().enumerate() {
            panel.set_char(i, c);
        }
        return;
    };
    let v = ip[index];
    panel.set_char(0, (b'1' + index as u8) as char);
    panel.set_char(1, if v >= 100 { (b'0' + v / 100) as char } else { ' ' });
    panel.set_char(2, if v >= 10 { (b'0' + v / 10 % 10) as char } else { ' ' });
    panel.set_char(3, (b'0' + v % 10) as char);
}

/// The update page's PIN: four digits, colon off.
fn draw_pin(panel: &mut Panel, pin: u16) {
    let mut v = pin;
    for i in (0..4).rev() {
        panel.set_char(i, (b'0' + (v % 10) as u8) as char);
        v /= 10;
    }
}

fn draw(panel: &mut Panel, view: View, settings: &Settings, ip: Option<[u8; 4]>, phase_ms: u64) {
    panel.clear();
    panel.set_color(settings.color());
    panel.set_current(current_step_for(settings.brightness_pct()).min(MAX_CURRENT_STEP));
    panel.set_degree(settings.alarm_armed);
    let twelve = settings.twelve_hour;
    match view {
        View::Sweep => draw_sweep(panel, settings.color(), phase_ms),
        View::Time { time, colon } => {
            draw_hhmm(panel, twelve, time.hour, time.minute, true, true);
            panel.set_colon(colon);
        }
        View::AlarmSet { hour, minute, field, blank } => {
            draw_hhmm(
                panel,
                twelve,
                hour,
                minute,
                !(blank && field == Field::Hour),
                !(blank && field == Field::Minute),
            );
            panel.set_colon(true);
        }
        View::Ringing { time, flash } => {
            if flash {
                draw_hhmm(panel, twelve, time.hour, time.minute, true, true);
                panel.set_colon(true);
            }
        }
        View::Ip { index } => draw_ip(panel, ip, index),
        View::Pin { pin } => draw_pin(panel, pin),
    }
}

/// How long after an alarm was handled a software reset still counts it
/// as handled: the missed-alarm grace plus the ringing timeout, with room.
const HANDLED_WINDOW_SECS: i64 = 20 * 60;

/// Whether the alarm is ringing right now. Written only by the display
/// task, on the ringing edges; read by `heal::reset`, which holds a
/// deliberate reset off while it is set.
static RINGING: AtomicBool = AtomicBool::new(false);

/// Is the alarm ringing?
pub fn ringing() -> bool {
    RINGING.load(Ordering::Relaxed)
}

/// Remember, across a software reset, that today's alarm has been dealt
/// with. Needs the time; without it there is no grace to suppress either.
fn record_handled(s: &Settings) {
    if let Some(unix) = clock::unix_secs() {
        let minute = s.alarm_hour as u16 * 60 + s.alarm_minute as u16;
        carry::update(|c| c.handled = Some((unix, minute)));
    }
}

/// Run the UI: wait for a tick, a button or a settings change from
/// elsewhere, update, draw, ring. `stack` is only asked for the IP.
#[embassy_executor::task]
pub async fn display_task(stack: Stack<'static>) -> ! {
    let mut panel = Panel::new();
    let mut ui = Ui::new();
    let mut changes = settings::receiver();
    let mut last: Option<ScanImage> = None;
    let mut was_ringing = false;

    // Volatile alarm state from before a software reset. The skip flag
    // goes straight back into the settings; the handled alarm is applied
    // once the time is known, since it is only meaningful near it.
    let carried = carry::get();
    let mut carried_skip = carried.skip_next;
    if carried.skip_next {
        settings::update(|s| s.skip_next = true);
        let _ = changes.try_changed();
        info!("display: skip-next alarm carried over the reset");
    }
    let mut seed_handled = carried.handled;

    loop {
        let now = Instant::now();
        let time = clock::now();
        if let (Some((at, minute)), Some(unix)) = (seed_handled, clock::unix_secs()) {
            seed_handled = None;
            if unix - at <= HANDLED_WINDOW_SECS {
                ui.suppress_alarm(minute);
                info!("display: alarm {:02}:{:02} handled {} s before the reset, not ringing it again", minute / 60, minute % 60, unix - at);
            }
        }
        // The tick only writes the settings to use up a skipped alarm.
        if settings::update(|s| ui.tick(s, time, now)) {
            let _ = changes.try_changed();
            record_handled(&settings::get());
        }
        let current = settings::get();
        if current.skip_next != carried_skip {
            carried_skip = current.skip_next;
            carry::update(|c| c.skip_next = carried_skip);
        }

        // The buzzer loops the melody on its own; tell it only on the edges.
        // The flag goes up before the buzzer starts and comes down after
        // the alarm is recorded as handled, so a heal held off by the
        // ring never resets between the end of the ring and the record.
        if ui.ringing() != was_ringing {
            was_ringing = ui.ringing();
            if was_ringing {
                RINGING.store(true, Ordering::Relaxed);
                buzzer::ring(current.melody);
            } else {
                buzzer::stop();
                // Rung and ended, by a press or the timeout: do not ring it
                // again should a reset follow within the grace. A reset
                // while still ringing leaves this unset on purpose, so the
                // grace brings the alarm back after the reboot.
                record_handled(&current);
                RINGING.store(false, Ordering::Relaxed);
            }
        }

        let view = ui.view(&current, time, now.as_millis());
        let ip = stack.config_v4().map(|c| c.address.address().octets());
        draw(&mut panel, view, &current, ip, now.as_millis());
        let img = panel.render();
        if last != Some(img) {
            scan::publish(&img).await;
            last = Some(img);
        }

        // Next redraw: the next second in clock mode, the blink step while
        // anything blinks or sweeps, plus a little so the boundary has passed.
        let wait = match view {
            View::Time { .. } => clock::until_next_second(),
            View::Ip { .. } => IP_STEP / 4,
            _ => BLINK_STEP / 4,
        };
        match select4(
            Timer::after(wait + Duration::from_millis(2)),
            buttons::next_event(),
            changes.changed(),
            ota::pin_to_show(),
        )
        .await
        {
            Either4::First(()) => {}
            Either4::Second(ev) => {
                let now = Instant::now();
                settings::update(|s| ui.handle(s, ev, now));
                // Our own change; the redraw happens on this loop anyway.
                let _ = changes.try_changed();
            }
            Either4::Third(_) => {}
            Either4::Fourth(pin) => ui.show_pin(pin, Instant::now()),
        }
    }
}

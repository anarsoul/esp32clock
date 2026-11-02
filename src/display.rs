// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! Turns the clock and the UI state into panel images, and rings the alarm.

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_futures::select::{Either4, select4};
use embassy_net::Stack;
use embassy_time::{Duration, Instant, Timer};
use log::info;

use crate::buttons::{self, Kind};
use crate::buzzer;
use crate::net::{self, Mode};
use crate::panel::{Panel, ScanImage};
use crate::settings::{self, Settings};
use crate::ui::{Field, IP_STEP, Ui, View};
use crate::{auth, carry, clock, heal, scan, sensors, settings as settings_mod};

/// Step time of the waiting animation and of the blink.
const BLINK_STEP: Duration = Duration::from_millis(250);

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

/// One dash sweeps across the four digits, so a glance says "working
/// on it".
fn draw_sweep(panel: &mut Panel, phase_ms: u64) {
    let step = (phase_ms / BLINK_STEP.as_millis()) as usize;
    panel.set_char(step % 4, '-');
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

/// The page PIN (update or token page): four digits, colon off.
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
    panel.set_current(settings.current_step());
    panel.set_degree(settings.alarm_armed);
    let twelve = settings.twelve_hour;
    match view {
        // No time to show: the sweep while the station waits for one,
        // `AP` while the clock is the access point and will never have one.
        View::Sweep if net::mode() == Mode::AccessPoint => {
            panel.set_char(0, 'A');
            panel.set_char(1, 'P');
        }
        View::Sweep => draw_sweep(panel, phase_ms),
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
        View::Temp { tenths, fahrenheit } => {
            for (i, c) in sensors::face(tenths, if fahrenheit { 'F' } else { 'C' }).into_iter().enumerate() {
                panel.set_char(i, c);
            }
            panel.set_degree(true);
        }
    }
}

/// Whether the alarm is ringing right now. Written only by the display
/// task, on the ringing edges; read by `heal::reset`, which holds a
/// deliberate reset off while it is set.
static RINGING: AtomicBool = AtomicBool::new(false);

/// Is the alarm ringing?
pub fn ringing() -> bool {
    RINGING.load(Ordering::Relaxed)
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

    // Volatile alarm state from before a software reset: the skip flag
    // goes straight back into the settings, the handled alarm into the UI.
    let carried = carry::get();
    let mut carried_skip = carried.skip_next;
    if carried.skip_next {
        settings::update(|s| s.skip_next = true);
        info!("display: skip-next alarm carried over the reset");
    }
    let mut carried_alarm = carried.last_alarm_unix;
    ui.last_alarm = carried.last_alarm_unix;
    if let Some(at) = carried.last_alarm_unix {
        info!("display: alarm last handled at unix {at}, carried over the reset");
    }

    loop {
        let now = Instant::now();
        let time = clock::now();
        // Keep the carried time fresh for a panic reset, which cannot
        // write it on the way down; this loop runs at least every second.
        clock::carry_time();
        // The tick only writes the settings to use up a skipped alarm.
        settings::update(|s| ui.tick(s, time, now));
        let current = settings::get();
        if current.skip_next != carried_skip {
            carried_skip = current.skip_next;
            carry::update(|c| c.skip_next = carried_skip);
        }
        // The handled alarm is carried once the ring has ended, by a press
        // or the timeout, so a reset that follows within the grace does
        // not ring it again; a reset mid-ring finds nothing carried and
        // the grace brings the alarm back.
        let last_alarm = if ui.ringing() { None } else { ui.last_alarm };
        if last_alarm != carried_alarm {
            carried_alarm = last_alarm;
            carry::update(|c| c.last_alarm_unix = last_alarm);
        }

        // The buzzer loops the melody on its own; tell it only on the edges.
        // The flag goes up before the buzzer starts and comes down after
        // the handled alarm is carried, above, so a heal held off by the
        // ring never resets between the end of the ring and the record.
        if ui.ringing() != was_ringing {
            was_ringing = ui.ringing();
            if was_ringing {
                RINGING.store(true, Ordering::Relaxed);
                buzzer::ring(current.melody);
            } else {
                buzzer::stop();
                RINGING.store(false, Ordering::Relaxed);
            }
        }

        let view = ui.view(&current, time, now.as_millis(), sensors::room().0);
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
            View::Time { .. } | View::Temp { .. } => clock::until_next_second(),
            View::Ip { .. } => IP_STEP / 4,
            _ => BLINK_STEP / 4,
        };
        match select4(
            Timer::after(wait + Duration::from_millis(2)),
            buttons::next_event(),
            changes.changed(),
            auth::pin_to_show(),
        )
        .await
        {
            Either4::First(()) => {}
            Either4::Second(ev) => {
                if ev.kind == Kind::Hold {
                    // The mode switch: into the access point from the
                    // station, back to the station from the access point
                    // when there is a network to join.
                    match (net::mode(), settings_mod::get().wifi.is_none()) {
                        (Mode::Station, _) => heal::switch_mode(true).await,
                        (Mode::AccessPoint, false) => heal::switch_mode(false).await,
                        (Mode::AccessPoint, true) => info!("display: hold ignored, no network to join yet"),
                    }
                    continue;
                }
                let now = Instant::now();
                settings::update(|s| ui.handle(s, ev, now));
            }
            Either4::Third(_) => {}
            Either4::Fourth(Some(pin)) => ui.show_pin(pin, Instant::now()),
            Either4::Fourth(None) => ui.hide_pin(Instant::now()),
        }
    }
}

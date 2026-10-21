//! Turns the clock into panel images.

use embassy_time::{Duration, Instant, Timer};

use crate::config::{BRIGHTNESS_PCT, MAX_CURRENT_STEP, PANEL_COLOR, TWELVE_HOUR};
use crate::panel::{Panel, Rgb, ScanImage, current_step_for};
use crate::{clock, scan};

/// Step time of the waiting animation.
const SWEEP_STEP: Duration = Duration::from_millis(250);

/// Component scale for the trailing dash, out of 256.
const TRAIL_SCALE: u16 = 64;

fn dimmed(c: Rgb) -> Rgb {
    let f = |v: u8| ((v as u16 * TRAIL_SCALE) >> 8) as u8;
    Rgb::new(f(c.r), f(c.g), f(c.b))
}

/// Until the clock is set: one dash sweeps across the four digits with a
/// dimmer dash one step behind it, so a glance says "working on it".
fn draw_sweep(panel: &mut Panel) {
    let step = (Instant::now().as_millis() / SWEEP_STEP.as_millis()) as usize;
    let head = step % 4;
    let tail = (step + 3) % 4;
    panel.set_color(PANEL_COLOR);
    panel.set_digit_color(tail, dimmed(PANEL_COLOR));
    panel.set_char(head, '-');
    panel.set_char(tail, '-');
}

fn draw(panel: &mut Panel) {
    panel.clear();
    // The sweep tints one digit; undo that before anything else draws.
    panel.set_color(PANEL_COLOR);
    match clock::now() {
        None => draw_sweep(panel),
        Some(t) => {
            let mut hour = t.hour;
            if TWELVE_HOUR {
                hour %= 12;
                if hour == 0 {
                    hour = 12;
                }
                panel.set_am(t.hour < 12);
            }
            panel.set_time(hour, t.minute);
            // One second on, one second off.
            panel.set_colon(t.second % 2 == 0);
        }
    }
}

/// Redraw on every second boundary and publish only when the image changed.
#[embassy_executor::task]
pub async fn display_task() -> ! {
    let mut panel = Panel::new();
    panel.set_color(PANEL_COLOR);
    panel.set_current(current_step_for(BRIGHTNESS_PCT).min(MAX_CURRENT_STEP));
    let mut last: Option<ScanImage> = None;

    loop {
        draw(&mut panel);
        let img = panel.render();
        if last != Some(img) {
            scan::publish(&img).await;
            last = Some(img);
        }
        // A little past the boundary so the second has really changed.
        let wait = if clock::is_synced() { clock::until_next_second() } else { SWEEP_STEP / 4 };
        Timer::after(wait + Duration::from_millis(2)).await;
    }
}

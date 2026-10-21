//! Turns the clock into panel images.

use embassy_time::Timer;

use crate::config::{BRIGHTNESS_PCT, MAX_CURRENT_STEP, PANEL_COLOR, TWELVE_HOUR};
use crate::panel::{Panel, ScanImage, current_step_for};
use crate::{clock, scan};

fn draw(panel: &mut Panel) {
    panel.clear();
    match clock::now() {
        None => {
            for block in 0..4 {
                panel.set_char(block, '-');
            }
        }
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
        Timer::after(clock::until_next_second() + embassy_time::Duration::from_millis(2)).await;
    }
}

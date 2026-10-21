//! Turns the clock into panel images.

use embassy_time::Timer;

use crate::config::TWELVE_HOUR;
use crate::panel::{Color, Panel, ScanImage};
use crate::{clock, scan};

/// Panel colour and current until there is a way to change them.
const COLOR: Color = Color::RED;
const CURRENT_STEP: u8 = 5;

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
    panel.set_color(COLOR);
    panel.set_current(CURRENT_STEP);
    let mut last: Option<ScanImage> = None;

    loop {
        draw(&mut panel);
        let img = panel.render();
        if last != Some(img) {
            scan::publish(img);
            last = Some(img);
        }
        // A little past the boundary so the second has really changed.
        Timer::after(clock::until_next_second() + embassy_time::Duration::from_millis(2)).await;
    }
}

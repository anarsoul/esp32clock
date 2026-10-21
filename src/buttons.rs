//! The two front buttons. Both switch to ground; the board has no pull-ups,
//! so the pins use the internal ones and a press reads low.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::gpio::Input;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    /// S1, the upper button.
    Top,
    /// S2, the lower button.
    Bottom,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Released before the long-press threshold.
    Short,
    /// Held past the threshold. Emitted while still held.
    Long,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Event {
    pub button: Button,
    pub kind: Kind,
}

const POLL: Duration = Duration::from_millis(10);
const DEBOUNCE: Duration = Duration::from_millis(30);
const LONG_PRESS: Duration = Duration::from_millis(800);

static EVENTS: Channel<CriticalSectionRawMutex, Event, 8> = Channel::new();

/// Wait for the next button event.
pub async fn next_event() -> Event {
    EVENTS.receive().await
}

/// Debounce and classify one button.
struct Tracker {
    button: Button,
    stable: bool,
    last_raw: bool,
    changed_at: Instant,
    pressed_at: Option<Instant>,
    long_sent: bool,
}

impl Tracker {
    fn new(button: Button) -> Self {
        Tracker {
            button,
            stable: false,
            last_raw: false,
            changed_at: Instant::now(),
            pressed_at: None,
            long_sent: false,
        }
    }

    fn poll(&mut self, raw_pressed: bool, now: Instant) {
        if raw_pressed != self.last_raw {
            self.last_raw = raw_pressed;
            self.changed_at = now;
        }
        if raw_pressed != self.stable && now - self.changed_at >= DEBOUNCE {
            self.stable = raw_pressed;
            if raw_pressed {
                self.pressed_at = Some(now);
                self.long_sent = false;
            } else if let Some(t) = self.pressed_at.take()
                && !self.long_sent
                && now - t < LONG_PRESS
            {
                self.emit(Kind::Short);
            }
        }
        if let Some(t) = self.pressed_at
            && !self.long_sent
            && now - t >= LONG_PRESS
        {
            self.long_sent = true;
            self.emit(Kind::Long);
        }
    }

    fn emit(&self, kind: Kind) {
        // Dropping an event when eight are queued is fine; nobody can
        // press that fast.
        let _ = EVENTS.try_send(Event { button: self.button, kind });
    }
}

/// Poll both buttons forever.
#[embassy_executor::task]
pub async fn button_task(top: Input<'static>, bottom: Input<'static>) -> ! {
    let mut t = Tracker::new(Button::Top);
    let mut b = Tracker::new(Button::Bottom);
    loop {
        let now = Instant::now();
        t.poll(top.is_low(), now);
        b.poll(bottom.is_low(), now);
        Timer::after(POLL).await;
    }
}

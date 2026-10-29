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
    /// Both held together. Only ever `Long`.
    Both,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Released before the long-press threshold.
    Short,
    /// Held past the threshold. Emitted while still held.
    Long,
    /// Both buttons held for `HOLD_PRESS`, long after their `Long`.
    /// Only ever for `Both`, once per hold.
    Hold,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Event {
    pub button: Button,
    pub kind: Kind,
}

const POLL: Duration = Duration::from_millis(10);
const DEBOUNCE: Duration = Duration::from_millis(30);
const LONG_PRESS: Duration = Duration::from_millis(800);

/// Both buttons down this long is the mode switch (station or access
/// point), far past anything a short or long press needs.
const HOLD_PRESS: Duration = Duration::from_secs(10);

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
    /// `held` is whether the button is down right now. A button that is
    /// down when the firmware starts is the tail of a press that began
    /// before the reset (the 10 s hold resets the chip while both are
    /// still held): its release must not count as a press, so it starts
    /// as stable-down with nothing pending.
    fn new(button: Button, held: bool) -> Self {
        Tracker {
            button,
            stable: held,
            last_raw: held,
            changed_at: Instant::now(),
            pressed_at: None,
            long_sent: true,
        }
    }

    /// Held past the long-press threshold, with no event sent for it yet.
    fn long_due(&self, now: Instant) -> bool {
        self.pressed_at.is_some_and(|t| now - t >= LONG_PRESS) && !self.long_sent
    }

    /// Treat the current press as reported; no event on release either.
    fn consume(&mut self) {
        if self.pressed_at.is_some() {
            self.long_sent = true;
        }
    }

    /// `other_held` holds back this button's long press while the other
    /// is down, so that a two-button hold can be reported as its own
    /// event instead.
    fn poll(&mut self, raw_pressed: bool, now: Instant, other_held: bool) {
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
        if self.long_due(now) && !other_held {
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
    let mut t = Tracker::new(Button::Top, top.is_low());
    let mut b = Tracker::new(Button::Bottom, bottom.is_low());
    // When both went down together, and whether the hold has been sent.
    let mut both_since: Option<Instant> = None;
    let mut hold_sent = false;
    loop {
        let now = Instant::now();
        t.poll(top.is_low(), now, b.pressed_at.is_some());
        b.poll(bottom.is_low(), now, t.pressed_at.is_some());
        if t.long_due(now) && b.long_due(now) {
            t.consume();
            b.consume();
            let _ = EVENTS.try_send(Event { button: Button::Both, kind: Kind::Long });
        }
        if t.pressed_at.is_some() && b.pressed_at.is_some() {
            let since = *both_since.get_or_insert(now);
            if !hold_sent && now - since >= HOLD_PRESS {
                hold_sent = true;
                let _ = EVENTS.try_send(Event { button: Button::Both, kind: Kind::Hold });
            }
        } else {
            both_since = None;
            hold_sent = false;
        }
        Timer::after(POLL).await;
    }
}

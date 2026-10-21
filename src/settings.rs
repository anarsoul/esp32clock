//! The user's settings: one shared value with a change signal, and the
//! record they are stored as in flash.
//!
//! Anything may change the settings through [`update`]; the display and
//! the persist task each hold a [`Receiver`] and wake when something did.
//! Pure logic apart from the `embassy-sync` watch, so host-testable.

use core::cell::{Cell, RefCell};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Receiver, Watch};

use crate::config::{DEFAULT_ALARM, DEFAULT_BRIGHTNESS, DEFAULT_COLOR};
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    pub color: usize,
    pub brightness: usize,
    pub alarm_hour: u8,
    pub alarm_minute: u8,
    pub alarm_armed: bool,
}

/// Bytes in a stored record.
pub const RECORD_LEN: usize = 8;

/// Stored record layout version. Bump when the layout changes; an old
/// record is then ignored and the defaults apply.
const RECORD_VERSION: u8 = 1;

impl Settings {
    /// The compile-time defaults.
    pub const DEFAULT: Settings = Settings {
        color: DEFAULT_COLOR,
        brightness: DEFAULT_BRIGHTNESS,
        alarm_hour: DEFAULT_ALARM.0,
        alarm_minute: DEFAULT_ALARM.1,
        alarm_armed: false,
    };

    pub fn color(&self) -> Rgb {
        PALETTE[self.color % PALETTE.len()]
    }

    pub fn brightness_pct(&self) -> u8 {
        BRIGHTNESS_STEPS[self.brightness % BRIGHTNESS_STEPS.len()]
    }

    /// Every field within its range.
    pub fn is_valid(&self) -> bool {
        self.color < PALETTE.len()
            && self.brightness < BRIGHTNESS_STEPS.len()
            && self.alarm_hour < 24
            && self.alarm_minute < 60
    }

    /// The flash record: version, colour, brightness, alarm hour, alarm
    /// minute, armed, two spare bytes.
    pub fn to_record(&self) -> [u8; RECORD_LEN] {
        [
            RECORD_VERSION,
            self.color as u8,
            self.brightness as u8,
            self.alarm_hour,
            self.alarm_minute,
            self.alarm_armed as u8,
            0,
            0,
        ]
    }

    /// Parse a flash record. `None` for another version or a field out of
    /// range, so a bad record falls back to the defaults.
    pub fn from_record(r: &[u8; RECORD_LEN]) -> Option<Settings> {
        if r[0] != RECORD_VERSION {
            return None;
        }
        let s = Settings {
            color: r[1] as usize,
            brightness: r[2] as usize,
            alarm_hour: r[3],
            alarm_minute: r[4],
            alarm_armed: r[5] != 0,
        };
        s.is_valid().then_some(s)
    }
}

/// Receivers: the display task and the persist task.
const RECEIVERS: usize = 2;

static WATCH: Watch<CriticalSectionRawMutex, Settings, RECEIVERS> = Watch::new();

pub type SettingsReceiver = Receiver<'static, CriticalSectionRawMutex, Settings, RECEIVERS>;

/// Publish the boot-time value. Call once before anything reads.
pub fn init(settings: Settings) {
    WATCH.sender().send(settings);
}

/// The current settings.
pub fn get() -> Settings {
    WATCH.try_get().unwrap_or(Settings::DEFAULT)
}

/// Apply `f` to the settings and notify receivers if anything changed.
/// Returns whether it did.
pub fn update(f: impl FnOnce(&mut Settings)) -> bool {
    // The watch wants a `Fn`; the closure runs exactly once under its lock.
    let f = RefCell::new(Some(f));
    let changed = Cell::new(false);
    WATCH.sender().send_if_modified(|slot| {
        let before = slot.unwrap_or(Settings::DEFAULT);
        let mut after = before;
        if let Some(f) = f.borrow_mut().take() {
            f(&mut after);
        }
        let modified = after != before || slot.is_none();
        if modified {
            *slot = Some(after);
        }
        changed.set(modified);
        modified
    });
    changed.get()
}

/// A receiver that wakes on every change. Panics past `RECEIVERS`.
pub fn receiver() -> SettingsReceiver {
    WATCH.receiver().expect("too many settings receivers")
}

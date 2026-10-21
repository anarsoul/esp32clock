//! Buzzer on GPIO5 through the LEDC PWM peripheral.
//!
//! The buzzer is a magnetic type driven by an S8550 PNP whose emitter sits
//! at +5 V, with a 10 k resistor into the base from the GPIO. A 3.3 V high
//! still leaves 1.7 V across base-emitter and turns it on, so the only "off"
//! is high impedance. The pin runs open-drain: low energises the buzzer,
//! released leaves it silent.
//!
//! A magnetic buzzer has no oscillator of its own, so it is driven with a
//! square wave at the tone frequency. LEDC generates it; a duty of 100 %
//! holds the line high (released) between beeps.
//!
//! Nothing beeps on its own. Beeps and melodies are reserved for the alarm.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel as MsgChannel;
use embassy_time::{Duration, Timer};
use esp_hal::gpio::{DriveMode, Level, Output, OutputConfig};
use esp_hal::ledc::channel::{self, ChannelIFace};
use esp_hal::ledc::timer::{self, TimerIFace};
use esp_hal::ledc::{Ledc, LowSpeed};
use esp_hal::peripherals::GPIO5;
use esp_hal::time::Rate;
use log::{info, warn};

/// One beep request: `count` tones of `on_ms`, separated by `off_ms`.
#[derive(Clone, Copy, Debug)]
pub struct Beep {
    pub freq_hz: u32,
    pub on_ms: u16,
    pub off_ms: u16,
    pub count: u8,
}

/// Starting point for the tone. Magnetic buzzers resonate around 2 to 3 kHz.
pub const DEFAULT_HZ: u32 = 2500;

impl Beep {
    pub const fn short() -> Self {
        Beep { freq_hz: DEFAULT_HZ, on_ms: 60, off_ms: 0, count: 1 }
    }

    pub const fn double() -> Self {
        Beep { freq_hz: DEFAULT_HZ, on_ms: 80, off_ms: 80, count: 2 }
    }
}

/// One note of a melody. `freq_hz == 0` is a rest.
#[derive(Clone, Copy, Debug)]
pub struct Note {
    pub freq_hz: u32,
    pub ms: u16,
}

/// Silence between consecutive notes so repeated pitches stay separate.
const ARTICULATION_MS: u64 = 20;

// Equal-tempered pitches, A4 = 440 Hz.
pub const C5: u32 = 523;
pub const D5: u32 = 587;
pub const E5: u32 = 659;
pub const F5: u32 = 698;
pub const G5: u32 = 784;
pub const A5: u32 = 880;
pub const B5: u32 = 988;
pub const C6: u32 = 1047;
pub const D6: u32 = 1175;
pub const E6: u32 = 1319;
pub const G6: u32 = 1568;
pub const REST: u32 = 0;

const fn n(freq_hz: u32, ms: u16) -> Note {
    Note { freq_hz, ms }
}

/// Opening of "Ode to Joy", one octave up so the small buzzer carries it.
pub const ODE_TO_JOY: &[Note] = &[
    n(E6, 250), n(E6, 250), n(F5 * 2, 250), n(G6, 250),
    n(G6, 250), n(F5 * 2, 250), n(E6, 250), n(D6, 250),
    n(C6, 250), n(C6, 250), n(D6, 250), n(E6, 250),
    n(E6, 375), n(D6, 125), n(D6, 500),
];

#[derive(Clone, Copy, Debug)]
enum Request {
    Beep(Beep),
    Melody(&'static [Note]),
}

static REQUESTS: MsgChannel<CriticalSectionRawMutex, Request, 4> = MsgChannel::new();

fn request(r: Request) {
    if REQUESTS.try_send(r).is_err() {
        warn!("buzzer: queue full, dropping request");
    }
}

/// Queue a beep. Dropped if four requests are already waiting.
pub fn beep(b: Beep) {
    request(Request::Beep(b));
}

/// Queue a melody. Dropped if four requests are already waiting.
pub fn play(notes: &'static [Note]) {
    request(Request::Melody(notes));
}

const SILENT_PCT: u8 = 100;
const TONE_PCT: u8 = 50;

/// Owns the LEDC peripheral and the buzzer pin.
///
/// The LEDC channel keeps a shared borrow of its timer, so the timer cannot
/// be retuned while the channel exists. Each tone therefore builds a fresh
/// timer and channel at the requested frequency, reborrowing the pin. The
/// pin is wrapped as an open-drain output already held high before LEDC
/// takes it, so the line is never driven low while the channel is wired up.
#[embassy_executor::task]
pub async fn buzzer_task(ledc: Ledc<'static>, pin: GPIO5<'static>) -> ! {
    let mut ledc = ledc;
    let mut pin = pin;
    ledc.set_global_slow_clock(esp_hal::ledc::LSGlobalClkSource::APBClk);

    loop {
        match REQUESTS.receive().await {
            Request::Beep(b) => {
                info!("buzzer: {} x {} ms at {} Hz", b.count, b.on_ms, b.freq_hz);
                for i in 0..b.count {
                    tone(&ledc, &mut pin, b.freq_hz, b.on_ms as u64).await;
                    if i + 1 < b.count {
                        Timer::after(Duration::from_millis(b.off_ms as u64)).await;
                    }
                }
            }
            Request::Melody(notes) => {
                info!("buzzer: melody, {} notes", notes.len());
                for note in notes {
                    if note.freq_hz == 0 {
                        Timer::after(Duration::from_millis(note.ms as u64)).await;
                    } else {
                        tone(&ledc, &mut pin, note.freq_hz, note.ms as u64).await;
                    }
                    Timer::after(Duration::from_millis(ARTICULATION_MS)).await;
                }
            }
        }
    }
}

/// Sound one tone, then release the line.
async fn tone(ledc: &Ledc<'static>, pin: &mut GPIO5<'static>, freq_hz: u32, ms: u64) {
    let mut pwm_timer = ledc.timer::<LowSpeed>(timer::Number::Timer0);
    if let Err(e) = pwm_timer.configure(timer::config::Config {
        duty: timer::config::Duty::Duty10Bit,
        clock_source: timer::LSClockSource::APBClk,
        frequency: Rate::from_hz(freq_hz),
    }) {
        warn!("buzzer: timer config failed at {freq_hz} Hz: {e:?}");
        return;
    }

    let output = Output::new(
        pin.reborrow(),
        Level::High,
        OutputConfig::default().with_drive_mode(DriveMode::OpenDrain),
    );
    let mut channel = ledc.channel(channel::Number::Channel0, output);
    if let Err(e) = channel.configure(channel::config::Config {
        timer: &pwm_timer,
        duty_pct: SILENT_PCT,
        drive_mode: DriveMode::OpenDrain,
    }) {
        warn!("buzzer: channel config failed: {e:?}");
        return;
    }

    let _ = channel.set_duty(TONE_PCT);
    Timer::after(Duration::from_millis(ms)).await;
    let _ = channel.set_duty(SILENT_PCT);
}

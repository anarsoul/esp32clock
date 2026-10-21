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
//! holds the line high (released) between notes.
//!
//! Nothing sounds on its own. The task plays a melody on command, on a
//! loop for the alarm or once for a preview, and stops within a note when
//! told to.

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use esp_hal::gpio::{DriveMode, Level, Output, OutputConfig};
use esp_hal::ledc::channel::{self, ChannelIFace};
use esp_hal::ledc::timer::{self, TimerIFace};
use esp_hal::ledc::{Ledc, LowSpeed};
use esp_hal::peripherals::GPIO5;
use esp_hal::time::Rate;
use log::{info, warn};

use crate::melody::{self, Melody};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Command {
    /// Play this melody, index into `melody::MELODIES`, until stopped.
    Ring(usize),
    /// Play it once.
    Preview(usize),
    Stop,
}

/// The latest command replaces any unread one, which is what we want: a
/// stop after a ring is a stop.
static COMMAND: Signal<CriticalSectionRawMutex, Command> = Signal::new();

/// Ring the alarm with this melody until `stop`.
pub fn ring(melody: usize) {
    COMMAND.signal(Command::Ring(melody));
}

/// Play this melody once.
pub fn preview(melody: usize) {
    COMMAND.signal(Command::Preview(melody));
}

/// Silence, within one note.
pub fn stop() {
    COMMAND.signal(Command::Stop);
}

const SILENT_PCT: u8 = 100;
const TONE_PCT: u8 = 50;

/// Play `m` once. Returns early with the interrupting command if one
/// arrives, checked after every note.
async fn play_once(ledc: &Ledc<'static>, pin: &mut GPIO5<'static>, m: &Melody) -> Option<Command> {
    for note in m.notes {
        let total = note.ms(m.bpm) as u64;
        let sounding = note.sounding_ms(m.bpm) as u64;
        if sounding > 0
            && let Either::Second(c) = select(tone(ledc, pin, note.hz, sounding), COMMAND.wait()).await
        {
            return Some(c);
        }
        if let Either::Second(c) = select(Timer::after(Duration::from_millis(total - sounding)), COMMAND.wait()).await {
            return Some(c);
        }
    }
    None
}

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

    let mut command = COMMAND.wait().await;
    loop {
        command = match command {
            Command::Stop => COMMAND.wait().await,
            Command::Preview(i) => {
                let m = melody::get(i);
                info!("buzzer: preview {}", m.name);
                match play_once(&ledc, &mut pin, m).await {
                    Some(c) => c,
                    None => COMMAND.wait().await,
                }
            }
            Command::Ring(i) => {
                let m = melody::get(i);
                info!("buzzer: ring {}", m.name);
                loop {
                    if let Some(c) = play_once(&ledc, &mut pin, m).await {
                        break c;
                    }
                    let gap = Timer::after(Duration::from_millis(m.repeat_gap_ms as u64));
                    if let Either::Second(c) = select(gap, COMMAND.wait()).await {
                        break c;
                    }
                }
            }
        };
    }
}

/// Holds the LEDC channel for one tone and silences it when dropped, so
/// a tone cut short by a command does not leave the buzzer on. The PWM
/// keeps the last duty after the channel object is gone, which is what
/// released the line between notes before and still does.
struct Sounding<'a>(channel::Channel<'a, LowSpeed>);

impl Drop for Sounding<'_> {
    fn drop(&mut self) {
        let _ = self.0.set_duty(SILENT_PCT);
    }
}

/// Sound one tone, then release the line. Cancel-safe through `Sounding`.
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

    let sounding = Sounding(channel);
    let _ = sounding.0.set_duty(TONE_PCT);
    Timer::after(Duration::from_millis(ms)).await;
    drop(sounding);
}

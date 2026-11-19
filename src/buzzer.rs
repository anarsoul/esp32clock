// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! Buzzer on GPIO5 through the LEDC PWM peripheral, or the same tones
//! on the line output, GPIO20, for a clock with an amplifier.
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
//! A clip, in a build that has any, is not LEDC's: `clip.rs`
//! sends it as a bitstream from I2S0 on the same pin, from a task of
//! its own, and hands the pin back released, with the GPIO matrix's inverter cleared, which the
//! `Output` each tone builds would do as well.
//!
//! The line output (`settings::SoundPin::Line`) is the other pin a
//! sound can go out of: GPIO20, push-pull, into an RC filter and an
//! amplifier's input. It rests driven low, so a tone there is silent at
//! a duty of 0 %, where the buzzer's is 100 %. Which pin is the
//! `output` setting, read at every tone here (`AudioOut::tones`) and at
//! every clip in `clip.rs` (`AudioOut::clips`), which need not be the
//! same pin, so it changes with no reset; `Pins` is the two pins and
//! the one place that knows how each is driven and what its rest is.
//! LEDC stays connected to the pin of the last tone, silent, so the
//! task takes that pin back before a tone goes to the other one.
//!
//! The amplifier behind the line output is shut down between sounds,
//! by its shutdown pin on GPIO10 (high is on): at rest it amplifies
//! what the Wi-Fi puts on its input, heard as faint clicks. The task
//! turns it on before a melody that goes to the line output, gives it
//! `AMP_WAKE`, and turns it off when the melody has ended and the pin
//! is back at its rest; it stays on through the gaps between notes and
//! between an alarm's repeats. GPIO10 is driven only for a sound on the
//! line output, so a clock with the output at `buzzer` never touches it.
//!
//! Nothing sounds on its own. The task plays a melody on command, on a
//! loop for the alarm or once for a preview, and stops within a note when
//! told to. A command naming `melody::RANDOM` is resolved here, from the
//! hardware RNG, so the setting passes through the display and the page
//! handler unchanged and the log says which tune was drawn.

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use esp_hal::gpio::{AnyPin, DriveMode, Level, Output, OutputConfig};
use esp_hal::ledc::channel::{self, ChannelIFace};
use esp_hal::ledc::timer::{self, TimerIFace};
use esp_hal::ledc::{Ledc, LowSpeed};
use esp_hal::rng::Rng;
use esp_hal::time::Rate;
use log::{info, warn};

use crate::clip;
use crate::display;
use crate::melody::{self, Melody};
use crate::settings::{self, SoundPin};
use crate::watchdog;

/// What the amplifier is given between its shutdown pin going high
/// and the sound. The datasheet has no figure for it: a guess until
/// heard, too short if a sound's start is cut.
const AMP_WAKE: Duration = Duration::from_millis(20);

/// Whether the amplifier's shutdown pin is high, the amplifier on.
/// Written by the buzzer task, and by the clip task while the buzzer
/// task waits for it in `clip::play`: by turns, as the pins are used.
static AMP_ON: AtomicBool = AtomicBool::new(false);

/// The two pins a sound can go out of, and the amplifier's shutdown
/// pin.
pub struct Pins {
    /// GPIO5: never pushed high (the head of this file).
    buzzer: AnyPin<'static>,
    /// GPIO20.
    line: AnyPin<'static>,
    /// GPIO10, to the shutdown pin of the amplifier behind `line`:
    /// high is on, low is shut down.
    amp: AnyPin<'static>,
}

impl Pins {
    pub fn new(buzzer: AnyPin<'static>, line: AnyPin<'static>, amp: AnyPin<'static>) -> Self {
        Self { buzzer, line, amp }
    }

    /// A second handle to both pins.
    ///
    /// # Safety
    ///
    /// The two holders must use the pins by turns.
    pub unsafe fn clone_unchecked(&self) -> Self {
        Self {
            buzzer: unsafe { self.buzzer.clone_unchecked() },
            line: unsafe { self.line.clone_unchecked() },
            amp: unsafe { self.amp.clone_unchecked() },
        }
    }

    /// How the pin of `out` is driven.
    pub fn drive_mode(out: SoundPin) -> DriveMode {
        match out {
            SoundPin::Buzzer => DriveMode::OpenDrain,
            SoundPin::Line => DriveMode::PushPull,
        }
    }

    /// The pin of `out` as an output of the GPIO register at its rest,
    /// whatever it was connected to: the buzzer's open-drain and
    /// released, the line's push-pull and low. The inverter of the GPIO
    /// matrix is cleared with it.
    pub fn at_rest(&mut self, out: SoundPin) -> Output<'_> {
        let (pin, level) = match out {
            SoundPin::Buzzer => (&mut self.buzzer, Level::High),
            SoundPin::Line => (&mut self.line, Level::Low),
        };
        Output::new(pin.reborrow(), level, OutputConfig::default().with_drive_mode(Self::drive_mode(out)))
    }

    /// Leave the pin of `out` at its rest. The pad keeps it after the
    /// `Output` is dropped.
    pub fn rest(&mut self, out: SoundPin) {
        let _ = self.at_rest(out);
    }

    /// Drive the amplifier's shutdown pin, push-pull. The pad keeps
    /// the level after the `Output` is dropped.
    fn amp(&mut self, on: bool) {
        let _ = Output::new(self.amp.reborrow(), Level::from(on), OutputConfig::default());
        AMP_ON.store(on, Ordering::Relaxed);
    }

    /// Shut the amplifier down whatever the pin was, for the boot of a
    /// clock whose sounds go to the line output: the pin floats through
    /// a reset.
    pub fn amp_shut(&mut self) {
        self.amp(false);
    }

    /// The amplifier on, for a sound on the line output. `true` if it
    /// was shut down, and so has yet to come up.
    pub fn amp_on(&mut self) -> bool {
        let was_off = !AMP_ON.load(Ordering::Relaxed);
        if was_off {
            self.amp(true);
        }
        was_off
    }

    /// The amplifier shut down, if a sound turned it on. A clock that
    /// never sounds on the line output never has the pin driven here.
    pub fn amp_off(&mut self) {
        if AMP_ON.load(Ordering::Relaxed) {
            self.amp(false);
        }
    }
}

/// The duty a tone is silent at on `out`, the pin at its rest: the
/// buzzer's line high all the period, the line output low.
const fn silent_pct(out: SoundPin) -> u8 {
    match out {
        SoundPin::Buzzer => 100,
        SoundPin::Line => 0,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Command {
    /// Play this melody, index into the melody table or
    /// `melody::RANDOM`, until stopped.
    Ring(usize),
    /// Play it once.
    Preview(usize),
    Stop,
}

/// The latest command replaces any unread one, which is what we want: a
/// stop after a ring is a stop.
static COMMAND: Signal<CriticalSectionRawMutex, Command> = Signal::new();

/// Ring the alarm with this melody, or a random tune, until `stop`.
pub fn ring(melody: usize) {
    COMMAND.signal(Command::Ring(melody));
}

/// Play this melody, or a random tune, once. Ignored while the alarm
/// rings: a preview from a page or the bus never takes the buzzer from
/// it. The flag is up before the display task signals the ring, and the
/// callers share its executor, so none gets in between the two.
pub fn preview(melody: usize) {
    if display::ringing() {
        info!("buzzer: preview ignored, the alarm is ringing");
        return;
    }
    COMMAND.signal(Command::Preview(melody));
}

/// An end to a preview, for the page's Stop button. Ignored while the
/// alarm rings, as `preview` is: a press ends a ring, a page does not.
pub fn stop_preview() {
    if display::ringing() {
        info!("buzzer: stop ignored, the alarm is ringing");
        return;
    }
    COMMAND.signal(Command::Stop);
}

/// Silence, within one note. The display task's, for the end of a ring.
pub fn stop() {
    COMMAND.signal(Command::Stop);
}

/// The melody a command names: `melody::RANDOM` becomes a tune drawn
/// from the hardware RNG, never the one drawn last, and `last` remembers
/// it. Logs the choice as `what`.
fn resolve(index: usize, last: &mut Option<usize>, what: &str) -> Melody {
    // With the clips under suspicion (the clip task hung in one and the
    // watchdog reset the clock) only notes play, so the alarm the grace
    // rings again after that reset does not hang the same way.
    let notes_only = watchdog::clips_suspect();
    let random = Rng::new().random();
    let picked = if notes_only { melody::pick_notes(index, random, *last) } else { melody::pick(index, random, *last) };
    let m = melody::get(picked);
    if notes_only && picked != index && index != melody::RANDOM {
        warn!("buzzer: {} is a clip and the clip task hung in one: notes until a new pack or firmware", melody::name(index));
    }
    if index == melody::RANDOM {
        *last = Some(picked);
        info!("buzzer: {what} {} (random)", m.name);
    } else {
        info!("buzzer: {what} {}", m.name);
    }
    m
}

/// Play `m` once. Returns early with the interrupting command if one
/// arrives, checked after every note, or twice a chunk in the clip. A
/// clip asked to go `round` that is made to (`melody::clip_loops`) does
/// not end by itself: it loops with no gap, as its tune is written to,
/// until a command arrives.
async fn play_once(ledc: &Ledc<'static>, pins: &mut Pins, on: &mut SoundPin, m: &Melody, round: bool) -> Option<Command> {
    if let Some(k) = m.clip {
        // The clip task has the pins until this returns.
        return clip::play(k, round && melody::clip_loops(k), &COMMAND).await;
    }
    for note in m.notes {
        let total = note.ms(m.bpm) as u64;
        let sounding = note.sounding_ms(m.bpm) as u64;
        if sounding > 0
            && let Either::Second(c) = select(tone(ledc, pins, on, note.hz, sounding), COMMAND.wait()).await
        {
            return Some(c);
        }
        if let Either::Second(c) = select(Timer::after(Duration::from_millis(total - sounding)), COMMAND.wait()).await {
            return Some(c);
        }
    }
    None
}

/// The amplifier as `m` needs it, before its first sound: on, and
/// given `AMP_WAKE` if it was shut down, for a melody that goes to the
/// line output; off for one that goes to the buzzer after one that did
/// not. The pin is the `output` setting's now; a tone or a clip that
/// finds it changed under the melody turns the amplifier on itself.
async fn amp_for(pins: &mut Pins, m: &Melody) {
    let output = settings::get().output;
    let out = if m.clip.is_some() { output.clips() } else { output.tones() };
    match out {
        SoundPin::Line => {
            if pins.amp_on() {
                Timer::after(AMP_WAKE).await;
            }
        }
        SoundPin::Buzzer => pins.amp_off(),
    }
}

/// Owns the LEDC peripheral and the two pins, which it lends to the
/// clip task for the length of a clip.
///
/// The LEDC channel keeps a shared borrow of its timer, so the timer cannot
/// be retuned while the channel exists. Each tone therefore builds a fresh
/// timer and channel at the requested frequency, reborrowing the pin. The
/// pin is wrapped as an open-drain output already held high before LEDC
/// takes it, so the line is never driven low while the channel is wired up.
#[embassy_executor::task]
pub async fn buzzer_task(ledc: Ledc<'static>, pins: Pins) -> ! {
    let mut ledc = ledc;
    let mut pins = pins;
    // The pin LEDC was last connected to.
    let mut on = SoundPin::Buzzer;
    ledc.set_global_slow_clock(esp_hal::ledc::LSGlobalClkSource::APBClk);

    let mut last_random = None;
    let mut command = COMMAND.wait().await;
    loop {
        // The amplifier goes off at a stop and at a melody's own end,
        // and stays as it is when another melody follows at once.
        command = match command {
            Command::Stop => {
                pins.amp_off();
                COMMAND.wait().await
            }
            Command::Preview(i) => {
                let m = resolve(i, &mut last_random, "preview");
                amp_for(&mut pins, &m).await;
                match play_once(&ledc, &mut pins, &mut on, &m, false).await {
                    Some(c) => c,
                    None => {
                        pins.amp_off();
                        COMMAND.wait().await
                    }
                }
            }
            Command::Ring(i) => {
                let m = resolve(i, &mut last_random, "ring");
                amp_for(&mut pins, &m).await;
                loop {
                    if let Some(c) = play_once(&ledc, &mut pins, &mut on, &m, true).await {
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
/// released the line between notes before and still does. The second
/// field is the silent duty of the pin the tone is on.
struct Sounding<'a>(channel::Channel<'a, LowSpeed>, u8);

impl Drop for Sounding<'_> {
    fn drop(&mut self) {
        let _ = self.0.set_duty(self.1);
    }
}

/// Sound one tone at the duty `melody::duty_pct` gives its pitch, then
/// leave the line at its rest. Cancel-safe through `Sounding`. The tone
/// goes to the pin the `output` setting names now; `on` is the pin of
/// the tone before, which LEDC is still connected to and is taken off
/// when this one goes elsewhere, or both would sound.
async fn tone(ledc: &Ledc<'static>, pins: &mut Pins, on: &mut SoundPin, freq_hz: u32, ms: u64) {
    let out = settings::get().output.tones();
    if *on != out {
        pins.rest(*on);
        *on = out;
    }
    // On already, but for an output changed in the middle of a melody.
    if out == SoundPin::Line {
        pins.amp_on();
    }
    let silent = silent_pct(out);
    let mut pwm_timer = ledc.timer::<LowSpeed>(timer::Number::Timer0);
    if let Err(e) = pwm_timer.configure(timer::config::Config {
        duty: timer::config::Duty::Duty10Bit,
        clock_source: timer::LSClockSource::APBClk,
        frequency: Rate::from_hz(freq_hz),
    }) {
        warn!("buzzer: timer config failed at {freq_hz} Hz: {e:?}");
        return;
    }

    let output = pins.at_rest(out);
    let mut channel = ledc.channel(channel::Number::Channel0, output);
    if let Err(e) = channel.configure(channel::config::Config {
        timer: &pwm_timer,
        duty_pct: silent,
        drive_mode: Pins::drive_mode(out),
    }) {
        warn!("buzzer: channel config failed: {e:?}");
        return;
    }

    let sounding = Sounding(channel, silent);
    let _ = sounding.0.set_duty(melody::duty_pct(freq_hz));
    Timer::after(Duration::from_millis(ms)).await;
    drop(sounding);
}

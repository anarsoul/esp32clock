// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! Sampled sound on the buzzer line: I2S0's transmitter as a 1-bit
//! output, fed by DMA from a ring the clip task keeps filled.
//!
//! A PCM clip or a tracker module goes out through the transmitter's
//! PDM mode and its PCM-to-PDM converter (`clip::CONVERTER`, the
//! default build): the ring holds the clip's samples, 16 bits, one
//! channel, and the chip makes `CLIP_PDM_BITS` of each: 256 by
//! default, 4.096 Mbit/s at 16 kHz. At 128 it was heard against the
//! software sigma-delta at half that rate and kept (2026-11-13). Two things about it are not esp-hal's
//! defaults. The driver (1.2.2) clears `tx_pcm_bypass` in PDM mode,
//! which puts the A-law compander before the converter and white noise
//! on the line; `play` sets the bit again. And in PDM mode the master
//! clock's divider is whole, so the sample rate is 160 MHz over
//! integers: `clip::converter_clock` picks the bit-clock divider that
//! lands nearest and the ring is paced by what comes out (16 026 Hz
//! for 16 kHz). A ratio of one, 64 bits a sample, is noise as well,
//! bypass or no.
//!
//! A beeper clip is bits already, and for it, as for every clip in a
//! build with a software modulator (`CLIP_MODULATOR=sd` or `pwm`), the
//! transmitter runs in TDM mode with two 16-bit channels and no
//! clock pins, so its data line carries the ring's bits at the clip's
//! bit rate (`Stream::bit_rate`). Sixteen bits and not eight, though
//! the stream is bytes: the converter's samples are 16 bits wide, and
//! a transmitter that has run at 8 bits now and then goes on taking
//! one byte a sample when it is next set up for 16, whatever its
//! registers say, through the peripheral's reset and every start
//! after, until it has run at 8 bits again: a converter clip then
//! plays at half its rate with the pacer filling the ring at the full
//! one (about one start in sixteen after a beeper clip on the bench,
//! 2026-11-17; `HISTORY.md`). With one width for both the transmitter
//! is never in the other. A 16-bit word goes out high byte first and
//! is read from memory low byte first, so `play` swaps each pair of
//! the stream's bytes in the ring. The ring is `SLOTS` descriptors of `CHUNK` bytes,
//! the last pointing at the first, and the DMA goes round it until it
//! is stopped. The C3's GDMA reads internal RAM only, so the clip stays
//! in flash and `core::clip::Stream` writes its bits into the slots a few
//! chunks ahead of the DMA; `core::clip::Pacer` says which, by the
//! clock.
//!
//! The line goes out of the pin the `output` setting names when the
//! clip starts (`buzzer::Pins`). On the line output, GPIO20, it is
//! push-pull with no inverter and rests low: the stream's zeros are the
//! bottom rail, which its ramps start from and return to, and the
//! thermometer's hold is not asked for, there being no transistor to
//! heat. A clip cut short there is not cut at once: the chunk two on
//! from the one being read is rewritten as a ramp down from the sample
//! before it (`clip::fade`) with the rest after, and the pin is taken
//! back when that has gone out, a tenth of a second later at 16 kHz;
//! taken back at once, from the middle of the scale, it was a click in
//! the speaker at every Stop (heard 2026-11-18). A stream of bits has
//! no level to ramp and is cut as before. The rest of this is the
//! buzzer's pin.
//!
//! The pin is the one that must never be pushed high (see `buzzer.rs`).
//! It is handed over as an open-drain output already released, which
//! esp-hal turns into a frozen signal no driver reconfigures, and
//! connected to the data line through the GPIO matrix's inverter: a 1
//! bit pulls the line low, a 0 releases it. It is connected after the
//! transfer has started, on a stream that begins with zeros, and taken
//! back as a plain released output before the transfer is stopped, so
//! what the data line idles at never reaches the buzzer. Taking it back
//! also clears the inverter, which the tones' LEDC must not inherit.
//!
//! The clip has a task of its own, `clip_task`, on the interrupt
//! executor the scan task is on: the ring holds a quarter of a second
//! at most, and on the thread executor, where the buzzer task and its
//! tones stay (LEDC's driver is not `Send`), Wi-Fi work held the
//! topping-up off long enough to be heard as skips (2026-11-12). The
//! buzzer task asks for a clip with `play` and waits; the two share
//! the pin by turns, the clip task holding a second handle to it that
//! it uses only between a `play` and its end, while the buzzer task
//! waits and LEDC is off the pin.
//!
//! Beside `ring.rs` this is the other module with register access: the
//! DMA channel's descriptor register, which the pacer only checks the
//! clock against (it names the slot two on from the one being read).

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering, fence};

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::dma::aligned::{DmaAlignedMut, InternalMemory};
use esp_hal::dma::{BurstConfig, DmaDescriptor, DmaTxBuffer, Owner, Preparation};
use esp_hal::gpio::OutputSignal;
use esp_hal::i2s::master::{Channels, DataFormat, I2s, PdmConfig, PdmSlotMode, PdmTxConfig, TdmConfig};
use esp_hal::peripherals::{DMA, DMA_CH1, I2S0};
use esp_hal::time::Rate;
use onebitdawn_core::clip::{CHUNK, Pacer, SLOTS, Stream, chunk_us, chunks_in, converter_clock, fade};
use log::{info, warn};
use static_cell::ConstStaticCell;

use crate::buzzer::Pins;
use crate::melody;
use crate::settings::{self, SoundPin};
use crate::watchdog::{self, Party};

/// The ring's bytes. In every build, with clips built in or not: the
/// clips may be a pack's (`pack.rs`).
const RING_BYTES: usize = SLOTS * CHUNK;
/// Bits in one TDM frame: two channels of sixteen.
const FRAME_BITS: u32 = 32;
const _: () = assert!(CHUNK.is_multiple_of(2));
/// The converter's up-sampling numerator, the IDF's; the denominator
/// under it gives the ratio: 480, the largest the driver takes, for
/// two, 240 for four, 160 for six.
const PDM_FP: u32 = 960;
/// Bits a sample at a ratio of one.
const PDM_BASE_RATIO: u32 = 64;
/// The DMA channel the ring is on, as the register block counts them.
const DMA_CHANNEL: usize = 1;

static DESCRIPTORS: ConstStaticCell<InternalMemory<[DmaDescriptor; SLOTS]>> =
    ConstStaticCell::new(InternalMemory::new([DmaDescriptor::EMPTY; SLOTS]));
/// From the buzzer task to the clip task: play this clip, and stop.
/// And back: the clip has ended, the line released.
static START: Signal<CriticalSectionRawMutex, (usize, bool)> = Signal::new();
static STOP: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static DONE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

static BYTES: ConstStaticCell<InternalMemory<[u8; RING_BYTES]>> = ConstStaticCell::new(InternalMemory::new([0; RING_BYTES]));

/// What the sampled sound needs beside the buzzer pin.
pub struct Hardware {
    pub i2s: I2S0<'static>,
    pub dma: DMA_CH1<'static>,
}

/// The descriptor ring over the byte buffer.
struct Ring {
    descriptors: DmaAlignedMut<'static, [DmaDescriptor; SLOTS]>,
    bytes: DmaAlignedMut<'static, [u8; RING_BYTES]>,
}

impl Ring {
    fn new() -> Self {
        let mut descriptors = DESCRIPTORS.take().get_mut();
        let mut bytes = BYTES.take().get_mut();
        let first: *mut DmaDescriptor = descriptors.as_mut_ptr();
        let base = bytes.as_mut_ptr();
        for (i, d) in descriptors.iter_mut().enumerate() {
            d.set_owner(Owner::Dma);
            d.set_suc_eof(false);
            d.set_size(CHUNK);
            d.set_length(CHUNK);
            d.buffer = unsafe { base.add(i * CHUNK) };
            d.next = unsafe { first.add((i + 1) % SLOTS) };
        }
        Self { descriptors, bytes }
    }
}

unsafe impl DmaTxBuffer for Ring {
    type View = Ring;
    type Final = Ring;

    fn prepare(&mut self) -> Preparation {
        // No data cache on the C3; the fence keeps the fill ahead of the start.
        fence(Ordering::SeqCst);
        Preparation {
            start: self.descriptors.as_mut_ptr(),
            burst_transfer: BurstConfig::default(),
            // The descriptors are never written back, so the owner bit
            // would stop the second lap.
            check_owner: Some(false),
            auto_write_back: false,
        }
    }

    fn into_view(self) -> Ring {
        self
    }

    fn from_view(view: Ring) -> Ring {
        view
    }
}

/// Where the clip being played is in memory, 0 for none.
static PLAYING: AtomicUsize = AtomicUsize::new(0);

/// Whether a clip is being played from the `len` bytes at `start`: the
/// mapped clip pack, which must not be written while one is.
pub fn playing_in(start: usize, len: usize) -> bool {
    PLAYING.load(Ordering::Relaxed).wrapping_sub(start) < len
}

/// Set while a clip goes through the chip's converter.
static CONVERTING: AtomicBool = AtomicBool::new(false);

/// Whether a clip is on the buzzer's line through the chip's PDM converter: its
/// pulses are shorter than the buzzer's transistor switches, which
/// then runs hot beside the thermometer. For `sensors`, which holds
/// the reading.
pub fn converting() -> bool {
    CONVERTING.load(Ordering::Relaxed)
}

/// Play clip `index` of the melody table (`melody::clip_data`) on the clip task: once,
/// or `looping`, round and round with no gap until cancelled.
/// Returns early with what `cancel` was signalled with, the line
/// released by then; `None` when the clip has ended, or could not be
/// started. For the buzzer task, which must leave the pin alone until
/// this returns.
pub async fn play<T: Send>(index: usize, looping: bool, cancel: &Signal<CriticalSectionRawMutex, T>) -> Option<T> {
    STOP.reset();
    DONE.reset();
    START.signal((index, looping));
    match select(DONE.wait(), cancel.wait()).await {
        Either::First(()) => None,
        Either::Second(c) => {
            STOP.signal(());
            DONE.wait().await;
            Some(c)
        }
    }
}

/// Plays the clips the buzzer task asks for. `pins` is a second handle
/// to the buzzer task's pins, used only while a clip plays.
#[embassy_executor::task]
pub async fn clip_task(hw: Hardware, pins: Pins) -> ! {
    let mut sampler = Sampler::new(hw);
    let mut pins = pins;
    loop {
        let (index, looping) = START.wait().await;
        // The data and the note of where it is are taken in one step, with
        // no await between: `pack::install`, on the thread executor, takes
        // the pack out of the table and then waits for this to be clear
        // of it.
        if let Some(data) = melody::clip_data(index) {
            PLAYING.store(data.as_ptr() as usize, Ordering::Relaxed);
            // Watched from here to the clip's end; the loop that fills
            // the ring checks in.
            watchdog::expect(Party::Clip, true);
            sampler.play(&mut pins, data, looping, &STOP).await;
            watchdog::expect(Party::Clip, false);
            PLAYING.store(0, Ordering::Relaxed);
        }
        DONE.signal(());
    }
}

/// Plays clips. Owns I2S0, its DMA channel and the ring; borrows the
/// buzzer pin for each clip.
struct Sampler {
    hw: Hardware,
    /// `None` while a transfer holds it.
    ring: Option<Ring>,
}

impl Sampler {
    /// Panics if called twice.
    fn new(hw: Hardware) -> Self {
        Self { hw, ring: Some(Ring::new()) }
    }

    /// Play the clip `data`, PCM or beeper, once on the pin the `output` setting names. Returns early with what `cancel` was
    /// signalled with, the line released by then; `None` when the clip
    /// has ended, or could not be started.
    async fn play<T: Send>(
        &mut self,
        pins: &mut Pins,
        data: &'static [u8],
        looping: bool,
        cancel: &Signal<CriticalSectionRawMutex, T>,
    ) -> Option<T> {
        let mut ring = self.ring.take()?;
        // The slots are written through these while the transfer owns
        // the ring. The DMA is the only other party, and the pacer
        // keeps the writes off the slots it is reading.
        // (As an address: a pointer held across an await would keep the
        // task off the interrupt executor, whose futures must be `Send`.)
        let base = ring.bytes.as_mut_ptr() as usize;
        let first = ring.descriptors.as_ptr() as usize;
        let slot = |chunk: u32| unsafe { core::slice::from_raw_parts_mut((base as *mut u8).add(chunk as usize % SLOTS * CHUNK), CHUNK) };

        let mut player = Stream::new(data, looping);
        // Set for a clip that goes through the chip's converter.
        let (converter, sample_rate) = (player.converter(), player.sample_rate());
        // The rate the ring is read at. A beeper clip has its own; see
        // `clip::BEEPER_BIT_RATE`. The converter's is what its whole
        // dividers come to.
        let (bclk_div, bit_rate) = match converter {
            None => (0, player.bit_rate()),
            Some(ratio) => converter_clock(sample_rate, ratio),
        };
        // A rate the converter's dividers cannot make: `clip::FAST_HZ`
        // at 384 bits a sample would be 12 Mbit/s.
        if bit_rate == 0 {
            warn!("clip: no clock for {sample_rate} Hz at {converter:?} bits a sample");
            self.ring = Some(ring);
            return None;
        }
        // The ring is topped up twice a chunk.
        let tick = Duration::from_micros(chunk_us(bit_rate) / 2);
        player.rest(slot(SLOTS as u32 - 1));
        // The chunk the clip's last bit is in, once it has been written.
        let mut last = None;
        // Time spent making the stream, for the log: a player's share
        // of the processor.
        let mut busy_us = 0u64;
        let mut write = |chunk: u32, last: &mut Option<u32>| {
            let from = Instant::now();
            let slot = slot(chunk);
            player.fill(slot);
            // A stream of bytes goes out as 16-bit words, the high byte
            // of each first: see the module's head.
            if converter.is_none() {
                for pair in slot.as_chunks_mut::<2>().0 {
                    pair.swap(0, 1);
                }
            }
            busy_us += from.elapsed().as_micros();
            if last.is_none() && player.done() {
                *last = Some(chunk);
            }
        };
        for chunk in 0..SLOTS as u32 - 1 {
            write(chunk, &mut last);
            // A chunk is a millisecond or two of work, and this is the
            // interrupt executor: let the threads in between chunks.
            Timer::after(Duration::from_micros(500)).await;
        }

        let i2s = match converter {
            None => {
                let config = TdmConfig::new_tdm_msb()
                    .with_sample_rate(Rate::from_hz(bit_rate / FRAME_BITS))
                    .with_channels(Channels::STEREO)
                    .with_data_format(DataFormat::Data16Channel16);
                I2s::new(self.hw.i2s.reborrow(), self.hw.dma.reborrow(), config)
            }
            // The chip's PCM-to-PDM converter: the ring is then 16-bit
            // samples, one channel, and the line `ratio` bits a sample. The DAC line mode is
            // the one channel all the time, with no clock to tell two
            // apart by. No high-pass: the stream's rest and its ramps
            // are levels, and the tune has had its own.
            Some(ratio) => {
                let mut config = PdmTxConfig::new_dac_default(Rate::from_hz(sample_rate), PdmSlotMode::Mono);
                config.clock.up_sample_fp = PDM_FP;
                config.clock.up_sample_fs = PDM_FP * PDM_BASE_RATIO / ratio;
                config.clock.bclk_div = bclk_div;
                config.slot = config.slot.with_hp_en(false);
                let i2s = I2s::new_pdm(self.hw.i2s.reborrow(), self.hw.dma.reborrow(), PdmConfig::tx_only(config));
                // esp-hal 1.2.2 clears `tx_pcm_bypass` for PDM, which puts
                // the A-law compander in the samples' way (white noise on
                // the line, 2026-11-13); the IDF leaves it bypassed.
                let regs = I2S0::regs();
                regs.tx_conf().modify(|_, w| w.tx_pcm_bypass().set_bit());
                regs.tx_conf().modify(|_, w| w.tx_update().set_bit());
                while regs.tx_conf().read().tx_update().bit_is_set() {}
                i2s
            }
        };
        let tx = match i2s {
            Ok(i2s) => i2s.i2s_tx.build(),
            Err(e) => {
                warn!("clip: i2s config failed: {e:?}");
                self.ring = Some(ring);
                return None;
            }
        };
        settle_clock();
        let transfer = match tx.write(ring) {
            Ok(t) => t,
            Err((e, _, ring)) => {
                warn!("clip: dma start failed: {e:?}");
                self.ring = Some(ring);
                return None;
            }
        };
        let started = Instant::now();
        // The stream is under way and at zeros: connect the line. The
        // buzzer's through the inverter, a 1 pulling it low; the line
        // output's as it is, a 1 high.
        let out = settings::get().output.clips();
        // The amplifier is on already, the buzzer task's doing, but for
        // an output changed since the melody began.
        if out == SoundPin::Line {
            pins.amp_on();
        }
        let line = pins.at_rest(out).into_peripheral_output().with_output_inverter(out == SoundPin::Buzzer);
        OutputSignal::I2SO_SD.connect_to(&line);
        // The hold is for the buzzer's transistor.
        CONVERTING.store(converter.is_some() && out == SoundPin::Buzzer, Ordering::Relaxed);

        let mut pacer = Pacer::new();
        let mut chunks = 0;
        let cancelled = loop {
            if let Either::Second(c) = select(Timer::after(tick), cancel.wait()).await {
                break Some(c);
            }
            #[cfg(feature = "bench-hang")]
            watchdog::bench::park(Party::Clip).await;
            watchdog::check_in(Party::Clip);
            let played = pacer.played(position(first), chunks_in(started.elapsed().as_micros(), bit_rate));
            // Past the last chunk and the one after, which is zeros.
            if last.is_some_and(|last| played > last + 1) {
                chunks = played;
                break None;
            }
            for chunk in pacer.due(played) {
                write(chunk, &mut last);
            }
        };

        // Cut short on the line output: down to the rest by a ramp
        // before the pin is taken, in the first chunk the pacer would
        // write after an underrun, which the DMA is clear of.
        if cancelled.is_some() && converter.is_some() && out == SoundPin::Line {
            let at = pacer.played(position(first), chunks_in(started.elapsed().as_micros(), bit_rate)) + 2;
            let before = slot(at - 1);
            let from = i16::from_le_bytes([before[CHUNK - 2], before[CHUNK - 1]]);
            fade(from, slot(at));
            player.rest(slot(at + 1));
            player.rest(slot(at + 2));
            while chunks_in(started.elapsed().as_micros(), bit_rate) <= at {
                Timer::after(tick).await;
                watchdog::check_in(Party::Clip);
            }
        }

        // The pin first, back to a plain output at its rest with no
        // inverter; then the transfer.
        pins.rest(out);
        CONVERTING.store(false, Ordering::Relaxed);
        let (_, ring) = transfer.stop();
        self.ring = Some(ring);
        let (busy, whole) = (busy_us / 1000, started.elapsed().as_millis());
        match cancelled {
            Some(_) => info!("clip: cut short, {} underruns, {} stray positions, {busy} of {whole} ms filling", pacer.underruns(), pacer.strays()),
            None => info!("clip: {chunks} chunks, {} underruns, {} stray positions, {busy} of {whole} ms filling", pacer.underruns(), pacer.strays()),
        }
        cancelled
    }
}

/// Write the master clock's divider again the way the IDF's
/// `i2s_ll_tx_set_raw_clk_div` does for this chip (v6.0.1, "the double
/// division issue. The division coefficients must be set in particular
/// sequence. And it has to switch to a small division first"): a
/// divider of 2 with y alone set, then the target's `yn1`, z, y, x and
/// the whole part, in that order. esp-hal 1.2.2 does it on the chips
/// with a PCR only. Without it the divider's fraction is not what the
/// registers say on every start but the first after a reset: a beeper
/// clip's 39 1/16 ran 0.16 % off, a chunk in ten seconds against the
/// pacer's clock (2026-11-18; `agents/hardware.md`). The converter's
/// divider is whole and showed nothing; it gets the same, as in the
/// IDF. Call with the transmitter configured and not started.
///
/// A stopgap for esp-hal 1.2.2: its `main` does this sequence itself
/// for this chip (`i2s/clocks/v2v3.rs`, seen 2026-11-18). Drop this
/// function and its call when the project moves to a release that
/// has it.
fn settle_clock() {
    let regs = I2S0::regs();
    let (conf, div) = (regs.tx_clkm_conf().read(), regs.tx_clkm_div_conf().read());
    let (num, x, y, z, yn1) =
        (conf.tx_clkm_div_num().bits(), div.tx_clkm_div_x().bits(), div.tx_clkm_div_y().bits(), div.tx_clkm_div_z().bits(), div.tx_clkm_div_yn1().bit());
    let (clkm, coef) = (regs.tx_clkm_conf(), regs.tx_clkm_div_conf());
    // A field a write, as the IDF's bit-field stores are.
    clkm.modify(|_, w| unsafe { w.tx_clkm_div_num().bits(2) });
    coef.modify(|_, w| w.tx_clkm_div_yn1().clear_bit());
    coef.modify(|_, w| unsafe { w.tx_clkm_div_y().bits(1) });
    coef.modify(|_, w| unsafe { w.tx_clkm_div_z().bits(0) });
    coef.modify(|_, w| unsafe { w.tx_clkm_div_x().bits(0) });
    coef.modify(|_, w| w.tx_clkm_div_yn1().bit(yn1));
    coef.modify(|_, w| unsafe { w.tx_clkm_div_z().bits(z) });
    coef.modify(|_, w| unsafe { w.tx_clkm_div_y().bits(y) });
    coef.modify(|_, w| unsafe { w.tx_clkm_div_x().bits(x) });
    clkm.modify(|_, w| unsafe { w.tx_clkm_div_num().bits(num) });
    regs.tx_conf().modify(|_, w| w.tx_update().set_bit());
    while regs.tx_conf().read().tx_update().bit_is_set() {}
}

/// The slot whose descriptor the DMA channel names, which is
/// `Pacer::DSCR_AHEAD` on from the one it is reading; `None` for an
/// address outside the ring.
fn position(first: usize) -> Option<usize> {
    let size = core::mem::size_of::<DmaDescriptor>();
    let addr = DMA::regs().ch(DMA_CHANNEL).out_dscr().read().outlink_dscr().bits() as usize;
    let off = addr.wrapping_sub(first);
    (off < SLOTS * size).then_some(off / size)
}

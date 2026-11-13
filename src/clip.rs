// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! Sampled sound on the buzzer line: I2S0's transmitter as a 1-bit
//! output, fed by DMA from a ring the buzzer task keeps filled.
//!
//! The transmitter runs in TDM mode with two 8-bit channels and no
//! clock pins, so its data line carries the ring's bytes MSB first at
//! the clip's bit rate (`Stream::bit_rate`). The ring is `SLOTS` descriptors of `CHUNK` bytes,
//! the last pointing at the first, and the DMA goes round it until it
//! is stopped. The C3's GDMA reads internal RAM only, so the clip stays
//! in flash and `core::clip::Stream` writes its bits into the slots a few
//! chunks ahead of the DMA; `core::clip::Pacer` says which, by the
//! clock.
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

use core::sync::atomic::{Ordering, fence};

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::dma::aligned::{DmaAlignedMut, InternalMemory};
use esp_hal::dma::{BurstConfig, DmaDescriptor, DmaTxBuffer, Owner, Preparation};
use esp_hal::gpio::{DriveMode, Level, Output, OutputConfig, OutputSignal};
use esp_hal::i2s::master::{Channels, DataFormat, I2s, TdmConfig};
use esp_hal::peripherals::{DMA, DMA_CH1, GPIO5, I2S0};
use esp_hal::time::Rate;
use esp32clock_core::clip::{CHUNK, Pacer, SLOTS, Stream, chunk_us, chunks_in};
use log::{info, warn};
use static_cell::ConstStaticCell;

use crate::melody;

/// The ring's bytes; none in a build without a clip.
const RING_BYTES: usize = if melody::HAS_CLIP { SLOTS * CHUNK } else { 0 };
/// Bits in one TDM frame: two channels of eight.
const FRAME_BITS: u32 = 16;
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

/// Play clip `index` of `melody::CLIP_DATA` on the clip task: once,
/// or `looping`, round and round with no gap until cancelled.
/// Returns early with what `cancel` was signalled with, the line
/// released by then; `None` when the clip has ended, or could not be
/// started. For the buzzer task, which must leave the pin alone until
/// this returns.
pub async fn play<T: Send>(index: usize, looping: bool, cancel: &Signal<CriticalSectionRawMutex, T>) -> Option<T> {
    if !melody::HAS_CLIP {
        return None;
    }
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

/// Plays the clips the buzzer task asks for. `pin` is a second handle
/// to the buzzer's pin, used only while a clip plays.
#[embassy_executor::task]
pub async fn clip_task(hw: Hardware, pin: GPIO5<'static>) -> ! {
    let mut sampler = Sampler::new(hw);
    let mut pin = pin;
    loop {
        let (index, looping) = START.wait().await;
        if let Some(data) = melody::CLIP_DATA.get(index) {
            sampler.play(&mut pin, data, looping, &STOP).await;
        }
        DONE.signal(());
    }
}

/// Plays clips. Owns I2S0, its DMA channel and the ring; borrows the
/// buzzer pin for each clip.
struct Sampler {
    hw: Hardware,
    /// `None` in a build without a clip, and while a transfer holds it.
    ring: Option<Ring>,
}

impl Sampler {
    /// Panics if called twice in a build with a clip.
    fn new(hw: Hardware) -> Self {
        Self { hw, ring: melody::HAS_CLIP.then(Ring::new) }
    }

    /// Play the clip `data`, PCM or beeper, once on `pin`. Returns early with what `cancel` was
    /// signalled with, the line released by then; `None` when the clip
    /// has ended, or could not be started.
    async fn play<T: Send>(
        &mut self,
        pin: &mut GPIO5<'static>,
        data: &'static [u8],
        looping: bool,
        cancel: &Signal<CriticalSectionRawMutex, T>,
    ) -> Option<T> {
        // A constant in a build without a clip, so none of this is in its image.
        if !melody::HAS_CLIP {
            return None;
        }
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
        // A beeper clip has its own; see `clip::BEEPER_BIT_RATE`.
        let bit_rate = player.bit_rate();
        // The ring is topped up twice a chunk.
        let tick = Duration::from_micros(chunk_us(bit_rate) / 2);
        // The chunk the clip's last bit is in, once it has been written.
        let mut last = None;
        let mut write = |chunk: u32, last: &mut Option<u32>| {
            player.fill(slot(chunk));
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
        slot(SLOTS as u32 - 1).fill(0);

        let config = TdmConfig::new_tdm_msb()
            .with_sample_rate(Rate::from_hz(bit_rate / FRAME_BITS))
            .with_channels(Channels::STEREO)
            .with_data_format(DataFormat::Data8Channel8);
        let tx = match I2s::new(self.hw.i2s.reborrow(), self.hw.dma.reborrow(), config) {
            Ok(i2s) => i2s.i2s_tx.build(),
            Err(e) => {
                warn!("clip: i2s config failed: {e:?}");
                self.ring = Some(ring);
                return None;
            }
        };
        let transfer = match tx.write(ring) {
            Ok(t) => t,
            Err((e, _, ring)) => {
                warn!("clip: dma start failed: {e:?}");
                self.ring = Some(ring);
                return None;
            }
        };
        let started = Instant::now();
        // The stream is under way and at zeros: connect the line.
        let line = Output::new(pin.reborrow(), Level::High, OutputConfig::default().with_drive_mode(DriveMode::OpenDrain))
            .into_peripheral_output()
            .with_output_inverter(true);
        OutputSignal::I2SO_SD.connect_to(&line);

        let mut pacer = Pacer::new();
        let mut chunks = 0;
        let cancelled = loop {
            if let Either::Second(c) = select(Timer::after(tick), cancel.wait()).await {
                break Some(c);
            }
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

        // The pin first, back to a plain released output with no
        // inverter; then the transfer.
        release(pin);
        let (_, ring) = transfer.stop();
        self.ring = Some(ring);
        match cancelled {
            Some(_) => info!("clip: cut short, {} underruns, {} stray positions", pacer.underruns(), pacer.strays()),
            None => info!("clip: {chunks} chunks, {} underruns, {} stray positions", pacer.underruns(), pacer.strays()),
        }
        cancelled
    }
}

/// Make the pin a released open-drain output of the GPIO register,
/// whatever it was connected to. The pad keeps this after the `Output`
/// is dropped, as it does in `main.rs` at boot.
fn release(pin: &mut GPIO5<'static>) {
    let _ = Output::new(pin.reborrow(), Level::High, OutputConfig::default().with_drive_mode(DriveMode::OpenDrain));
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

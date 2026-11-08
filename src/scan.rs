// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! Display scan engine: SPI2 in quad mode as a four-wire pattern generator.
//!
//! One DMA transfer carries several whole multiplex cycles back to back.
//! Each COM slot is split into binary-weighted sub-frame regions of 1, 2, 4
//! and 8 units; a region is a frame to both drivers followed by zero padding
//! that holds the latched output for the rest of the region. A channel at
//! level L is lit in region k when bit k of L is set, so its lit time per
//! slot is L units.
//!
//! The cycle is short on purpose. A dim channel is one short pulse per
//! cycle, and pulses at a few hundred hertz are seen as shimmer whenever the
//! eyes move, so the cycle runs in the kilohertz. Frames cost the CPU
//! nothing here, which is what makes that free.
//!
//! The transfer ends with an all-off frame so that the gap between one
//! transfer ending and the next starting is dark rather than leaving the
//! last COM pair lit for longer than the other three.

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, Ordering};

use critical_section::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Instant, Timer};
use esp_hal::dma::DmaTxBuf;
use esp_hal::gpio::Flex;
use esp_hal::peripherals::{DMA_CH0, SPI2};
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Address, Command, Config as SpiConfig, DataMode, Spi};
use esp_hal::time::Rate;
use log::{error, info};

/// Dark time between one DMA transfer ending and the next starting,
/// accumulated by the scan task and read by whoever wants to report it.
#[derive(Clone, Copy, Default, Debug)]
pub struct GapStats {
    pub transfers: u32,
    pub sum_us: u64,
    pub max_us: u64,
}

static GAP: Mutex<Cell<GapStats>> = Mutex::new(Cell::new(GapStats {
    transfers: 0,
    sum_us: 0,
    max_us: 0,
}));

/// Read and reset the gap statistics.
pub fn take_gap_stats() -> GapStats {
    critical_section::with(|cs| GAP.borrow(cs).replace(GapStats::default()))
}

fn record_gap(us: u64) {
    critical_section::with(|cs| {
        let cell = GAP.borrow(cs);
        let mut g = cell.get();
        g.transfers += 1;
        g.sum_us += us;
        g.max_us = g.max_us.max(us);
        cell.set(g);
    });
}

use crate::aip33628::{FRAME_BYTES, encode_pair};
use crate::panel::{COM_SEQ, PLANES, SLOTS, ScanImage};

/// SPI clock. One nibble, i.e. one state of the four wires, per period.
/// 20 MHz gives 50 ns clock phases, about what the reference firmware's
/// register-store bit-bang drives the same panel with at 3.3 V. If a panel
/// misreads at this rate, drop to 16 MHz with a 6 µs unit, or 8 MHz with 12.
pub const SPI_HZ: u32 = 20_000_000;

/// Microseconds in one sub-frame unit, the smallest colour step. A frame
/// (47 bytes) must fit in one unit, so this is bounded below by the clock.
pub const UNIT_US: usize = 5;

/// The SPI driver refuses transfers above 32736 bytes.
const MAX_TRANSFER_BYTES: usize = 32_000;

/// Cycles packed into one DMA transfer: as many as fit, so the restart gap
/// is as rare as it can be.
pub const CYCLES_PER_TRANSFER: usize = (MAX_TRANSFER_BYTES - FRAME_BYTES) / CYCLE_BYTES;

/// Bytes per unit. Two nibbles per byte.
pub const UNIT_BYTES: usize = UNIT_US * (SPI_HZ as usize / 1_000_000) / 2;

/// Units per slot: 1 + 2 + 4 for three planes.
pub const SLOT_UNITS: usize = (1 << PLANES) - 1;

/// Bytes per COM slot.
pub const SLOT_BYTES: usize = UNIT_BYTES * SLOT_UNITS;

/// Bytes per cycle: four slots.
pub const CYCLE_BYTES: usize = SLOTS * SLOT_BYTES;

/// Microseconds per cycle: four slots of `SLOT_UNITS` units each.
pub const CYCLE_US: usize = SLOTS * UNIT_US * SLOT_UNITS;

/// Bytes per DMA transfer: the cycles plus the trailing blanking frame.
pub const TRANSFER_BYTES: usize = CYCLES_PER_TRANSFER * CYCLE_BYTES + FRAME_BYTES;

// A frame has to fit inside the shortest region, with its latch at the same
// offset in every region so that lit time equals region length.
const _: () = assert!(UNIT_BYTES >= FRAME_BYTES);
const _: () = assert!(TRANSFER_BYTES <= MAX_TRANSFER_BYTES);
const _: () = assert!(CYCLES_PER_TRANSFER >= 1);

/// Fill `out` with one scan cycle for `img`, no blanking.
///
/// Plane-major order: for each plane, every COM slot in turn. A digit is
/// then lit once per plane rather than once per cycle, which shortens its
/// longest dark stretch and pushes the multiplex modulation higher in
/// frequency, where the eye no longer resolves it during a saccade.
pub fn build_cycle(img: &ScanImage, out: &mut [u8]) {
    let out = &mut out[..CYCLE_BYTES];
    let mut off = 0;
    for plane in 0..PLANES {
        for (k, cs) in COM_SEQ.iter().enumerate() {
            let len = UNIT_BYTES << plane;
            let region = &mut out[off..off + len];
            let (frame, pad) = region.split_at_mut(FRAME_BYTES);
            encode_pair(
                img.ss[0][k][plane],
                img.ss[1][k][plane],
                *cs,
                img.is_step,
                frame.try_into().unwrap(),
            );
            pad.fill(0);
            off += len;
        }
    }
}

/// Fill `out` with one complete DMA transfer for `img`: the cycles back to
/// back, then the blanking frame so the inter-transfer gap is dark.
pub fn build_transfer(img: &ScanImage, out: &mut [u8]) {
    let out = &mut out[..TRANSFER_BYTES];
    let (cycles, blank) = out.split_at_mut(CYCLES_PER_TRANSFER * CYCLE_BYTES);
    build_cycle(img, cycles);
    let (first, rest) = cycles.split_at_mut(CYCLE_BYTES);
    for chunk in rest.as_chunks_mut::<CYCLE_BYTES>().0 {
        chunk.copy_from_slice(first);
    }
    encode_pair(0, 0, COM_SEQ[SLOTS - 1], img.is_step, blank.try_into().unwrap());
}

/// Buffers not in flight, ready to be filled. One of the two lives here
/// while the other is being transferred.
static FREE: Channel<CriticalSectionRawMutex, DmaTxBuf, 1> = Channel::new();

/// A filled buffer waiting for the scan task to swap it in.
static READY: Channel<CriticalSectionRawMutex, DmaTxBuf, 1> = Channel::new();

/// Set to end the scan at the next transfer boundary.
static STOP: AtomicBool = AtomicBool::new(false);

/// Set by the scan task once it has stopped, cleared when it resumes.
static HALTED: AtomicBool = AtomicBool::new(false);

/// Raised to let a stopped scan task run again.
static RESUME: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Latched by `halt`: the stop is for a reset, and `resume` is void.
static FINAL: AtomicBool = AtomicBool::new(false);

/// Stop the scan with the panel blank. Returns once the transfer in
/// flight has ended, on its blanking frame, and no other will start; the
/// wires then rest on their pull-downs. For a deliberate reset, which
/// must not land mid-transfer or the row the drivers had latched stays
/// lit at full current until the firmware is back, and for a firmware
/// upload, whose flash writes would otherwise flicker the panel. Takes
/// up to one transfer, about 3 ms. Safe to call while already stopped,
/// so a self-heal during an upload does not hang here: it polls a flag
/// rather than waiting on a one-shot signal.
pub async fn stop() {
    STOP.store(true, Ordering::Release);
    while !HALTED.load(Ordering::Acquire) {
        Timer::after_millis(1).await;
    }
}

/// Let a stopped scan run again, from the buffer it had. A no-op when
/// it is running, and void once `halt` has latched the stop for a
/// reset: an upload that fails while the reset is pausing must not put
/// a transfer under it. The flags are read and written inside one
/// critical section here and in `halt`, so the two cannot interleave.
pub fn resume() {
    critical_section::with(|_| {
        if !FINAL.load(Ordering::Acquire) && HALTED.load(Ordering::Acquire) {
            STOP.store(false, Ordering::Release);
            RESUME.signal(());
        }
    });
}

/// `stop`, latched: for a deliberate reset, after which nothing resumes
/// the scan, whatever an upload under way does.
pub async fn halt() {
    critical_section::with(|_| {
        FINAL.store(true, Ordering::Release);
        STOP.store(true, Ordering::Release);
    });
    stop().await
}

/// Give the publisher its spare buffer. Call once, before any `publish`.
pub fn init_spare(buf: DmaTxBuf) {
    assert!(FREE.try_send(buf).is_ok(), "spare buffer slot taken");
}

/// Hand a new image to the scan engine. Builds the transfer here, in the
/// caller's task, so the scan task never spends more than a few
/// microseconds between transfers. Takes effect at the next transfer
/// boundary, within about 3 ms.
pub async fn publish(img: &ScanImage) {
    let mut buf = FREE.receive().await;
    build_transfer(img, buf.as_mut_slice());
    READY.send(buf).await;
}

/// What the scan task builds its SPI bus from: the peripherals, the four
/// wires with their idle pull-downs already set (the builder leaves a
/// `Flex` pin's configuration alone, see `main.rs`), and the first DMA
/// buffer. The async driver is built in the task rather than handed to
/// it: esp-hal ties an async driver to the core its interrupt handler was
/// installed on and makes it `!Send` for that reason, so it is created
/// where it runs.
pub struct ScanHardware {
    pub spi: SPI2<'static>,
    pub dma: DMA_CH0<'static>,
    /// SIO0 to SIO3: DATA, CLK, DATA_1, CLK_1.
    pub wires: [Flex<'static>; 4],
    pub buf: DmaTxBuf,
}

/// Runs the display forever. Owns the SPI peripheral and the in-flight
/// DMA buffer; swaps it for a freshly built one whenever there is one.
#[embassy_executor::task]
pub async fn scan_task(hw: ScanHardware) -> ! {
    let ScanHardware { spi, dma, wires, mut buf } = hw;
    let [sio0, sio1, sio2, sio3] = wires;

    // SPI2 in quad mode, no SCK: the four data lines are the four panel
    // wires. See aip33628.rs for what goes down them.
    let mut spi = Spi::new(spi, SpiConfig::default().with_frequency(Rate::from_hz(SPI_HZ)).with_mode(Mode::_0))
        .expect("spi config")
        .with_sio0(sio0)
        .with_sio1(sio1)
        .with_sio2(sio2)
        .with_sio3(sio3)
        .with_dma(dma)
        .into_async();

    build_transfer(&ScanImage::DARK, buf.as_mut_slice());
    info!(
        "scan: {} us/cycle ({} Hz), {} us/unit, {} cycles and {} bytes per transfer",
        CYCLE_US,
        1_000_000 / CYCLE_US,
        UNIT_US,
        CYCLES_PER_TRANSFER,
        TRANSFER_BYTES
    );

    // Nothing slow may run between a transfer ending and the next starting:
    // that time is dark on the panel. No logging in this loop.
    let mut ended_at: Option<Instant> = None;

    loop {
        if let Some(t) = ended_at {
            record_gap(t.elapsed().as_micros());
        }
        let mut transfer = match spi.half_duplex_write_buffer(
            DataMode::Quad,
            Command::None,
            Address::None,
            0,
            TRANSFER_BYTES,
            buf,
        ) {
            Ok(t) => t,
            Err((e, s, b)) => {
                error!("scan: dma start failed: {e:?}");
                spi = s;
                buf = b;
                Timer::after_millis(100).await;
                continue;
            }
        };
        transfer.wait_for_done().await;
        (spi, buf) = transfer.wait();
        ended_at = Some(Instant::now());

        if STOP.load(Ordering::Acquire) {
            HALTED.store(true, Ordering::Release);
            // `resume` clears the stop before it signals; a stop set
            // again meanwhile (a reset's `halt`) keeps the task here.
            loop {
                RESUME.wait().await;
                if !STOP.load(Ordering::Acquire) {
                    break;
                }
            }
            HALTED.store(false, Ordering::Release);
            ended_at = None;
        }

        if let Ok(next) = READY.try_receive() {
            let done = core::mem::replace(&mut buf, next);
            // The publisher always returns the spare before asking for
            // another, so this slot is free.
            let _ = FREE.try_send(done);
        }
    }
}

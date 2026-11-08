// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The descriptor ring: the whole scan as one hardware-chained transfer.
//!
//! GP-SPI2's DMA-controlled configurable segmented transfer (ESP32-C3
//! TRM 27.5.8.5) reads a conf buffer from the DMA stream before each
//! segment: a bitmap word, with the magic value in bits 31:28 and bits
//! 0 to 13 naming the registers that follow, then one word per named
//! register. `SPI_USR_CONF_NXT` in the USER register says another
//! segment follows. The chain here is one segment, the conf buffer and
//! the data in whole cycles, with the last descriptor pointing back at
//! the first, so the hardware scans the panel for ever with no CPU in
//! the loop. The only gap is the CONF state after the blanking frame, a
//! fixed 0.6 µs (measured 2026-11-08: 19 447 laps in 60.000 s against
//! 3084.7 µs of data per lap), with the lines idle through it (scope).
//! Before this the CPU restarted every transfer, 10 to 19 µs each time
//! and up to 1 ms when a critical section held the interrupt off.
//!
//! The conf buffer carries the bitmap and the USER register, so the
//! chain is ended the way the manual intends: clear `usr_conf_nxt` in
//! the buffer's USER word, and the segment after the next CONF state is
//! the last. Nothing else changes between laps, so no other register is
//! named.
//!
//! A new image is published by pointing the data descriptors at the
//! other buffer: eleven aligned stores the DMA reads as it reaches each
//! descriptor, so a lap in flight changes image at a cycle boundary.
//! The buffer it left may still be read for the rest of that lap, so the
//! publisher does not build into it until two laps have passed.
//!
//! The spike this grew from (2026-11-08) also counted laps by the GDMA's
//! `out_eof` and routed the chip select to a pin for the scope; see
//! PLAN.md.

use core::cell::RefCell;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering, fence};

use critical_section::Mutex;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::dma::aligned::{DmaAlignedMut, InternalMemory};
use esp_hal::dma::{BurstConfig, CHUNK_SIZE, DmaDescriptor, DmaTxBuffer, Owner, Preparation};
use esp_hal::peripherals::{DMA, SPI2};
use static_cell::ConstStaticCell;

use crate::aip33628::FRAME_BYTES;
use crate::panel::ScanImage;
use crate::scan::{CYCLE_BYTES, CYCLES_PER_TRANSFER, TRANSFER_BYTES, TRANSFER_NS, build_transfer};

/// Cycles per data descriptor. Whole cycles, so an image swap at
/// descriptor granularity lands on a cycle boundary; two of them plus
/// the blanking frame stay under the descriptor's 4092-byte limit.
const CYCLES_PER_DESC: usize = 2;
/// Data descriptors in the segment; the last also carries the blanking frame.
const DATA_DESCS: usize = CYCLES_PER_TRANSFER.div_ceil(CYCLES_PER_DESC);
/// The conf descriptor and the data descriptors.
pub const DESCS: usize = 1 + DATA_DESCS;
const _: () = assert!(CYCLES_PER_DESC * CYCLE_BYTES + FRAME_BYTES <= CHUNK_SIZE);
const _: () = assert!(DATA_DESCS * CYCLES_PER_DESC >= CYCLES_PER_TRANSFER);

/// Magic in bits 31:28 of the bitmap word and in `SPI_DMA_SEG_MAGIC_VALUE`;
/// the manual's example value.
const MAGIC: u8 = 0xA;
/// Bitmap bit 3: the USER register follows.
const BM_USER: u32 = 1 << 3;
/// `SPI_USR_CONF_NXT`, bit 15 of `SPI_USER_REG`.
const USR_CONF_NXT: u32 = 1 << 15;
/// Words in the conf buffer: the bitmap and the USER register.
const CONF_WORDS: usize = 2;
/// `SPI_CONF_BITLEN`: the CONF state lasts (value + 5) APB clocks of
/// 12.5 ns. The shortest; the conf fetch dominates the gap anyway.
const CONF_BITLEN: u32 = 0;

/// One lap on the wire plus the gap, rounded up.
pub const LAP: Duration = Duration::from_micros(TRANSFER_NS / 1000 + 2);
/// How long after a swap the buffer it left may still be read: the rest
/// of the lap in flight, with a lap of margin.
const HOLD: Duration = Duration::from_micros(2 * (TRANSFER_NS / 1000 + 2) + 500);

static DESCRIPTORS: ConstStaticCell<InternalMemory<[DmaDescriptor; DESCS]>> =
    ConstStaticCell::new(InternalMemory::new([DmaDescriptor::EMPTY; DESCS]));
static CONF: ConstStaticCell<InternalMemory<[u32; CONF_WORDS]>> =
    ConstStaticCell::new(InternalMemory::new([0; CONF_WORDS]));

/// Address of the first descriptor, for the publisher's swap and the
/// report, while the ring itself is inside the transfer. Written once by
/// `Ring::new`.
static DESC_BASE: AtomicUsize = AtomicUsize::new(0);
/// Address of the conf buffer's USER word, for `request_stop`. Written
/// once by `Ring::new`.
static CONF_USER_PTR: AtomicUsize = AtomicUsize::new(0);
/// The USER register's value with `usr_conf_nxt` set, as programmed by
/// esp-hal for the running transfer. Written by `prepare`.
static USER_WORD: AtomicU32 = AtomicU32::new(0);

/// The descriptor ring: one conf descriptor, then the data in whole
/// cycles, the last descriptor pointing back at the first. Owns no
/// buffer memory: the data buffers belong to the publisher, which
/// repoints the descriptors.
pub struct Ring {
    descriptors: DmaAlignedMut<'static, [DmaDescriptor; DESCS]>,
    conf: DmaAlignedMut<'static, [u32; CONF_WORDS]>,
}

impl Ring {
    /// Build the ring over `live`, which holds one transfer as
    /// `build_transfer` writes it. Panics if called twice.
    pub fn new(live: &mut DmaAlignedMut<'static, [u8]>) -> Self {
        assert!(live.len() >= TRANSFER_BYTES);
        let mut descriptors = DESCRIPTORS.take().get_mut();
        let mut conf = CONF.take().get_mut();
        conf[0] = (MAGIC as u32) << 28 | BM_USER;
        CONF_USER_PTR.store(conf.as_mut_ptr().wrapping_add(1) as usize, Ordering::Release);

        let first: *mut DmaDescriptor = descriptors.as_mut_ptr();
        DESC_BASE.store(first as usize, Ordering::Release);
        let conf_bytes = CONF_WORDS * 4;
        {
            let d = &mut descriptors[0];
            d.set_owner(Owner::Dma);
            d.set_suc_eof(false);
            d.set_size(conf_bytes);
            d.set_length(conf_bytes);
            d.buffer = conf.as_mut_ptr() as *mut u8;
            d.next = unsafe { first.add(1) };
        }
        let base = live.as_mut_ptr();
        for i in 0..DATA_DESCS {
            let last = i + 1 == DATA_DESCS;
            let len = if last { TRANSFER_BYTES - data_offset(i) } else { CYCLES_PER_DESC * CYCLE_BYTES };
            let d = &mut descriptors[1 + i];
            d.set_owner(Owner::Dma);
            d.set_suc_eof(false);
            d.set_size(len);
            d.set_length(len);
            d.buffer = unsafe { base.add(data_offset(i)) };
            d.next = if last { first } else { unsafe { first.add(2 + i) } };
        }
        Self { descriptors, conf }
    }
}

/// Byte offset of data descriptor `i` into a transfer buffer.
const fn data_offset(i: usize) -> usize {
    i * CYCLES_PER_DESC * CYCLE_BYTES
}

unsafe impl DmaTxBuffer for Ring {
    type View = Ring;
    type Final = Ring;

    /// Called by esp-hal after it has programmed the registers for the
    /// transfer, zeroed SLAVE among them, and before it sets `usr`: the
    /// place to switch the segmented mode on. The USER word for the conf
    /// buffer is read back from the register esp-hal programmed, so the
    /// CONF state writes back exactly the configuration in force, with
    /// the chain armed again after a stop.
    fn prepare(&mut self) -> Preparation {
        let spi = SPI2::regs();
        spi.slave().modify(|_, w| {
            w.usr_conf().set_bit();
            unsafe { w.dma_seg_magic_value().bits(MAGIC) }
        });
        spi.user().modify(|_, w| w.usr_conf_nxt().set_bit());
        spi.cmd().modify(|_, w| unsafe { w.conf_bitlen().bits(CONF_BITLEN) });
        let user = spi.user().read().bits() | USR_CONF_NXT;
        USER_WORD.store(user, Ordering::Relaxed);
        self.conf[1] = user;
        // No data cache on the C3; the fence keeps the store ahead of the start.
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

/// End the chain: clear `usr_conf_nxt` in the conf buffer's USER word.
/// The next CONF state applies it and that segment is the last, so the
/// transfer ends within two laps on its blanking frame and
/// `seg_trans_done` is raised.
pub fn request_stop() {
    let ptr = CONF_USER_PTR.load(Ordering::Acquire) as *mut u32;
    let word = USER_WORD.load(Ordering::Relaxed) & !USR_CONF_NXT;
    unsafe { ptr.write_volatile(word) };
    fence(Ordering::SeqCst);
}

/// Whether the chain has ended since the flag was last cleared.
pub fn ended() -> bool {
    SPI2::regs().dma_int_raw().read().dma_seg_trans_done().bit_is_set()
}

/// Clear the chain-ended flag, before the next start.
pub fn clear_ended() {
    SPI2::regs().dma_int_clr().write(|w| w.dma_seg_trans_done().clear_bit_by_one());
}

/// Whether the SPI is in a transfer (`SPI_USR`).
pub fn busy() -> bool {
    SPI2::regs().cmd().read().usr().bit_is_set()
}

/// Point the data descriptors at `buffer`. Each store is one aligned
/// word the DMA reads when it reaches the descriptor, so the image
/// changes at a cycle boundary, at most one lap from now.
fn point_at(buffer: *mut u8) {
    let base = DESC_BASE.load(Ordering::Acquire) as *mut DmaDescriptor;
    for i in 0..DATA_DESCS {
        let d = unsafe { base.add(1 + i) };
        let field = unsafe { &raw mut (*d).buffer };
        unsafe { field.write_volatile(buffer.add(data_offset(i))) };
    }
    fence(Ordering::SeqCst);
}

/// The two transfer buffers: the one the descriptors point at and the
/// spare, with when the last swap was, so the spare is not written while
/// the lap in flight may still read it.
struct Slots {
    live: DmaAlignedMut<'static, [u8]>,
    spare: DmaAlignedMut<'static, [u8]>,
    swapped_at: Option<Instant>,
}

/// Held by the publisher between calls; `None` until the scan task has
/// built the ring, and while a publish is under way.
static SLOTS: Mutex<RefCell<Option<Slots>>> = Mutex::new(RefCell::new(None));

/// Images published since boot, for the heartbeat. One writer.
static SWAPS: AtomicU32 = AtomicU32::new(0);

/// Hand the publisher its buffers, once the ring points at `live`.
pub fn init_slots(live: DmaAlignedMut<'static, [u8]>, spare: DmaAlignedMut<'static, [u8]>) {
    critical_section::with(|cs| {
        let slot = SLOTS.borrow(cs);
        assert!(slot.borrow().is_none(), "ring slots set twice");
        slot.replace(Some(Slots { live, spare, swapped_at: None }));
    });
}

/// Hand a new image to the scan: build it into the spare buffer and point
/// the ring at it. Waits for the ring to exist, and for the spare to be
/// clear of the lap that last read it. One publisher at a time, the
/// display task; a second would wait here for the first.
pub async fn publish(img: &ScanImage) {
    let mut slots = loop {
        if let Some(s) = critical_section::with(|cs| SLOTS.borrow(cs).take()) {
            break s;
        }
        Timer::after_millis(1).await;
    };
    if let Some(at) = slots.swapped_at {
        Timer::at(at + HOLD).await;
    }
    build_transfer(img, &mut slots.spare[..TRANSFER_BYTES]);
    point_at(slots.spare.as_mut_ptr());
    core::mem::swap(&mut slots.live, &mut slots.spare);
    slots.swapped_at = Some(Instant::now());
    SWAPS.store(SWAPS.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    critical_section::with(|cs| SLOTS.borrow(cs).replace(Some(slots)));
}

/// Images published since boot.
pub fn swaps() -> u32 {
    SWAPS.load(Ordering::Relaxed)
}

/// Where the DMA is in the ring: the index of the descriptor it is
/// reading, 0 the conf descriptor and 1 to `DESCS - 1` the data, or
/// `None` for an address outside the ring (before the first start).
pub fn position() -> Option<usize> {
    let base = DESC_BASE.load(Ordering::Acquire);
    let size = core::mem::size_of::<DmaDescriptor>();
    let addr = DMA::regs().ch(0).out_dscr().read().outlink_dscr().bits() as usize;
    let off = addr.wrapping_sub(base);
    (base != 0 && off < DESCS * size).then_some(off / size)
}

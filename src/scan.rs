//! Display scan engine: SPI2 in quad mode as a four-wire pattern generator.
//!
//! One DMA transfer is a whole multiplex cycle. Each COM slot is a frame to
//! both drivers followed by zero padding that holds the latched output for
//! the rest of the slot. The cycle ends with an all-off frame so that the gap
//! between one transfer ending and the next starting is dark rather than
//! leaving the last COM pair lit for longer than the other three.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::Timer;
use esp_hal::Async;
use esp_hal::dma::DmaTxBuf;
use esp_hal::spi::master::{Address, Command, DataMode, SpiDma};
use log::{error, info};

use crate::aip33628::{FRAME_BYTES, encode_pair};
use crate::panel::{COM_SEQ, SLOTS, ScanImage};

/// SPI clock. One nibble, i.e. one state of the four wires, per period.
pub const SPI_HZ: u32 = 2_000_000;

/// Microseconds each COM pair is lit.
pub const SLOT_US: usize = 600;

/// Bytes per COM slot. Two nibbles per byte.
pub const SLOT_BYTES: usize = SLOT_US * (SPI_HZ as usize / 1_000_000) / 2;

/// Bytes per cycle: four lit slots plus the trailing blanking frame.
pub const CYCLE_BYTES: usize = SLOTS * SLOT_BYTES + FRAME_BYTES;

const _: () = assert!(SLOT_BYTES >= FRAME_BYTES);

/// Fill `out` with one complete scan cycle for `img`.
pub fn build_cycle(img: &ScanImage, out: &mut [u8]) {
    let out = &mut out[..CYCLE_BYTES];
    for (k, cs) in COM_SEQ.iter().enumerate() {
        let slot = &mut out[k * SLOT_BYTES..(k + 1) * SLOT_BYTES];
        let (frame, pad) = slot.split_at_mut(FRAME_BYTES);
        encode_pair(img.ss[0][k], img.ss[1][k], *cs, img.is_step, frame.try_into().unwrap());
        pad.fill(0);
    }
    // Blanking frame: every sink off, so the inter-transfer gap is dark.
    let blank = &mut out[SLOTS * SLOT_BYTES..];
    encode_pair(0, 0, COM_SEQ[SLOTS - 1], img.is_step, blank.try_into().unwrap());
}

/// The next image to scan. Written by whoever owns the panel state, picked
/// up by the scan task between cycles. Only the latest value matters.
static IMAGE: Signal<CriticalSectionRawMutex, ScanImage> = Signal::new();

/// Hand a new image to the scan engine. Takes effect within one cycle.
pub fn publish(img: ScanImage) {
    IMAGE.signal(img);
}

/// Runs the display forever. Owns the SPI peripheral and its DMA buffer.
#[embassy_executor::task]
pub async fn scan_task(spi: SpiDma<'static, Async>, buf: DmaTxBuf) -> ! {
    let mut spi = spi;
    let mut buf = buf;

    build_cycle(&ScanImage::DARK, buf.as_mut_slice());
    info!(
        "scan: {} bytes/cycle, {} us/slot, {} Hz refresh",
        CYCLE_BYTES,
        SLOT_US,
        (SPI_HZ as usize / 2) / CYCLE_BYTES
    );

    loop {
        let mut transfer = match spi.half_duplex_write_buffer(
            DataMode::Quad,
            Command::None,
            Address::None,
            0,
            CYCLE_BYTES,
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

        if let Some(img) = IMAGE.try_take() {
            build_cycle(&img, buf.as_mut_slice());
        }
    }
}

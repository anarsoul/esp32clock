// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The clip pack in flash: the `clips` partition, if this clock's
//! partition table has one, mapped into the address space so the
//! players read a clip in place as they read one built in, checked
//! (`core::pack`) and put in the melody table; and the upload of a new
//! one, `POST /clips`.
//!
//! A clock flashed before the partition existed has the old table,
//! which an update over the air leaves alone: there is then no
//! partition, no pack, and the clips are whatever the firmware has
//! built in (`ALARM_CLIP`), none by default. The same goes for a
//! partition that is empty or holds anything `Pack::parse` refuses.
//!
//! The mapping is the flash MMU's: 128 entries of 64 KB from
//! 0x3C00_0000, of which the bootloader filled the first few with the
//! application. The partition's pages go into the entries below the
//! last one (the bootloader's own), entry and then
//! `Cache_Invalidate_Addr`, as esp-storage maps a page for an
//! encrypted read; nothing is mapped if any of them is in use. With
//! `ring.rs` and `clip.rs` this is a module that touches registers,
//! through esp-hal's `regs()`.

use core::cell::Cell;

use critical_section::Mutex;
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::Read;
use esp_bootloader_esp_idf::partitions::{self, PARTITION_TABLE_MAX_LEN};
use esp_hal::peripherals::MMU_TABLE;
use log::{error, info, warn};

use esp32clock_core::pack::{HASH_LEN, HEADER_LEN, Pack, PackError, declared_len};

use crate::auth::{Refusal, refuse};
use crate::ota::{self, Buffers, SECTOR};
use crate::storage::with_flash;
use crate::{buzzer, clip, melody, scan};

/// The partition's label in the table.
const LABEL: &str = "clips";
/// The table's type byte for a data partition.
const TYPE_DATA: u8 = 1;
/// A page of the flash MMU, and where its first entry sits.
const PAGE: u32 = 0x1_0000;
const MAPPED_BASE: u32 = 0x3C00_0000;
/// Entries in the table; the last is left to the bootloader.
const ENTRIES: u32 = 128;
/// The largest partition that is mapped: twice what the table gives it.
const PAGES_MAX: u32 = 32;
/// How long an upload waits for a clip playing from the pack to stop.
const QUIET_WAIT: Duration = Duration::from_secs(3);

unsafe extern "C" {
    /// ROM: drop the cache's lines for `size` bytes at `addr`.
    fn Cache_Invalidate_Addr(addr: u32, size: u32);
}

/// The `clips` partition of this clock.
#[derive(Clone, Copy)]
struct Region {
    /// Where it is in the flash, and its size.
    offset: u32,
    size: u32,
    /// The address its first byte is mapped at, if the mapping was made.
    mapped: Option<usize>,
}

static REGION: Mutex<Cell<Option<Region>>> = Mutex::new(Cell::new(None));

fn region() -> Option<Region> {
    critical_section::with(|cs| REGION.borrow(cs).get())
}

/// The size of the `clips` partition: `None` on a clock without one.
pub fn room() -> Option<u32> {
    region().map(|r| r.size)
}

/// Find the partition, map it, and put the pack it holds in the melody
/// table. Call after `storage::init_flash` and before the settings are
/// loaded, which hold the melody to the table, and before any task runs.
pub fn init() {
    let mut table = [0u8; PARTITION_TABLE_MAX_LEN];
    let found = with_flash(|flash| {
        partitions::read_partition_table(flash, &mut table)
            .ok()
            .and_then(|t| t.iter().find(|p| p.raw_type() == TYPE_DATA && p.label_as_str() == LABEL).map(|p| (p.offset(), p.len())))
    });
    let Some((offset, size)) = found else {
        info!("clips: no `{LABEL}` partition in the table: {} clips built in", melody::CLIP_COUNT);
        return;
    };
    let mapped = map(offset, size);
    critical_section::with(|cs| REGION.borrow(cs).set(Some(Region { offset, size, mapped })));
    let Some(addr) = mapped else {
        error!("clips: partition at {offset:#x}, {size} bytes, could not be mapped: {} clips built in", melody::CLIP_COUNT);
        return;
    };
    let from = Instant::now();
    match load() {
        Ok(pack) => {
            info!(
                "clips: pack of {} clips, {} of {size} bytes at {offset:#x}, mapped at {addr:#x}, checked in {} ms",
                pack.count(),
                pack.len(),
                from.elapsed().as_millis()
            );
            melody::install(Some(pack));
        }
        Err(PackError::Magic) => info!("clips: partition at {offset:#x}, {size} bytes, holds no pack: {} clips built in", melody::CLIP_COUNT),
        Err(e) => warn!("clips: the pack at {offset:#x} is refused: {e:?} ({}): {} clips built in", e.text(), melody::CLIP_COUNT),
    }
}

/// Map the flash from `offset` for `size` bytes into the free entries
/// below the table's last, and answer where its first byte is then.
/// `None`, with nothing changed, if the range is empty or too long or
/// any of those entries is in use.
fn map(offset: u32, size: u32) -> Option<usize> {
    let first = offset / PAGE;
    let pages = offset.checked_add(size)?.div_ceil(PAGE) - first;
    if size == 0 || pages > PAGES_MAX || first + pages > u8::MAX as u32 {
        return None;
    }
    let start = ENTRIES - 1 - pages;
    let regs = MMU_TABLE::regs();
    critical_section::with(|_| {
        if (start..start + pages).any(|i| !regs.entry(i as usize).read().invalid().bit()) {
            return None;
        }
        for i in 0..pages {
            regs.entry((start + i) as usize).write(|w| {
                // Safety: a page number of the flash, under 256.
                unsafe { w.paddr().bits((first + i) as u8) };
                w.invalid().clear_bit()
            });
        }
        let base = MAPPED_BASE + start * PAGE;
        // Safety: a ROM routine, on a range just mapped that nothing reads yet.
        unsafe { Cache_Invalidate_Addr(base, pages * PAGE) };
        Some((base + offset % PAGE) as usize)
    })
}

/// The pack in the mapped partition, checked.
fn load() -> Result<Pack<'static>, PackError> {
    let (addr, size) = region().and_then(|r| r.mapped.map(|a| (a, r.size as usize))).ok_or(PackError::Short)?;
    // Safety: `map` put `size` bytes of flash at `addr`, for as long as
    // the firmware runs. They change only under `install`, which takes
    // the pack out of the melody table and waits out a clip played from
    // it before it writes, and puts none back without a check of what
    // is there then.
    let whole: &'static [u8] = unsafe { core::slice::from_raw_parts(addr as *const u8, size) };
    let total = declared_len(whole)?;
    Pack::parse(whole.get(..total).ok_or(PackError::Length)?)
}

/// What a finished upload reports back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Done {
    pub clips: usize,
    pub bytes: usize,
}

/// Write a pack of `total` bytes, the first `first.len()` of which have
/// been read already, to the `clips` partition. The pack in force
/// leaves the melody table before the first erase, and a clip playing
/// from it is stopped; the panel is dark for the write. What was
/// written is then read as the boot will read it: a pack it would
/// refuse is refused here, and its first sector erased. On `Ok` the
/// caller reboots, into a table with the new clips; after a failure
/// the clock goes on with the built-in clips.
pub async fn install(first: &[u8], total: usize, body: &mut impl Read) -> Result<Done, Refusal> {
    let region = region().ok_or(refuse("409 Conflict", "this clock's partition table has no clips partition"))?;
    let Some(addr) = region.mapped else {
        return Err(refuse("500 Internal Server Error", "the clips partition is not mapped"));
    };
    if total > region.size as usize {
        return Err(refuse("413 Payload Too Large", "pack larger than the clips partition"));
    }
    if total < HEADER_LEN + HASH_LEN {
        return Err(refuse("400 Bad Request", PackError::Short.text()));
    }
    let mut guard = ota::BUFFERS.try_lock().map_err(|_| refuse("503 Service Unavailable", "an upload is in progress"))?;
    let f = guard.as_mut().ok_or(refuse("500 Internal Server Error", "no upload buffers"))?;
    let Buffers { sector, check, .. } = &mut **f;

    // The header is checked before anything is erased.
    let mut first = first;
    let filled = ota::fill(&mut sector.0, &mut first, body, SECTOR.min(total)).await?;
    match declared_len(&sector.0[..filled]) {
        Ok(declared) if declared == total => {}
        Ok(_) => return Err(refuse("400 Bad Request", PackError::Length.text())),
        Err(e) => return Err(refuse("400 Bad Request", e.text())),
    }

    info!("clips: writing a pack of {total} bytes at {:#x}", region.offset);
    // Out of the table, so nothing starts a clip from the partition;
    // then whatever is playing from it must end.
    melody::install(None);
    buzzer::stop();
    let from = Instant::now();
    while clip::playing_in(addr, region.size as usize) {
        if from.elapsed() > QUIET_WAIT {
            reload();
            return Err(refuse("503 Service Unavailable", "a clip is playing and did not stop"));
        }
        Timer::after(Duration::from_millis(20)).await;
    }
    scan::stop().await;
    let written = match ota::write_hashed(sector, check, region.offset, filled, &mut first, total, body).await {
        Ok(written) => written,
        Err(e) => {
            warn!("clips: upload of {total} bytes failed: {}; no pack until the next upload", e.text);
            scan::resume();
            return Err(e);
        }
    };
    // The mapping still has the old pack's lines in the cache.
    // Safety: a ROM routine; nothing reads the range, the pack is out of the table.
    unsafe { Cache_Invalidate_Addr((addr as u32) & !(PAGE - 1), (region.offset % PAGE + region.size).div_ceil(PAGE) * PAGE) };
    match load() {
        Ok(pack) => {
            info!("clips: pack of {} clips written, {written} bytes", pack.count());
            Ok(Done { clips: pack.count(), bytes: written })
        }
        Err(e) => {
            warn!("clips: the pack written is refused: {e:?} ({}); erasing its header", e.text());
            if let Err(e) = with_flash(|flash| flash.erase(region.offset, region.offset + SECTOR as u32)) {
                error!("clips: erase failed: {e:?}");
            }
            scan::resume();
            Err(refuse("400 Bad Request", e.text()))
        }
    }
}

/// Put the pack in the partition back in the table, if it is one: for
/// an upload given up before it wrote anything.
fn reload() {
    if let Ok(pack) = load() {
        melody::install(Some(pack));
    }
}

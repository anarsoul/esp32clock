//! Firmware updates over the LAN: `POST /ota` with the application image
//! as the body writes the slot that is not running, switches `otadata`
//! to it and reboots. The upload must prove it may: either the
//! build-time token (`config::OTA_TOKEN`, for the script) or the PIN
//! the clock showed on its panel when the update page was opened, so
//! whoever uploads from the page can see the clock. Either is refused
//! before the flash is touched.
//!
//! The bootloader only falls back on its own when an image fails its
//! checksum, so an image that boots and then misbehaves is handled here:
//! the carry record counts the boots of an unconfirmed image and
//! `check_rollback` switches back once there have been too many.
//!
//! The flash is `storage::with_flash`, the one handle in the firmware,
//! taken for one blocking operation at a time: the partition table is
//! read and the slot chosen under it, each sector is erased, written and
//! read back under it, and `otadata` is switched under it; between those
//! the body is awaited from the socket with nothing held.

use core::cell::RefCell;
use core::sync::atomic::{AtomicU8, Ordering};

use critical_section::Mutex as CsMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Instant, Timer};
use embedded_io_async::Read;
use esp_bootloader_esp_idf::ota_updater::OtaUpdater;
use esp_bootloader_esp_idf::partitions::{self, AppPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionType};
use esp_storage::FlashStorage;
use log::{error, info, warn};
use sha2::{Digest, Sha256};
use static_cell::StaticCell;

use crate::config::{OTA_BOOT_LIMIT, OTA_TOKEN, OTA_TOKEN_MIN};
use crate::storage::with_flash;
use crate::{carry, clock, scan};

/// What this build calls itself: `git describe --always --dirty` from
/// `build.rs`, also in the image's app descriptor.
pub const VERSION: &str = env!("FIRMWARE_VERSION");

/// Flash sector: the erase unit and the write granule of the upload.
pub const SECTOR: usize = FlashStorage::SECTOR_SIZE as usize;

/// Image header magic and the app descriptor's, which sits at 0x20 in
/// every esp-idf style image (24-byte image header, 8-byte segment
/// header, then the descriptor as the first thing in the first segment).
const IMAGE_MAGIC: u8 = 0xE9;
const DESC_OFFSET: usize = 0x20;
const DESC_MAGIC: u32 = 0xABCD_5432;
/// The 32-byte version string, NUL padded, inside the descriptor.
const DESC_VERSION: core::ops::Range<usize> = DESC_OFFSET + 16..DESC_OFFSET + 48;
/// Image header byte that says a SHA-256 of the rest of the image is its
/// last 32 bytes. espflash always sets it; an image without it is
/// refused, since nothing else can tell a truncated file from a whole one.
const HASH_APPENDED: usize = 23;
const HASH_LEN: usize = 32;

/// An app slot, as the boot found it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Slot {
    Unknown = 0,
    Factory = 1,
    Ota0 = 2,
    Ota1 = 3,
}

impl Slot {
    pub fn name(self) -> &'static str {
        match self {
            Slot::Unknown => "?",
            Slot::Factory => "factory",
            Slot::Ota0 => "ota_0",
            Slot::Ota1 => "ota_1",
        }
    }

    fn from_raw(raw: u8) -> Slot {
        match raw {
            1 => Slot::Factory,
            2 => Slot::Ota0,
            3 => Slot::Ota1,
            _ => Slot::Unknown,
        }
    }

    fn from_subtype(sub: AppPartitionSubType) -> Slot {
        match sub {
            AppPartitionSubType::Factory => Slot::Factory,
            AppPartitionSubType::Ota0 => Slot::Ota0,
            AppPartitionSubType::Ota1 => Slot::Ota1,
            _ => Slot::Unknown,
        }
    }
}

/// The slot the running image was loaded from, and the one `otadata`
/// selects; they differ only after the bootloader fell back.
static BOOTED: AtomicU8 = AtomicU8::new(Slot::Unknown as u8);
static SELECTED: AtomicU8 = AtomicU8::new(Slot::Unknown as u8);

/// The slot the running image came from.
pub fn booted() -> Slot {
    Slot::from_raw(BOOTED.load(Ordering::Relaxed))
}

/// A sector of image data, word aligned so `esp-storage` reads straight
/// into it instead of through a buffer on the stack.
#[repr(C, align(4))]
struct Sector([u8; SECTOR]);

/// The buffers an upload needs, held by one listener at a time. `None`
/// until `init`, or when the table has no OTA slots.
struct Buffers {
    table: [u8; PARTITION_TABLE_MAX_LEN],
    sector: Sector,
    check: Sector,
}

/// The buffers live in a `StaticCell` so they are zero-initialised RAM
/// rather than 11 KB of image copied in at boot.
static BUFFERS_CELL: StaticCell<Buffers> = StaticCell::new();
static BUFFERS: Mutex<CriticalSectionRawMutex, Option<&'static mut Buffers>> = Mutex::new(None);

/// Whether this build accepts the script's token at all. The page's PIN
/// works regardless.
pub fn token_enabled() -> bool {
    OTA_TOKEN.len() >= OTA_TOKEN_MIN
}

/// A PIN is good for this long after it was issued.
pub const PIN_TTL_MS: u64 = 5 * 60 * 1000;

/// After a wrong PIN no new one is issued for this long.
pub const PIN_LOCKOUT_MS: u64 = 10 * 1000;

/// The PIN the update page must send: four digits the clock shows on
/// its panel when the page is opened. One attempt uses it up, right or
/// wrong, and a wrong one starts the lockout. Pure, with the clock
/// passed in as milliseconds, so the rules are checked on the host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PinState {
    /// The PIN and when it was issued.
    pending: Option<(u16, u64)>,
    /// No PIN is issued before this time.
    locked_until: Option<u64>,
}

/// Why a PIN was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinError {
    /// Nothing issued, or the last one was used up.
    None,
    Expired,
    Wrong,
}

impl PinState {
    /// Issue a PIN from a random number, or say how many milliseconds
    /// of lockout remain. A new PIN replaces an unused one.
    pub fn issue(&mut self, now_ms: u64, random: u32) -> Result<u16, u64> {
        if let Some(until) = self.locked_until {
            if now_ms < until {
                return Err(until - now_ms);
            }
            self.locked_until = None;
        }
        let pin = (random % 10_000) as u16;
        self.pending = Some((pin, now_ms));
        Ok(pin)
    }

    /// Check the presented digits against the pending PIN, using it up.
    pub fn check(&mut self, now_ms: u64, presented: &str) -> Result<(), PinError> {
        let Some((pin, issued)) = self.pending.take() else {
            return Err(PinError::None);
        };
        if now_ms.saturating_sub(issued) > PIN_TTL_MS {
            return Err(PinError::Expired);
        }
        let mut digits = [0u8; 4];
        digits[0] = b'0' + (pin / 1000) as u8;
        digits[1] = b'0' + (pin / 100 % 10) as u8;
        digits[2] = b'0' + (pin / 10 % 10) as u8;
        digits[3] = b'0' + (pin % 10) as u8;
        let p = presented.as_bytes();
        let mut diff = 0u8;
        for (i, &d) in digits.iter().enumerate() {
            diff |= d ^ p.get(i).copied().unwrap_or(0);
        }
        if p.len() == 4 && diff == 0 {
            Ok(())
        } else {
            self.locked_until = Some(now_ms + PIN_LOCKOUT_MS);
            Err(PinError::Wrong)
        }
    }
}

static PIN: CsMutex<RefCell<PinState>> = CsMutex::new(RefCell::new(PinState { pending: None, locked_until: None }));

/// A PIN for the display task to show, raised by `issue_pin`.
static SHOW_PIN: Signal<CriticalSectionRawMutex, u16> = Signal::new();

/// Issue a PIN for the update page: shown on the panel, logged (the
/// serial port is physical access too, and a headless board has no
/// panel), and remembered for `PIN_TTL_MS`. `Err` is the lockout left,
/// in seconds rounded up, after a wrong attempt.
pub fn issue_pin() -> Result<u16, u64> {
    let now_ms = Instant::now().as_millis();
    let random = esp_hal::rng::Rng::new().random();
    let result = critical_section::with(|cs| PIN.borrow_ref_mut(cs).issue(now_ms, random));
    match result {
        Ok(pin) => {
            info!("ota: PIN {pin:04} shown on the panel for the update page");
            SHOW_PIN.signal(pin);
            Ok(pin)
        }
        Err(left_ms) => {
            warn!("ota: update page opened during the lockout, {} s left", left_ms.div_ceil(1000));
            Err(left_ms.div_ceil(1000))
        }
    }
}

/// The next PIN to show on the panel.
pub async fn pin_to_show() -> u16 {
    SHOW_PIN.wait().await
}

/// Compare a presented token with the built-in one in time that depends
/// only on the lengths, not on where they first differ.
pub fn token_ok(presented: &[u8]) -> bool {
    let token = OTA_TOKEN.as_bytes();
    let mut diff = 0u8;
    for (i, &t) in token.iter().enumerate() {
        diff |= t ^ presented.get(i).copied().unwrap_or(0);
    }
    token_enabled() && presented.len() == token.len() && diff == 0
}

/// Read the partition table once, remember the booted and selected
/// slots, and roll back a new image that has not been confirmed in
/// `OTA_BOOT_LIMIT` boots. Call after `storage::init_flash` and
/// `carry::init` and before the scan starts: a rollback resets the chip
/// from here, with no transfer in flight.
pub fn init() {
    let mut table = [0u8; PARTITION_TABLE_MAX_LEN];
    let booted = match with_flash(|flash| partitions::read_partition_table(flash, &mut table).map(|t| t.booted_partition())) {
        Ok(Ok(Some(p))) => match p.partition_type() {
            PartitionType::App(sub) => Slot::from_subtype(sub),
            _ => Slot::Unknown,
        },
        Ok(_) => Slot::Unknown,
        Err(e) => {
            error!("ota: cannot read the partition table: {e:?}");
            Slot::Unknown
        }
    };
    BOOTED.store(booted as u8, Ordering::Relaxed);

    let (selected, ota_layout) = with_flash(|flash| match OtaUpdater::new(flash, &mut table) {
        Ok(mut updater) => (updater.selected_partition().map(Slot::from_subtype).unwrap_or(Slot::Unknown), true),
        Err(_) => (Slot::Unknown, false),
    });
    SELECTED.store(selected as u8, Ordering::Relaxed);
    info!(
        "ota: firmware {VERSION}, booted from {}, otadata selects {}, updates {}",
        booted.name(),
        selected.name(),
        if !ota_layout {
            "impossible (no OTA slots in the table)"
        } else if token_enabled() {
            "from the page with a PIN and from the script with the token"
        } else {
            "from the page with a PIN only (no OTA_TOKEN at build time)"
        }
    );

    let boots = carry::get().ota_boots;
    if boots > 0 && ota_layout {
        if boots >= OTA_BOOT_LIMIT {
            error!("ota: image {VERSION} not confirmed after {boots} boots, switching back to the previous slot");
            let result = with_flash(|flash| OtaUpdater::new(flash, &mut table).and_then(|mut u| u.activate_next_partition()));
            match result {
                Ok(()) => {
                    carry::update(|c| c.ota_boots = 0);
                    clock::carry_time();
                    esp_hal::delay::Delay::new().delay_millis(clock::RESET_PAUSE.as_millis() as u32);
                    esp_hal::system::software_reset()
                }
                Err(e) => error!("ota: rollback failed: {e:?}; staying on this image"),
            }
        } else {
            carry::update(|c| c.ota_boots = boots + 1);
            warn!("ota: image {VERSION} boot {boots} of {}, rolled back if not confirmed", OTA_BOOT_LIMIT - 1);
        }
    }

    if ota_layout {
        let mut slot = BUFFERS.try_lock().expect("ota buffers lock at init");
        *slot = Some(BUFFERS_CELL.init_with(|| Buffers { table, sector: Sector([0; SECTOR]), check: Sector([0; SECTOR]) }));
    }
}

/// The running image has done something useful (a gateway probe
/// answered, an HTTP request served): it will not be rolled back.
pub fn confirm() {
    if carry::get().ota_boots != 0 {
        carry::update(|c| c.ota_boots = 0);
        info!("ota: image {VERSION} confirmed good");
    }
}

/// Why an upload was refused, with the HTTP status it earns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Refusal {
    pub status: &'static str,
    pub text: &'static str,
}

const fn refuse(status: &'static str, text: &'static str) -> Refusal {
    Refusal { status, text }
}

/// Check the `Authorization` header, if any: `Bearer <token>` for the
/// script, `PIN <digits>` for the page. Checks the token with a closure
/// and the PIN with the state, both pure, so the rules run on the host.
pub fn authorize(header: Option<&str>) -> Result<(), Refusal> {
    let now_ms = Instant::now().as_millis();
    authorize_with(header, |digits| critical_section::with(|cs| PIN.borrow_ref_mut(cs).check(now_ms, digits)))
}

pub fn authorize_with(header: Option<&str>, check_pin: impl FnOnce(&str) -> Result<(), PinError>) -> Result<(), Refusal> {
    let Some(header) = header else {
        return Err(refuse("403 Forbidden", "no credentials: a PIN from the update page or the token"));
    };
    if let Some(token) = header.strip_prefix("Bearer ") {
        if !token_enabled() {
            return Err(refuse("403 Forbidden", "the script path is disabled: no token built in"));
        }
        return if token_ok(token.trim().as_bytes()) { Ok(()) } else { Err(refuse("403 Forbidden", "bad token")) };
    }
    if let Some(digits) = header.strip_prefix("PIN ") {
        return match check_pin(digits.trim()) {
            Ok(()) => Ok(()),
            Err(PinError::None) => Err(refuse("403 Forbidden", "no PIN issued: open the update page first")),
            Err(PinError::Expired) => Err(refuse("403 Forbidden", "PIN expired: reload the update page")),
            Err(PinError::Wrong) => Err(refuse("403 Forbidden", "wrong PIN")),
        };
    }
    Err(refuse("403 Forbidden", "bad credentials"))
}

/// What a finished upload reports back.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Done {
    pub slot: Slot,
    pub bytes: usize,
    pub version: heapless::String<32>,
}

/// Write an image of `total` bytes, the first `first.len()` of which
/// have been read already, into the slot that is not running, and
/// select it for the next boot. The panel is dark from the first erase
/// on; a failure brings it back and leaves `otadata` as it was, so the
/// running image stays the boot image. The caller reboots on `Ok`
/// through `reboot`.
pub async fn install(first: &[u8], total: usize, body: &mut impl Read) -> Result<Done, Refusal> {
    if total < SECTOR {
        // Also keeps `total - HASH_LEN` from underflowing below.
        return Err(refuse("400 Bad Request", "body too short for an image"));
    }
    let mut guard = BUFFERS.try_lock().map_err(|_| refuse("503 Service Unavailable", "an upload is in progress"))?;
    let f = guard.as_mut().ok_or(refuse("500 Internal Server Error", "no OTA slots in the partition table"))?;
    let Buffers { table, sector, check } = &mut **f;
    // Where the image goes: the slot `otadata` would switch to, as an
    // absolute flash range, so that the sector writes need no updater.
    let (slot, base, room) = with_flash(|flash| -> Result<_, Refusal> {
        let mut updater = OtaUpdater::new(flash, table)
            .map_err(|_| refuse("500 Internal Server Error", "partition table unreadable"))?;
        let (next, room) = {
            let (region, next) =
                updater.next_partition().map_err(|_| refuse("500 Internal Server Error", "no slot to write"))?;
            (next, region.partition_size())
        };
        let entry = partitions::read_partition_table(flash, table)
            .ok()
            .and_then(|t| t.find_partition(PartitionType::App(next)).ok().flatten())
            .ok_or(refuse("500 Internal Server Error", "slot missing from the table"))?;
        Ok((Slot::from_subtype(next), entry.offset(), room))
    })?;
    if total > room {
        return Err(refuse("413 Payload Too Large", "image larger than a slot"));
    }

    // The first sector is checked before anything is erased.
    let mut first = first;
    let mut filled = fill(&mut sector.0, &mut first, body, SECTOR).await?;
    if sector.0[0] != IMAGE_MAGIC || u32::from_le_bytes(sector.0[DESC_OFFSET..DESC_OFFSET + 4].try_into().unwrap()) != DESC_MAGIC {
        return Err(refuse("400 Bad Request", "not an application image"));
    }
    if sector.0[HASH_APPENDED] != 1 {
        return Err(refuse("400 Bad Request", "image has no appended hash"));
    }
    let version = version_from(&sector.0[DESC_VERSION]);
    info!("ota: writing image {version} of {total} bytes to {}", slot.name());

    scan::stop().await;
    let mut written = 0usize;
    let mut hasher = Sha256::new();
    let mut tail = [0u8; HASH_LEN];
    let result = async {
        loop {
            // Everything before the last 32 bytes is hashed; those 32 are
            // the hash to compare with, wherever they fall in the sectors.
            hash_chunk(&mut hasher, &mut tail, &sector.0[..filled], written, total);
            // Pad a short last sector to the write granule with erased bytes.
            let len = filled.div_ceil(4) * 4;
            sector.0[filled..len].fill(0xFF);
            let at = base + written as u32;
            with_flash(|flash| {
                flash
                    .erase(at, at + SECTOR as u32)
                    .and_then(|()| flash.write_nor(at, &sector.0[..len]))
                    .and_then(|()| flash.read_nor(at, &mut check.0[..len]))
            })
            .map_err(|e| {
                error!("ota: flash error at {at:#x}: {e:?}");
                refuse("500 Internal Server Error", "flash write failed")
            })?;
            if check.0[..len] != sector.0[..len] {
                error!("ota: read-back mismatch at {at:#x}");
                return Err(refuse("500 Internal Server Error", "flash read-back mismatch"));
            }
            written += filled;
            if written >= total {
                break;
            }
            filled = fill(&mut sector.0, &mut first, body, SECTOR.min(total - written)).await?;
        }
        Ok(())
    }
    .await;
    let result = result.and_then(|()| {
        if hasher.finalize()[..] == tail[..] {
            Ok(())
        } else {
            Err(refuse("400 Bad Request", "image hash mismatch: truncated or corrupt"))
        }
    });
    if let Err(e) = result {
        warn!("ota: upload failed after {written} of {total} bytes: {}; the running image stays", e.text);
        scan::resume();
        return Err(e);
    }
    let activated = with_flash(|flash| OtaUpdater::new(flash, table).and_then(|mut u| u.activate_next_partition()));
    if let Err(e) = activated {
        error!("ota: cannot select {}: {e:?}", slot.name());
        scan::resume();
        return Err(refuse("500 Internal Server Error", "cannot select the new slot"));
    }
    info!("ota: image {version} written to {}, {written} bytes, selected for the next boot", slot.name());
    Ok(Done { slot, bytes: written, version })
}

/// Fill `buf[..want]` from what is left of `first`, then the body. The
/// number of bytes read is `want` unless the body ends early.
async fn fill(buf: &mut [u8], first: &mut &[u8], body: &mut impl Read, want: usize) -> Result<usize, Refusal> {
    let head = first.len().min(want);
    buf[..head].copy_from_slice(&first[..head]);
    *first = &first[head..];
    let mut n = head;
    while n < want {
        match body.read(&mut buf[n..want]).await {
            Ok(0) => return Err(refuse("400 Bad Request", "body ended short")),
            Ok(got) => n += got,
            Err(_) => return Err(refuse("400 Bad Request", "body read failed")),
        }
    }
    Ok(n)
}

/// Feed the chunk at `at` of an image `total` bytes long to the hasher,
/// except for the last `HASH_LEN` bytes of the image, which go into
/// `tail` instead.
fn hash_chunk(hasher: &mut Sha256, tail: &mut [u8; HASH_LEN], chunk: &[u8], at: usize, total: usize) {
    let hashed_end = total - HASH_LEN;
    let split = hashed_end.saturating_sub(at).min(chunk.len());
    hasher.update(&chunk[..split]);
    for (i, &b) in chunk[split..].iter().enumerate() {
        tail[at + split + i - hashed_end] = b;
    }
}

/// The descriptor's version field as text: up to the first NUL, printable
/// ASCII only.
fn version_from(field: &[u8]) -> heapless::String<32> {
    let mut s = heapless::String::new();
    for &b in field.iter().take_while(|&&b| b != 0) {
        let c = if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' };
        if s.push(c).is_err() {
            break;
        }
    }
    s
}

/// Reboot into the new image: the time goes across as for a self-heal
/// and the carry record notes one boot of an unconfirmed image. The scan
/// is already stopped, so no transfer is in flight.
pub async fn reboot() -> ! {
    carry::update(|c| c.ota_boots = 1);
    clock::carry_time();
    info!("ota: rebooting into the new image");
    Timer::after(clock::RESET_PAUSE).await;
    esp_hal::system::software_reset()
}

//! Settings in flash: the `nvs` partition, a `sequential-storage` map over
//! `esp-storage`, and a task that writes a changed value once the user has
//! stopped pressing buttons.
//!
//! Every flash operation runs with interrupts off and the cache disabled,
//! so the scan task cannot restart a DMA transfer meanwhile. A record
//! write is well under a millisecond and invisible; a sector erase is tens
//! of milliseconds, a brief dark blink, and happens once per few hundred
//! changes.

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};
use embedded_storage::nor_flash::{NorFlash as BlockingNorFlash, ReadNorFlash as BlockingReadNorFlash};
use embedded_storage_async::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use esp_bootloader_esp_idf::partitions::{self, DataPartitionSubType, PartitionType};
use esp_storage::{FlashStorage, FlashStorageError};
use log::{error, info, warn};
use sequential_storage::cache::{Cache, Uncached};
use sequential_storage::map::{MapConfig, MapStorage};

use crate::settings::{self, RECORD_LEN, Settings};

/// The one map key.
const KEY: u8 = 0;

/// Quiet time after the last change before it is written.
const SETTLE: Duration = Duration::from_secs(2);

/// `esp-storage`'s blocking flash behind the async traits
/// `sequential-storage` is written against. Each call completes before it
/// returns; nothing is awaited.
struct AsyncFlash(FlashStorage<'static>);

impl ErrorType for AsyncFlash {
    type Error = FlashStorageError;
}

impl ReadNorFlash for AsyncFlash {
    const READ_SIZE: usize = <FlashStorage<'static> as BlockingReadNorFlash>::READ_SIZE;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        BlockingReadNorFlash::read(&mut self.0, offset, bytes)
    }

    fn capacity(&self) -> usize {
        BlockingReadNorFlash::capacity(&self.0)
    }
}

impl NorFlash for AsyncFlash {
    const WRITE_SIZE: usize = <FlashStorage<'static> as BlockingNorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <FlashStorage<'static> as BlockingNorFlash>::ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        BlockingNorFlash::erase(&mut self.0, from, to)
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        BlockingNorFlash::write(&mut self.0, offset, bytes)
    }
}

impl MultiwriteNorFlash for AsyncFlash {}

/// No cache: the map is read once at boot and written a few times a day.
type NoCache = Cache<Uncached, Uncached, Uncached, u8>;

type Map = MapStorage<u8, AsyncFlash, NoCache>;

type MapError = sequential_storage::Error<FlashStorageError>;

/// Scratch for the map: key, record, item header, rounded up; a stale
/// record of another length fits too. Word aligned
/// so `esp-storage` reads into it directly instead of through a sector
/// buffer on the stack.
#[repr(C, align(4))]
struct Scratch([u8; 128]);

/// The settings map in the `nvs` partition.
pub struct Store {
    map: Map,
    scratch: Scratch,
}

impl Store {
    /// Read the partition table and open the map over the `nvs` partition.
    /// `None`, with the reason logged, when there is no usable partition;
    /// settings then live in RAM only.
    pub fn open(mut flash: FlashStorage<'static>) -> Option<Store> {
        let mut table_buf = [0u8; partitions::PARTITION_TABLE_MAX_LEN];
        let table = match partitions::read_partition_table(&mut flash, &mut table_buf) {
            Ok(t) => t,
            Err(e) => {
                error!("settings: cannot read the partition table: {e:?}");
                return None;
            }
        };
        let entry = match table.find_partition(PartitionType::Data(DataPartitionSubType::Nvs)) {
            Ok(Some(e)) => e,
            Ok(None) => {
                error!("settings: no nvs partition, settings will not persist");
                return None;
            }
            Err(e) => {
                error!("settings: partition table: {e:?}");
                return None;
            }
        };
        let range = entry.offset()..entry.offset() + entry.len();
        info!("settings: nvs partition at {:#x}, {} bytes", range.start, entry.len());
        let config = match MapConfig::<AsyncFlash>::try_new(range) {
            Ok(c) => c,
            Err(e) => {
                error!("settings: nvs partition unusable: {e:?}");
                return None;
            }
        };
        Some(Store { map: Map::new(AsyncFlash(flash), config, NoCache::new_uncached()), scratch: Scratch([0; 128]) })
    }

    /// The stored settings, if there is a valid record. A map that cannot
    /// be read (first boot over a partition holding something else, or a
    /// torn write the repair could not fix) is erased so that saving works.
    pub async fn load(&mut self) -> Option<Settings> {
        match self.map.fetch_item::<&[u8]>(&mut self.scratch.0, &KEY).await {
            Ok(Some(record)) => match Settings::from_record(record) {
                Some(s) => {
                    info!("settings: loaded from flash");
                    Some(s)
                }
                None => {
                    warn!("settings: stored record {record:?} not understood, using defaults");
                    None
                }
            },
            Ok(None) => {
                info!("settings: nothing stored, using defaults");
                None
            }
            Err(e) => {
                warn!("settings: cannot read the map ({e:?}), erasing it");
                if let Err(e) = self.map.erase_all().await {
                    error!("settings: erase failed: {e:?}");
                }
                None
            }
        }
    }

    /// Write the settings.
    pub async fn save(&mut self, s: &Settings) -> Result<(), MapError> {
        let record: [u8; RECORD_LEN] = s.to_record();
        self.map.store_item(&mut self.scratch.0, &KEY, &record.as_slice()).await
    }
}

/// Write the settings to flash whenever they change, once they have been
/// stable for [`SETTLE`]. `stored` is what the flash holds at start.
#[embassy_executor::task]
pub async fn persist_task(mut store: Store, mut stored: Option<Settings>) -> ! {
    let mut rx = settings::receiver();
    // The boot value is already in flash (or is the default); do not
    // rewrite it.
    let _ = rx.try_changed();
    loop {
        let mut s = rx.changed().await;
        loop {
            match select(Timer::after(SETTLE), rx.changed()).await {
                Either::First(()) => break,
                Either::Second(next) => s = next,
            }
        }
        // Compare what would be written, so a change to a field the
        // record leaves out (`skip_next`) does not touch flash.
        if stored.is_some_and(|old| old.to_record() == s.to_record()) {
            continue;
        }
        match store.save(&s).await {
            Ok(()) => {
                info!("settings: saved");
                stored = Some(s);
            }
            Err(e) => warn!("settings: save failed: {e:?}"),
        }
    }
}

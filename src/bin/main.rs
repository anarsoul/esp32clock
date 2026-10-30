#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

extern crate alloc;

use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_time::{Duration, Instant, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::dma_tx_buffer;
use esp_hal::gpio::{DriveMode, Flex, Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::interrupt::Priority;
use esp_hal::ledc::Ledc;
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::{ControllerConfig, Interface, WifiController};
use esp_rtos::embassy::InterruptExecutor;
use esp_storage::FlashStorage;
use log::info;
use static_cell::StaticCell;

use esp32clock::buzzer;
use esp32clock::settings::Settings;
use esp32clock::net::Mode;
use esp32clock::{buttons, carry, clock, dhcpd, display, dnsd, heal, http, net, ota, scan, settings, storage};

// The app descriptor the esp-idf bootloader requires, with the git
// version in it so an image file says what it is before it is flashed.
esp_bootloader_esp_idf::esp_app_desc!(
    ota::VERSION,
    env!("CARGO_PKG_NAME"),
    esp_bootloader_esp_idf::BUILD_TIME,
    esp_bootloader_esp_idf::BUILD_DATE,
    esp_bootloader_esp_idf::ESP_IDF_COMPATIBLE_VERSION,
    esp_bootloader_esp_idf::MMU_PAGE_SIZE,
    0,
    u16::MAX,
    esp_bootloader_esp_idf::SECURE_VERSION
);

/// Called by esp-backtrace after it has printed the panic message and the
/// backtrace. A clock should not sit dark waiting for a power cycle, so
/// give the serial output a moment to drain and reboot. The DMA scan has
/// already stopped by then; the panel is dark until the firmware is back,
/// a couple of seconds including the NTP sync.
#[unsafe(no_mangle)]
pub extern "Rust" fn custom_halt() -> ! {
    esp_hal::delay::Delay::new().delay_millis(500);
    esp_hal::system::software_reset()
}

/// Stamp every log line with the uptime in milliseconds (esp-println's
/// `timestamp` feature), so a capture is a timeline on its own. Read
/// straight from the system timer, which runs from before `esp_hal::init`.
#[unsafe(no_mangle)]
pub extern "Rust" fn _esp_println_timestamp() -> u64 {
    esp_hal::time::Instant::now().duration_since_epoch().as_millis()
}

/// A display wire with its idle pull-down set. Every frame ends with all
/// four wires low, so a weak pull-down keeps the drivers' inputs at a
/// defined level and no spurious edge can reach them between DMA
/// transfers. The SPI builder resets the pull of a bare pin it takes, but a
/// `Flex` reaches it as a frozen signal whose configuration it leaves
/// alone, so the pull set here survives. The input buffer is enabled as
/// the builder would have done on a bare pin.
fn display_wire(pin: impl esp_hal::gpio::Pin + 'static) -> Flex<'static> {
    let mut wire = Flex::new(pin);
    wire.apply_output_config(&OutputConfig::default().with_pull(Pull::Down));
    wire.set_input_enable(true);
    wire
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Heap for the Wi-Fi driver, sized as esp-radio recommends.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    esp_alloc::heap_allocator!(size: 36 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // Buzzer first. The pin feeds the base of a PNP whose emitter sits at
    // +5 V, so a push-pull 3.3 V high still turns it on. Open drain and
    // released (high impedance) is off, low is on.
    let mut buzzer_pin = peripherals.GPIO5;
    {
        // Set the output register high and the pad open-drain now, so the
        // pin is released however the pad was left. The buzzer task takes
        // the pin over from here.
        let _ = Output::new(
            buzzer_pin.reborrow(),
            Level::High,
            OutputConfig::default().with_drive_mode(DriveMode::OpenDrain),
        );
    }

    // Buttons switch to ground and the board has no pull-ups. GPIO4 is the
    // upper button on this board, GPIO3 the lower one.
    let button_top = Input::new(peripherals.GPIO4, InputConfig::default().with_pull(Pull::Up));
    let button_bottom = Input::new(peripherals.GPIO3, InputConfig::default().with_pull(Pull::Up));

    // Display wires, SPI2 SIO0 to SIO3. The scan task builds the bus from
    // them; see scan.rs and aip33628.rs.
    let wires = [
        display_wire(peripherals.GPIO9), // DATA,   driver 1
        display_wire(peripherals.GPIO8), // CLK,    driver 1
        display_wire(peripherals.GPIO7), // DATA_1, driver 2
        display_wire(peripherals.GPIO6), // CLK_1,  driver 2
    ];

    // Two transfer buffers: one in flight, one being filled.
    let buf = dma_tx_buffer!(scan::TRANSFER_BYTES).expect("dma buffer");
    let spare = dma_tx_buffer!(scan::TRANSFER_BYTES).expect("dma spare buffer");
    scan::init_spare(spare);

    // Saved settings, so the first frame already has the user's colour.
    // Without a usable nvs partition they live in RAM for this boot.
    storage::init_flash(FlashStorage::new(peripherals.FLASH));
    let mut store = storage::Store::open();
    let mut stored = None;
    if let Some(store) = store.as_mut() {
        stored = store.load().await;
    }
    // The build's network and token fill in what the record lacks; the
    // first save carries them from then on.
    let mut boot_settings = stored.unwrap_or(Settings::DEFAULT);
    let (stored_wifi, stored_token) = (!boot_settings.wifi.is_none(), !boot_settings.token.0.is_empty());
    boot_settings.seed();
    settings::init(boot_settings);
    info!(
        "boot: network {}, api token {}",
        match (stored_wifi, boot_settings.wifi.is_none()) {
            (true, _) => "stored",
            (false, false) => "built in",
            (false, true) => "none",
        },
        match (stored_token, boot_settings.token.0.is_empty(), boot_settings.token_cleared) {
            (true, _, _) => "stored",
            (false, false, _) => "built in",
            (false, true, true) => "cleared",
            (false, true, false) => "none",
        }
    );
    let (reason, kept) = carry::init();
    if kept {
        info!("boot: reset reason {reason:?}, software reset: carried over {:?}", carry::get());
    } else {
        info!("boot: reset reason {reason:?}, RTC carry-over discarded");
    }
    // Slot bookkeeping over the same flash handle. This may reset the chip
    // to roll back an unconfirmed image; nothing is running yet.
    ota::init();
    // With a carried time the clock is known before Wi-Fi is even started.
    if clock::restore_carried()
        && let Some(t) = clock::now()
    {
        info!("clock: time carried over the reset, local time {:02}:{:02}:{:02}", t.hour, t.minute, t.second);
    }
    info!("settings: {:?}", settings::get());

    // The drivers discard anything sent in the first 200 us after power-up.
    Timer::after(Duration::from_millis(10)).await;

    // Station or access point, for this whole boot: the access point when
    // there is no network to join or the button hold asked for it. The
    // driver does not do both at once.
    let mode = if carry::get().boot_ap {
        info!("boot: access point mode, asked for by the button hold or POST /ap");
        Mode::AccessPoint
    } else if settings::get().wifi.is_none() {
        info!("boot: access point mode, no network to join");
        Mode::AccessPoint
    } else {
        Mode::Station
    };
    net::set_mode(mode);

    // The Wi-Fi driver and the network stack on top of it. Built before
    // the display task, which asks the stack for the IP to show.
    let wifi_config = ControllerConfig::default().with_rx_queue_size(esp32clock::config::WIFI_RX_QUEUE_FRAMES);
    let controller = WifiController::new(peripherals.WIFI, wifi_config).expect("wifi init");
    let interface = if mode == Mode::AccessPoint { Interface::access_point() } else { Interface::station() };
    let mac = interface.mac_address();
    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    // Station: DHCP, DNS, SNTP, the gateway probe, the HTTP listeners,
    // and headroom; no IPv4 configuration yet, the Wi-Fi task starts
    // DHCP once the station is connected and the link has settled.
    // Access point: the fixed address, the DHCP and DNS servers and the
    // listeners.
    static RESOURCES: StaticCell<StackResources<10>> = StaticCell::new();
    let stack_config = if mode == Mode::AccessPoint { net::ap_stack_config() } else { embassy_net::Config::default() };
    let (stack, runner) = embassy_net::new(interface, stack_config, RESOURCES.init(StackResources::new()), seed);

    // The scan task restarts the DMA transfer the moment one ends. In the
    // thread-mode executor a Wi-Fi task can hold it off for hundreds of
    // microseconds, which is a dark blink on the panel. An interrupt-mode
    // executor cannot be preempted by threads, so the gap stays at a few
    // microseconds whatever Wi-Fi is doing.
    static SCAN_EXECUTOR: StaticCell<InterruptExecutor<1>> = StaticCell::new();
    let scan_executor = SCAN_EXECUTOR.init(InterruptExecutor::new(peripherals.FROM_CPU_INTR1));
    let scan_spawner = scan_executor.start(Priority::Priority2);
    let hw = scan::ScanHardware { spi: peripherals.SPI2, dma: peripherals.DMA_CH0, wires, buf };
    scan_spawner.spawn(scan::scan_task(hw).expect("spawn scan"));
    spawner.spawn(display::display_task(stack).expect("spawn display"));
    spawner.spawn(buttons::button_task(button_top, button_bottom).expect("spawn buttons"));
    spawner.spawn(buzzer::buzzer_task(Ledc::new(peripherals.LEDC), buzzer_pin).expect("spawn buzzer"));
    if let Some(store) = store.take() {
        spawner.spawn(storage::persist_task(store, stored).expect("spawn persist"));
    }

    spawner.spawn(net::net_task(runner).expect("spawn net"));
    if mode == Mode::AccessPoint {
        spawner.spawn(net::ap_task(controller, net::ap_ssid(mac)).expect("spawn ap"));
        spawner.spawn(dhcpd::dhcpd_task(stack).expect("spawn dhcpd"));
        spawner.spawn(dnsd::dnsd_task(stack).expect("spawn dnsd"));
    } else {
        spawner.spawn(net::wifi_task(controller, stack).expect("spawn wifi"));
        spawner.spawn(net::ntp_task(stack).expect("spawn ntp"));
        spawner.spawn(net::hostname_task(stack).expect("spawn hostname"));
        spawner.spawn(net::monitor_task(stack).expect("spawn monitor"));
        spawner.spawn(net::probe_task(stack).expect("spawn probe"));
    }
    for i in 0..http::LISTENERS {
        spawner.spawn(http::http_task(stack, i).expect("spawn http"));
    }

    info!("running");
    loop {
        Timer::after(Duration::from_secs(60)).await;
        let g = scan::take_gap_stats();
        let (sent, failed, streak) = net::probe_stats();
        info!(
            "alive {} s, heap free {} bytes; scan restart gap avg {} us, max {} us over {} transfers; \
             gateway probes {} sent, {} failed, {} failing now; self-heal resets {}",
            Instant::now().as_secs(),
            esp_alloc::HEAP.free(),
            g.sum_us / g.transfers.max(1) as u64,
            g.max_us,
            g.transfers,
            sent,
            failed,
            streak,
            heal::count()
        );
        http::log_status();
    }
}

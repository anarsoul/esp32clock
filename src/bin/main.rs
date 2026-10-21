#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::dma_tx_buffer;
use esp_hal::gpio::{DriveMode, Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use log::info;

use esp32clock::panel::{Color, Panel};
use esp32clock::scan;

// This creates a default app-descriptor required by the esp-idf bootloader.
esp_bootloader_esp_idf::esp_app_desc!();

/// Display wires, as GPIO numbers, for the idle pull-downs.
const DISPLAY_GPIOS: [usize; 4] = [6, 7, 8, 9];

/// Pull the four display wires low while the SPI peripheral is not driving
/// them. The SPI driver resets the pull configuration when it takes a pin,
/// so this has to run after the bus is built. Every frame ends with all four
/// wires low, so a weak pull-down keeps the drivers' inputs at a defined
/// level and no spurious edge can reach them between DMA transfers.
fn pull_down_display_wires() {
    let io_mux = unsafe { &*esp32c3::IO_MUX::PTR };
    for gpio in DISPLAY_GPIOS {
        io_mux.gpio(gpio).modify(|_, w| w.fun_wpu().clear_bit().fun_wpd().set_bit());
    }
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

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // Buzzer first. The pin feeds the base of a PNP whose emitter sits at
    // +5 V, so a push-pull 3.3 V high still turns it on. Open drain and
    // released (high impedance) is off, low is on.
    let _buzzer = Output::new(
        peripherals.GPIO5,
        Level::High,
        OutputConfig::default().with_drive_mode(DriveMode::OpenDrain),
    );

    // Buttons switch to ground and the board has no pull-ups.
    let _button1 = Input::new(peripherals.GPIO3, InputConfig::default().with_pull(Pull::Up));
    let _button2 = Input::new(peripherals.GPIO4, InputConfig::default().with_pull(Pull::Up));

    // Display: SPI2 in quad mode, no SCK, the four data lines are the four
    // panel wires. See scan.rs and aip33628.rs.
    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_hz(scan::SPI_HZ))
            .with_mode(Mode::_0),
    )
    .expect("spi config")
    .with_sio0(peripherals.GPIO9) // DATA,   driver 1
    .with_sio1(peripherals.GPIO8) // CLK,    driver 1
    .with_sio2(peripherals.GPIO7) // DATA_1, driver 2
    .with_sio3(peripherals.GPIO6) // CLK_1,  driver 2
    .with_dma(peripherals.DMA_CH0)
    .into_async();
    pull_down_display_wires();

    let buf = dma_tx_buffer!(scan::CYCLE_BYTES).expect("dma buffer");

    // The drivers discard anything sent in the first 200 us after power-up.
    Timer::after(Duration::from_millis(10)).await;
    spawner.spawn(scan::scan_task(spi, buf).expect("spawn scan"));

    let mut panel = Panel::new();
    panel.set_color(Color::RED);
    panel.set_current(5); // 15.2 mA per lit sink, conservative first light
    panel.set_time(12, 34);
    panel.set_colon(true);
    scan::publish(panel.render());
    info!("showing 12:34");

    loop {
        Timer::after(Duration::from_secs(10)).await;
        info!("alive");
    }
}

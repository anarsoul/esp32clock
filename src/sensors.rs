// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The room sensors on the adapter board: the NTC divider on GPIO0 and
//! the LDR divider on GPIO1, both ADC1 channels, which is the converter
//! that works with Wi-Fi on. One task owns the converter and both pins
//! (`read_oneshot` borrows the converter, and the pins are typed to it)
//! and publishes the filtered millivolts; the conversions to a
//! temperature and a light index are the pure half in the core crate.
//!
//! Every second the task takes `SAMPLES` conversions of each channel a
//! few tens of microseconds apart, averages them, and folds the result
//! into an exponential average with a weight of `1 / FILTER_DIV`, about
//! ten seconds. The first reading seeds the average so nothing ramps up
//! from zero. The cells have this task as their single writer; the page
//! and the heartbeat only read them.
//!
//! The calibration scheme is `AdcCalCurve`: on the C3 it corrects the
//! raw code with the eFuse two-point data and a polynomial, and hands
//! back millivolts. It is the scheme that reads straight on this chip
//! (the plain line fit is a few percent off).

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_time::{Duration, Timer};
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcConfig, Attenuation};
use esp_hal::peripherals::{ADC1, GPIO0, GPIO1};
use log::info;

use crate::settings;

pub use esp32clock_core::sensors::{Tenths, fahrenheit_tenths, light_pct, temperature_dc};

/// Conversions averaged into one reading.
const SAMPLES: u32 = 8;
/// The exponential average's divisor: each reading moves it by this
/// fraction of the difference, so it settles in about `FILTER_DIV` ×
/// `PERIOD`.
const FILTER_DIV: u32 = 8;
/// Fixed-point bits the running average keeps below a millivolt.
const FRACTION_BITS: u32 = 8;
/// Between readings.
const PERIOD: Duration = Duration::from_secs(1);
/// Between the conversions of one reading, so the one-shot start is
/// seen by the converter (esp-hal notes the controller can miss it when
/// the next sample follows at once).
const SAMPLE_GAP: Duration = Duration::from_micros(50);

/// No reading yet.
const EMPTY: u32 = u32::MAX;

/// The filtered millivolts of each divider, `EMPTY` until the first
/// reading.
static NTC_MV: AtomicU32 = AtomicU32::new(EMPTY);
static LDR_MV: AtomicU32 = AtomicU32::new(EMPTY);

/// The filtered millivolts of both dividers, NTC then LDR, `None`
/// until the first reading.
pub fn readings() -> Option<(u16, u16)> {
    let (ntc, ldr) = (NTC_MV.load(Ordering::Relaxed), LDR_MV.load(Ordering::Relaxed));
    (ntc != EMPTY && ldr != EMPTY).then_some((ntc as u16, ldr as u16))
}

/// The temperature in tenths of a degree Celsius, with the settings'
/// thermometer offset added, and the light index, as the page and the
/// API show them: `None` before the first reading or, for the
/// temperature, with no NTC fitted.
pub fn room() -> (Option<i16>, Option<u8>) {
    match readings() {
        Some((ntc, ldr)) => {
            let offset = settings::get().temp_offset_dc;
            (temperature_dc(ntc).map(|dc| dc.saturating_add(offset)), Some(light_pct(ldr)))
        }
        None => (None, None),
    }
}

/// The converter and the two pins, handed over from `main`.
pub struct SensorHardware {
    pub adc: ADC1<'static>,
    pub ntc: GPIO0<'static>,
    pub ldr: GPIO1<'static>,
}

/// One running average in fixed point, seeded by its first reading.
struct Filter(Option<u32>);

impl Filter {
    fn push(&mut self, mv: u32) -> u16 {
        let scaled = mv << FRACTION_BITS;
        let value = match self.0 {
            None => scaled,
            // The difference may be negative; the sum never is.
            Some(avg) => (avg as i64 + (scaled as i64 - avg as i64) / FILTER_DIV as i64) as u32,
        };
        self.0 = Some(value);
        (value >> FRACTION_BITS) as u16
    }
}

#[embassy_executor::task]
pub async fn sensor_task(hw: SensorHardware) {
    let mut config = AdcConfig::new();
    let mut ntc = config.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(hw.ntc, Attenuation::_11dB);
    let mut ldr = config.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(hw.ldr, Attenuation::_11dB);
    let mut adc = Adc::new(hw.adc, config).into_async();
    let (mut ntc_avg, mut ldr_avg) = (Filter(None), Filter(None));
    let mut first = true;
    loop {
        let (mut ntc_sum, mut ldr_sum) = (0u32, 0u32);
        for _ in 0..SAMPLES {
            ntc_sum += adc.read_oneshot(&mut ntc).await as u32;
            Timer::after(SAMPLE_GAP).await;
            ldr_sum += adc.read_oneshot(&mut ldr).await as u32;
            Timer::after(SAMPLE_GAP).await;
        }
        let ntc_mv = ntc_avg.push(ntc_sum / SAMPLES);
        let ldr_mv = ldr_avg.push(ldr_sum / SAMPLES);
        NTC_MV.store(ntc_mv as u32, Ordering::Relaxed);
        LDR_MV.store(ldr_mv as u32, Ordering::Relaxed);
        if first {
            first = false;
            info!("sensors: {}", Report);
        }
        Timer::after(PERIOD).await;
    }
}

/// The readings as the log says them: the filtered millivolts of both
/// dividers, then the temperature in both units (offset applied, and
/// said so when it is not zero) and the light index, or why not.
pub struct Report;

impl core::fmt::Display for Report {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Some((ntc, ldr)) = readings() else { return f.write_str("not read yet") };
        write!(f, "ntc {ntc} mV, ldr {ldr} mV, ")?;
        let offset = settings::get().temp_offset_dc;
        match temperature_dc(ntc) {
            Some(dc) => {
                let dc = dc.saturating_add(offset);
                write!(f, "room {} C ({} F)", Tenths(dc), Tenths(fahrenheit_tenths(dc)))?;
                if offset != 0 {
                    write!(f, " with offset {}", Tenths(offset))?;
                }
            }
            None => f.write_str("no thermometer")?,
        }
        write!(f, ", light {} %", light_pct(ldr))
    }
}

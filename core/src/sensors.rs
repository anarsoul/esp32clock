// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The room sensors' arithmetic: millivolts from the two dividers on
//! the adapter board to a temperature and a light index. Pure code; the
//! ADC lives in the firmware's `sensors.rs`.
//!
//! Both dividers are a 10 kΩ pull-up from 3V3 on the adapter with the
//! clock board's part to ground: the NTC at R4 on GPIO0, the LDR at R3
//! on GPIO1. A pin reads high when the part is cold, dark or missing.
//!
//! The NTC is taken as 10 kΩ at 25 °C with B = 3950, what these kits
//! ship. `NTC_MV` is the divider's voltage at every 5 °C from −20 to
//! 80 °C by the B equation, computed on the host and pasted, and the
//! reading is interpolated between the two nearest entries. The error
//! of the straight segments and the rounding to tenths is under 0.2 °C
//! over the span, which the tests check against the equation in
//! floating point. The ADC is only
//! calibrated up to about 2.5 V at 11 dB attenuation and pins near
//! 3.1 V, so the cold end below 0 °C is rough and nothing below −20 °C
//! can be told from a missing sensor; a room is far from both.
//!
//! The light index is the logarithm of the LDR's resistance between
//! 1 kΩ and 50 kΩ; see `LIGHT_OHMS`.

/// The divider's millivolts at `NTC_TABLE_FIRST_C`, then every
/// `NTC_TABLE_STEP_C` degrees, descending as the NTC warms.
const NTC_MV: [u16; 21] = [
    3014, 2925, 2816, 2689, 2543, 2381, 2206, 2023, 1836, 1650, 1470, 1301, 1143, 1000, 871, 757, 657, 570, 494, 428,
    372,
];
const NTC_TABLE_FIRST_C: i16 = -20;
const NTC_TABLE_STEP_C: i16 = 5;

/// The pull-up and the supply of both dividers, for the resistance
/// behind a reading.
const PULL_UP_OHMS: u32 = 10_000;
const SUPPLY_MV: u32 = 3300;

/// The light index is the logarithm of the LDR's resistance, which is
/// how the eye and the part both work (a GL55-type LDR is about a
/// kilohm in daylight, tens of kilohms at dusk, megohms in the dark):
/// 100 at `LIGHT_FULL_OHMS` and below, 0 at `LIGHT_DARK_OHMS` and
/// above, linear in the logarithm between. The dark end is what the
/// ADC can still tell apart from the pull-up alone: the raw code pins
/// near 2.9 V, which is about 70 kΩ, and a missing LDR reads the same.
pub const LIGHT_FULL_OHMS: u32 = 1_000;
pub const LIGHT_DARK_OHMS: u32 = 50_000;

/// The resistance at each 5 % step of the index, from 100 % down to 0 %:
/// `LIGHT_FULL_OHMS × (LIGHT_DARK_OHMS / LIGHT_FULL_OHMS) ^ (k / 20)`,
/// computed on the host and pasted; the tests check it.
const LIGHT_OHMS: [u32; 21] = [
    1000, 1216, 1479, 1798, 2187, 2659, 3234, 3932, 4782, 5815, 7071, 8599, 10456, 12715, 15462, 18803, 22865, 27805,
    33812, 41117, 50000,
];

/// Tenths of a degree Celsius from the NTC divider's millivolts, or
/// `None` at or above the coldest table entry: no sensor fitted, or
/// colder than the ADC can tell. Hotter than the table's end is
/// reported as its end.
pub fn temperature_dc(mv: u16) -> Option<i16> {
    if mv >= NTC_MV[0] {
        return None;
    }
    let last = NTC_MV.len() - 1;
    if mv <= NTC_MV[last] {
        return Some((NTC_TABLE_FIRST_C + NTC_TABLE_STEP_C * last as i16) * 10);
    }
    // The first entry below the reading; `mv` is strictly between
    // `NTC_MV[i - 1]` and `NTC_MV[i]` or equal to the latter.
    let i = NTC_MV.iter().position(|&v| v <= mv).expect("the table ends below any reading here");
    let (hi, lo) = (NTC_MV[i - 1] as i32, NTC_MV[i] as i32);
    let start = (NTC_TABLE_FIRST_C + NTC_TABLE_STEP_C * (i as i16 - 1)) as i32 * 10;
    let span = NTC_TABLE_STEP_C as i32 * 10;
    // Rounded to the nearest tenth.
    let dc = start + (span * (hi - mv as i32) * 2 + (hi - lo)) / (2 * (hi - lo));
    Some(dc as i16)
}

/// Tenths of a degree Fahrenheit from tenths of a degree Celsius,
/// rounded to the nearest tenth.
pub fn fahrenheit_tenths(dc: i16) -> i16 {
    let f = dc as i32 * 9;
    let rounded = if f >= 0 { (f + 2) / 5 } else { (f - 2) / 5 };
    (rounded + 320) as i16
}

/// The LDR's resistance in ohms from its divider's millivolts; a
/// reading at or past the supply is taken as open.
pub fn ldr_ohms(mv: u16) -> u32 {
    let mv = mv as u32;
    if mv >= SUPPLY_MV {
        return u32::MAX;
    }
    PULL_UP_OHMS * mv / (SUPPLY_MV - mv)
}

/// The light index, 0 (dark) to 100, from the LDR divider's millivolts;
/// see `LIGHT_OHMS`.
pub fn light_pct(mv: u16) -> u8 {
    let ohms = ldr_ohms(mv);
    if ohms <= LIGHT_OHMS[0] {
        return 100;
    }
    let last = LIGHT_OHMS.len() - 1;
    if ohms >= LIGHT_OHMS[last] {
        return 0;
    }
    // The first entry above the resistance; `ohms` is strictly between
    // `LIGHT_OHMS[i - 1]` and `LIGHT_OHMS[i]` or equal to the former.
    let i = LIGHT_OHMS.iter().position(|&r| r > ohms).expect("the table ends above any resistance here");
    let (lo, hi) = (LIGHT_OHMS[i - 1], LIGHT_OHMS[i]);
    // 5 % per segment, rounded to the nearest percent.
    let top = 100 - 5 * (i as u32 - 1);
    (top - (5 * (ohms - lo) * 2 + (hi - lo)) / (2 * (hi - lo))) as u8
}

/// Tenths as a decimal with one place: `23.4`, `-0.5`.
pub struct Tenths(pub i16);

impl core::fmt::Display for Tenths {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        write!(f, "{sign}{}.{}", abs / 10, abs % 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: f64 = 3950.0;
    const R25: f64 = 10_000.0;
    const PULL_UP: f64 = 10_000.0;
    const SUPPLY_MV: f64 = 3300.0;

    fn equation_mv(celsius: f64) -> f64 {
        let r = R25 * (B * (1.0 / (celsius + 273.15) - 1.0 / 298.15)).exp();
        SUPPLY_MV * r / (PULL_UP + r)
    }

    fn equation_celsius(mv: f64) -> f64 {
        let r = PULL_UP * mv / (SUPPLY_MV - mv);
        B / ((r / R25).ln() + B / 298.15) - 273.15
    }

    #[test]
    fn table_is_the_equation() {
        for (i, &mv) in NTC_MV.iter().enumerate() {
            let c = (NTC_TABLE_FIRST_C + NTC_TABLE_STEP_C * i as i16) as f64;
            assert_eq!(mv as f64, equation_mv(c).round(), "{c} °C");
        }
        assert!(NTC_MV.windows(2).all(|w| w[0] > w[1]), "descending");
    }

    #[test]
    fn interpolation_is_within_a_tenth_or_so() {
        let mut worst: f64 = 0.0;
        for mv in NTC_MV[NTC_MV.len() - 1]..NTC_MV[0] {
            let dc = temperature_dc(mv).unwrap() as f64 / 10.0;
            worst = worst.max((dc - equation_celsius(mv as f64)).abs());
        }
        assert!(worst < 0.2, "worst error {worst} °C");
        assert_eq!(temperature_dc(1650), Some(250));
        assert_eq!(temperature_dc(2543), Some(0));
        assert_eq!(temperature_dc(1143), Some(400));
    }

    #[test]
    fn rails_and_ends() {
        assert_eq!(temperature_dc(NTC_MV[0]), None, "the pull-up alone: no sensor");
        assert_eq!(temperature_dc(3100), None);
        assert_eq!(temperature_dc(NTC_MV[0] - 1), Some(-199));
        assert_eq!(temperature_dc(NTC_MV[1]), Some(-150));
        assert_eq!(temperature_dc(372), Some(800));
        assert_eq!(temperature_dc(0), Some(800));
    }

    #[test]
    fn fahrenheit() {
        assert_eq!(fahrenheit_tenths(0), 320);
        assert_eq!(fahrenheit_tenths(1000), 2120);
        assert_eq!(fahrenheit_tenths(234), 741);
        assert_eq!(fahrenheit_tenths(-400), -400);
        assert_eq!(fahrenheit_tenths(-5), 311);
    }

    #[test]
    fn light_table_is_logarithmic() {
        let ratio = LIGHT_DARK_OHMS as f64 / LIGHT_FULL_OHMS as f64;
        for (k, &ohms) in LIGHT_OHMS.iter().enumerate() {
            let expected = LIGHT_FULL_OHMS as f64 * ratio.powf(k as f64 / 20.0);
            assert_eq!(ohms as f64, expected.round(), "step {k}");
        }
        assert!(LIGHT_OHMS.windows(2).all(|w| w[0] < w[1]), "ascending");
    }

    #[test]
    fn light_index() {
        let mv_of = |ohms: f64| (SUPPLY_MV * ohms / (PULL_UP + ohms)).round() as u16;
        assert_eq!(ldr_ohms(mv_of(10_000.0)), 10_000);
        assert_eq!(ldr_ohms(3300), u32::MAX);
        assert_eq!(light_pct(0), 100, "shorted");
        assert_eq!(light_pct(mv_of(1_000.0)), 100);
        assert_eq!(light_pct(mv_of(50_000.0)), 0);
        assert_eq!(light_pct(2894), 0, "the pull-up alone as the ADC pins");
        assert_eq!(light_pct(3300), 0);
        // Half the index is the geometric middle of the span.
        assert_eq!(light_pct(mv_of(7_071.0)), 50);
        // Within a percent or two of the logarithm anywhere between.
        for ohms in [1_500.0, 2_000.0, 4_000.0, 10_000.0, 14_600.0, 25_600.0, 40_000.0] {
            let exact = 100.0 * (1.0 - (ohms / LIGHT_FULL_OHMS as f64).log10() / (LIGHT_DARK_OHMS as f64 / LIGHT_FULL_OHMS as f64).log10());
            let got = light_pct(mv_of(ohms)) as f64;
            assert!((got - exact).abs() <= 1.5, "{ohms} Ω: {got} vs {exact}");
        }
    }

    #[test]
    fn tenths_format() {
        assert_eq!(format!("{}", Tenths(234)), "23.4");
        assert_eq!(format!("{}", Tenths(0)), "0.0");
        assert_eq!(format!("{}", Tenths(-5)), "-0.5");
        assert_eq!(format!("{}", Tenths(-200)), "-20.0");
    }
}

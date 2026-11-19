// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! What a clip's header tells the player's shaper
//! (`onebitdawn_core::shape`): the high-pass's section and the rate.

/// The high-pass as a second-order Butterworth section at the output
/// rate, in Q14: b0 b1 b2 a1 a2.
pub fn section(highpass: f64, rate: u32) -> Result<[i16; 5], String> {
    let w = 2.0 * core::f64::consts::PI * highpass / rate as f64;
    let alpha = w.sin() / 2f64.sqrt();
    let c = w.cos();
    let section = [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0, -2.0 * c, 1.0 - alpha];
    let mut out = [0i16; 5];
    for (o, k) in out.iter_mut().zip(section) {
        let q = (k / (1.0 + alpha) * 16384.0).round_ties_even();
        if !(-32768.0..=32767.0).contains(&q) {
            return Err(format!("highpass={highpass}: no such corner at {rate} samples a second"));
        }
        *o = q as i16;
    }
    Ok(out)
}

/// The rate's bits in a clip's flags: 8000, 16000 or `clip::FAST_HZ`.
pub fn rate_flag(rate: u32) -> Result<u8, String> {
    match rate {
        8000 => Ok(0),
        16000 => Ok(1),
        31250 => Ok(4),
        _ => Err(format!("rate={rate}: 8000, 16000 or 31250")),
    }
}
